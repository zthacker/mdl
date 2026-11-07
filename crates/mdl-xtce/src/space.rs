//! The SpaceSystem tree, what each SpaceSystem names, and XTCE name
//! references.
//!
//! References resolve as Yamcs resolves them, so documents written for it
//! import as they load there: a reference starting with `/` from the root
//! SpaceSystem, one starting with `./` or `..` from the referring
//! SpaceSystem, and any other in the referring SpaceSystem and then each
//! SpaceSystem above it in turn. A parameter reference may go on into an
//! aggregate's members, as XTCE writes it (`Packet_ID.APID`) or as Yamcs
//! does (`Packet_ID/APID`).

use std::collections::BTreeMap;

use crate::Code;
use crate::report::Report;
use crate::xml::{El, Expect};

/// Kinds of item, each with its own names in a SpaceSystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    ParameterType,
    Parameter,
    Container,
}

impl Kind {
    fn noun(self) -> &'static str {
        match self {
            Kind::ParameterType => "parameter type",
            Kind::Parameter => "parameter",
            Kind::Container => "container",
        }
    }
}

pub(crate) struct Space<'d> {
    pub(crate) el: El<'d>,
    pub(crate) name: &'d str,
    pub(crate) parent: Option<usize>,
    children: BTreeMap<&'d str, usize>,
    types: BTreeMap<&'d str, El<'d>>,
    params: BTreeMap<&'d str, El<'d>>,
    containers: BTreeMap<&'d str, El<'d>>,
}

impl<'d> Space<'d> {
    fn items(&self, kind: Kind) -> &BTreeMap<&'d str, El<'d>> {
        match kind {
            Kind::ParameterType => &self.types,
            Kind::Parameter => &self.params,
            Kind::Container => &self.containers,
        }
    }

    fn items_mut(&mut self, kind: Kind) -> &mut BTreeMap<&'d str, El<'d>> {
        match kind {
            Kind::ParameterType => &mut self.types,
            Kind::Parameter => &mut self.params,
            Kind::Container => &mut self.containers,
        }
    }
}

/// An item a reference names.
#[derive(Clone)]
pub(crate) struct Found<'d> {
    pub(crate) space: usize,
    pub(crate) el: El<'d>,
    /// For a parameter: the aggregate members the reference goes into.
    pub(crate) members: Vec<&'d str>,
}

pub(crate) struct Tree<'d> {
    pub(crate) spaces: Vec<Space<'d>>,
    roots: BTreeMap<&'d str, usize>,
    /// Each item, in document order, with its SpaceSystem.
    order: Vec<(Kind, usize, El<'d>)>,
}

/// What a SpaceSystem holds that the importer reads or reports.
const SPACE_SYSTEM: Expect<'static> = Expect {
    attrs: &["name"],
    children: &["TelemetryMetaData", "CommandMetaData", "SpaceSystem"],
    ignored: &[
        "shortDescription",
        "LongDescription",
        "AliasSet",
        "AncillaryDataSet",
        "Header",
        "ServiceSet",
        "operationalStatus",
    ],
    text: false,
};

const TELEMETRY: Expect<'static> = Expect {
    attrs: &[],
    children: &["ParameterTypeSet", "ParameterSet", "ContainerSet"],
    ignored: &["AlgorithmSet", "StreamSet"],
    text: false,
};

/// Commands come with the importer's next version; their parameters and
/// types are read already, since telemetry may use them.
const COMMANDS: Expect<'static> = Expect {
    attrs: &[],
    children: &["ParameterTypeSet", "ParameterSet"],
    ignored: &[
        "ArgumentTypeSet",
        "MetaCommandSet",
        "CommandContainerSet",
        "AlgorithmSet",
        "StreamSet",
    ],
    text: false,
};

/// The deepest SpaceSystem read, the root being 1: far beyond any real
/// document, and a bound on the reader's recursion.
pub(crate) const MAX_SPACE_DEPTH: usize = 64;

const PARAMETER_TYPES: &[&str] = &[
    "StringParameterType",
    "EnumeratedParameterType",
    "IntegerParameterType",
    "BinaryParameterType",
    "FloatParameterType",
    "BooleanParameterType",
    "RelativeTimeParameterType",
    "AbsoluteTimeParameterType",
    "ArrayParameterType",
    "AggregateParameterType",
];

impl<'d> Tree<'d> {
    /// The tree of every file's root SpaceSystem, with every name each
    /// SpaceSystem defines.
    pub(crate) fn build(roots: &[El<'d>], report: &mut Report) -> Tree<'d> {
        let mut tree = Tree {
            spaces: Vec::new(),
            roots: BTreeMap::new(),
            order: Vec::new(),
        };
        for root in roots {
            let Some(index) = tree.add(*root, None, 1, report) else {
                continue;
            };
            let name = tree.spaces[index].name;
            if let Some(&first) = tree.roots.get(name) {
                report.duplicate(
                    Code::Duplicate,
                    root.span(),
                    tree.spaces[first].el.span(),
                    format!("two files have a root SpaceSystem named {name}"),
                );
            } else {
                tree.roots.insert(name, index);
            }
        }
        tree
    }

    fn add(
        &mut self,
        el: El<'d>,
        parent: Option<usize>,
        depth: usize,
        report: &mut Report,
    ) -> Option<usize> {
        el.check(&SPACE_SYSTEM, report);
        let name = el.required("name", report)?.value;
        let index = self.spaces.len();
        self.spaces.push(Space {
            el,
            name,
            parent,
            children: BTreeMap::new(),
            types: BTreeMap::new(),
            params: BTreeMap::new(),
            containers: BTreeMap::new(),
        });
        for child in el.children() {
            match child.name() {
                "TelemetryMetaData" => {
                    child.check(&TELEMETRY, report);
                    self.add_sets(index, child, true, report);
                }
                "CommandMetaData" => {
                    child.check(&COMMANDS, report);
                    self.add_sets(index, child, false, report);
                }
                "SpaceSystem" if depth >= MAX_SPACE_DEPTH => report.error(
                    Code::Unsupported,
                    child.span(),
                    format!(
                        "SpaceSystems nested more than {MAX_SPACE_DEPTH} deep aren't supported"
                    ),
                ),
                "SpaceSystem" => {
                    let Some(sub) = self.add(child, Some(index), depth + 1, report) else {
                        continue;
                    };
                    let sub_name = self.spaces[sub].name;
                    match self.spaces[index].children.get(sub_name) {
                        Some(&first) => report.duplicate(
                            Code::Duplicate,
                            child.span(),
                            self.spaces[first].el.span(),
                            format!("{} has two SpaceSystems named {sub_name}", self.path(index)),
                        ),
                        None => {
                            self.spaces[index].children.insert(sub_name, sub);
                        }
                    }
                }
                _ => {}
            }
        }
        Some(index)
    }

    /// The parameter types, parameters and (for telemetry) containers of a
    /// TelemetryMetaData or CommandMetaData.
    fn add_sets(&mut self, space: usize, meta: El<'d>, telemetry: bool, report: &mut Report) {
        for set in meta.children() {
            match set.name() {
                "ParameterTypeSet" => {
                    set.check(
                        &Expect {
                            attrs: &[],
                            children: PARAMETER_TYPES,
                            ignored: &[],
                            text: false,
                        },
                        report,
                    );
                    for item in set.children() {
                        if PARAMETER_TYPES.contains(&item.name()) {
                            self.name(space, Kind::ParameterType, item, report);
                        }
                    }
                }
                "ParameterSet" => {
                    set.check(
                        &Expect {
                            attrs: &[],
                            children: &["Parameter"],
                            ignored: &[],
                            text: false,
                        },
                        report,
                    );
                    for item in set.all("Parameter") {
                        self.name(space, Kind::Parameter, item, report);
                    }
                }
                "ContainerSet" if telemetry => {
                    set.check(
                        &Expect {
                            attrs: &[],
                            children: &["SequenceContainer"],
                            ignored: &[],
                            text: false,
                        },
                        report,
                    );
                    for item in set.all("SequenceContainer") {
                        self.name(space, Kind::Container, item, report);
                    }
                }
                _ => {}
            }
        }
    }

    fn name(&mut self, space: usize, kind: Kind, el: El<'d>, report: &mut Report) {
        let Some(name) = el.required("name", report).map(|a| a.value) else {
            return;
        };
        let path = self.path(space);
        match self.spaces[space].items(kind).get(name) {
            Some(first) => report.duplicate(
                Code::Duplicate,
                el.span(),
                first.span(),
                format!("{path} has two {}s named {name}", kind.noun()),
            ),
            None => {
                self.spaces[space].items_mut(kind).insert(name, el);
                self.order.push((kind, space, el));
            }
        }
    }

    /// Every item of a kind, in document order, with its SpaceSystem.
    pub(crate) fn items(&self, kind: Kind) -> impl Iterator<Item = (usize, El<'d>)> + '_ {
        self.order
            .iter()
            .filter(move |(k, _, _)| *k == kind)
            .map(|(_, space, el)| (*space, *el))
    }

    /// A SpaceSystem's path, `/Root/Sub`.
    pub(crate) fn path(&self, space: usize) -> String {
        self.names(space)
            .iter()
            .map(|name| format!("/{name}"))
            .collect()
    }

    /// The names of a SpaceSystem and those above it, from the root down.
    pub(crate) fn names(&self, space: usize) -> Vec<&'d str> {
        let mut names = Vec::new();
        let mut at = Some(space);
        while let Some(index) = at {
            names.push(self.spaces[index].name);
            at = self.spaces[index].parent;
        }
        names.reverse();
        names
    }

    /// The item a reference from SpaceSystem `from` names, or a message
    /// saying why there's none.
    pub(crate) fn resolve(
        &self,
        from: usize,
        reference: &'d str,
        kind: Kind,
    ) -> Result<Found<'d>, String> {
        let not_found = || {
            format!(
                "{reference} names no {} that {} can see",
                kind.noun(),
                self.path(from)
            )
        };
        let segments: Vec<&'d str> = reference.split('/').filter(|s| !s.is_empty()).collect();
        if segments.is_empty() {
            return Err(format!("{reference:?} isn't a reference"));
        }
        if reference.starts_with('/') {
            let (root, rest) = segments.split_first().ok_or_else(not_found)?;
            let start = *self.roots.get(root).ok_or_else(not_found)?;
            return self.walk(start, rest, kind).ok_or_else(not_found);
        }
        if matches!(segments[0], "." | "..") {
            return self.walk(from, &segments, kind).ok_or_else(not_found);
        }
        let mut at = Some(from);
        while let Some(start) = at {
            if let Some(found) = self.walk(start, &segments, kind) {
                return Ok(found);
            }
            at = self.spaces[start].parent;
        }
        Err(not_found())
    }

    fn walk(&self, start: usize, segments: &[&'d str], kind: Kind) -> Option<Found<'d>> {
        let (last, path) = segments.split_last()?;
        let mut space = start;
        for (index, segment) in path.iter().enumerate() {
            match *segment {
                "." => {}
                ".." => space = self.spaces[space].parent?,
                name => match self.spaces[space].children.get(name) {
                    Some(&child) => space = child,
                    // Yamcs's spelling of an aggregate member: the rest of
                    // the path is members.
                    None if kind == Kind::Parameter => {
                        let el = *self.spaces[space].params.get(name)?;
                        let members = segments[index + 1..].to_vec();
                        return Some(Found { space, el, members });
                    }
                    None => return None,
                },
            }
        }
        let (name, members) = match kind {
            Kind::Parameter => {
                let mut parts = last.split('.');
                let name = parts.next()?;
                (name, parts.collect())
            }
            _ => (*last, Vec::new()),
        };
        let el = *self.spaces[space].items(kind).get(name)?;
        Some(Found { space, el, members })
    }
}
