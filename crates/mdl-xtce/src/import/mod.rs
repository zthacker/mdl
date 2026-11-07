//! Sequence containers as packets, the parameters in them as fields and
//! catalog params, and the bundle around them.
//!
//! A concrete container is a packet: its entries are those of every
//! container it inherits from, root first, then its own, all placed from
//! the start of the packet, as XTCE places them. The restriction criteria
//! on each BaseContainer along the way become the packet's identify keys.
//! A container's entries and criteria mean the same in every packet built
//! on it (criteria test only the entries above them, as XTCE evaluates
//! them), so each is worked out once.
//!
//! A container embedded with a ContainerRefEntry is a struct type, and so
//! are the elements of an array or a repeated parameter that are
//! aggregates. Struct types are named once every packet is known.

use std::collections::{BTreeMap, BTreeSet};

use mdl::v2::ir::Locator;
use mdl::v2::source::{SourceId, SourceMap, Span};
use serde_json::Value;

use crate::names::{catalog_part, identifier, qualified, token};
use crate::out::Object;
use crate::report::Report;
use crate::space::{Kind, Tree};
use crate::types::{Order, Raw, Scalar, Shape, Types, description};
use crate::xml::{self, El, Expect};
use crate::{Code, Naming, Options};

mod conditions;
mod emit;
mod entries;

/// A parameter, or a member of an aggregate one: its SpaceSystem, its
/// name, and the path of members.
type ParamKey<'d> = (usize, &'d str, Vec<&'d str>);

/// An element's identity, across files.
type ElId = (SourceId, usize);

/// A container's fields so far, by the parameter or member each reads.
type Fields<'d> = Vec<(ParamKey<'d>, Known)>;

fn id(el: &El<'_>) -> ElId {
    (el.file.id(), el.node.range().start)
}

struct Param<'d> {
    key: ParamKey<'d>,
    el: El<'d>,
    scalar: Scalar<'d>,
    description: Option<(String, Span)>,
}

/// A field, as conditions, identify keys, counts and lengths see it.
#[derive(Clone)]
struct Known {
    name: String,
    ty: &'static str,
    raw: Raw,
}

#[derive(Clone)]
enum Entry<'d> {
    Field(Box<Field<'d>>),
    Array(Box<ArrayField<'d>>),
    Struct(Box<StructField<'d>>),
    Padding {
        bits: u32,
        span: Span,
        when: Option<Object>,
    },
}

impl Entry<'_> {
    /// The XTCE entry it comes from, for an entry that one does.
    fn span(&self) -> Option<Span> {
        match self {
            Entry::Field(field) => Some(field.span),
            Entry::Array(array) => Some(array.span),
            Entry::Struct(inner) => Some(inner.span),
            Entry::Padding { .. } => None,
        }
    }

    fn name(&self) -> Option<&str> {
        match self {
            Entry::Field(field) => Some(&field.name),
            Entry::Array(array) => Some(&array.name),
            Entry::Struct(inner) => Some(&inner.name),
            Entry::Padding { .. } => None,
        }
    }

    fn offset(&self) -> Option<(u32, Span)> {
        match self {
            Entry::Field(field) => field.offset,
            Entry::Array(array) => array.offset,
            Entry::Struct(inner) => inner.offset,
            Entry::Padding { .. } => None,
        }
    }

    fn conditional(&self) -> bool {
        match self {
            Entry::Field(field) => field.when.is_some(),
            Entry::Array(array) => array.when.is_some(),
            Entry::Struct(inner) => inner.when.is_some(),
            Entry::Padding { when, .. } => when.is_some(),
        }
    }
}

#[derive(Clone)]
struct Field<'d> {
    name: String,
    /// The parameter or member it reads; `None` for a size tag the
    /// importer adds before a leading-size string.
    key: Option<ParamKey<'d>>,
    span: Span,
    scalar: Scalar<'d>,
    /// The length, written out, for a string or bytes field.
    length: Option<(Value, Span)>,
    canonical: Option<(String, Span)>,
    offset: Option<(u32, Span)>,
    when: Option<Object>,
}

#[derive(Clone)]
struct ArrayField<'d> {
    name: String,
    key: Option<ParamKey<'d>>,
    span: Span,
    element: Element<'d>,
    count: (Value, Span),
    canonical: Option<(String, Span)>,
    offset: Option<(u32, Span)>,
    when: Option<Object>,
}

#[derive(Clone)]
enum Element<'d> {
    Scalar(Box<Scalar<'d>>),
    Struct(TypeId<'d>),
}

#[derive(Clone)]
struct StructField<'d> {
    name: String,
    ty: TypeId<'d>,
    span: Span,
    offset: Option<(u32, Span)>,
    when: Option<Object>,
}

/// Where a struct type comes from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum TypeId<'d> {
    /// A container embedded with a ContainerRefEntry.
    Container(ElId),
    /// The elements of an array or repeated parameter (or member), and the
    /// bits of padding after each.
    Element(ParamKey<'d>, u32),
}

struct StructType<'d> {
    /// Its MDL name, worked out once every type is known.
    name: String,
    /// The name it's given before any qualifying: the container's, or the
    /// field's with `_element`.
    base: String,
    space: usize,
    span: Span,
    entries: Vec<Entry<'d>>,
}

/// What one container adds to the packets built on it.
#[derive(Clone)]
struct Part<'d> {
    entries: Vec<Entry<'d>>,
    fields: Fields<'d>,
    /// What its BaseContainer's restriction criteria require.
    identify: Vec<(String, i64, Span)>,
}

struct Packet<'d> {
    /// Its MDL name, worked out once every packet is known.
    name: String,
    space: usize,
    el: El<'d>,
    entries: Vec<Entry<'d>>,
    identify: BTreeMap<String, (i64, Span)>,
}

const CONTAINER: Expect<'static> = Expect {
    attrs: &["name", "abstract"],
    children: &["EntryList", "BaseContainer", "BinaryEncoding"],
    ignored: &[
        "shortDescription",
        "LongDescription",
        "AliasSet",
        "AncillaryDataSet",
        "DefaultRateInStream",
        "RateInStreamSet",
        "idlePattern",
    ],
    text: false,
};

pub(crate) struct Import<'d, 'r> {
    tree: &'r Tree<'d>,
    report: &'r mut Report,
    naming: Naming,
    types: Types<'d>,
    /// Imported params, by catalog name.
    params: BTreeMap<String, Param<'d>>,
    /// The catalog name of each parameter or member imported.
    names: BTreeMap<ParamKey<'d>, (String, Span)>,
    /// Each parameter's shape, once worked out.
    shapes: BTreeMap<ElId, Option<Shape<'d>>>,
    /// Parameters some entry or comparison names.
    seen: BTreeSet<(usize, &'d str)>,
    parts: BTreeMap<ElId, Option<Part<'d>>>,
    /// Struct types, by where each comes from.
    structs: BTreeMap<TypeId<'d>, StructType<'d>>,
    /// Embedded containers that couldn't be imported.
    broken: BTreeSet<ElId>,
    /// Embedded containers being worked out, outermost first.
    embedding: Vec<ElId>,
    /// Each parameter's field name, before members, and whether another
    /// parameter shares its name.
    field_names: BTreeMap<(usize, &'d str), (String, bool)>,
}

impl<'d, 'r> Import<'d, 'r> {
    pub(crate) fn new(tree: &'r Tree<'d>, report: &'r mut Report, options: &Options) -> Self {
        Import {
            tree,
            report,
            naming: options.naming,
            types: Types::default(),
            params: BTreeMap::new(),
            names: BTreeMap::new(),
            shapes: BTreeMap::new(),
            seen: BTreeSet::new(),
            parts: BTreeMap::new(),
            structs: BTreeMap::new(),
            broken: BTreeSet::new(),
            embedding: Vec::new(),
            field_names: field_names(tree, options.naming),
        }
    }

    /// The bundle, as JSON with its source map. `None` when there's nothing
    /// to build it from; every problem is reported.
    pub(crate) fn bundle(mut self, options: &Options) -> Option<(Value, SourceMap)> {
        let tree = self.tree;
        let embedded = self.embedded();
        let mut packets = Vec::new();
        for (space, el) in tree.items(Kind::Container) {
            let concrete = match el.attr("abstract") {
                Some(attr) => !xml::boolean(attr, self.report).unwrap_or(true),
                None => true,
            };
            if !concrete {
                continue;
            }
            if let Some((embedder_space, embedder)) = embedded.get(&id(&el)) {
                self.report.warning(
                    Code::Ignored,
                    el.span(),
                    format!(
                        "container {}/{} isn't a packet: {}/{embedder} embeds it, so it's a \
                         structure; mark it abstract to say so",
                        tree.path(space),
                        el.item_name().unwrap_or_default(),
                        tree.path(*embedder_space)
                    ),
                );
                continue;
            }
            if let Some(packet) = self.packet(space, el) {
                packets.push(packet);
            }
        }
        self.name_packets(&mut packets);
        self.name_types();
        self.report_unused();
        let root = tree.spaces.first()?;
        let root_span = root.el.span();
        let family = match &options.family {
            Some(name) => name.clone(),
            None => match token(root.name) {
                Some(name) => name,
                None => {
                    self.report.error(
                        Code::Name,
                        root_span,
                        format!(
                            "no family name can be made from SpaceSystem {}; name the family in \
                             the import options",
                            root.name
                        ),
                    );
                    return None;
                }
            },
        };
        let set = options.set.clone().unwrap_or_else(|| family.clone());
        let alias = options
            .alias
            .clone()
            .unwrap_or_else(|| Locator::fixed(root.name));
        let alias = match serde_json::to_value(alias) {
            Ok(alias) => alias,
            Err(err) => {
                self.report.error(
                    Code::Invalid,
                    root_span,
                    format!("the alias option can't be written as JSON: {err}"),
                );
                return None;
            }
        };
        let order = self.family_order(&packets);
        Some(emit::bundle(emit::Bundle {
            root: root_span,
            params: &self.params,
            family: &family,
            order,
            alias,
            set: &set,
            packets: &packets,
            structs: &self.structs,
        }))
    }

    /// The containers a ContainerRefEntry embeds somewhere. They're
    /// structures, not packets, even when they aren't abstract, as the
    /// CCSDS green book's secondary header isn't. A reference that names
    /// nothing is reported where its container is imported.
    ///
    /// Each is mapped to one container that embeds it, for the warning a
    /// container that isn't abstract gets.
    fn embedded(&self) -> BTreeMap<ElId, (usize, &'d str)> {
        let mut embedded = BTreeMap::new();
        for (space, el) in self.tree.items(Kind::Container) {
            let Some(list) = el.child("EntryList") else {
                continue;
            };
            let embedder = (space, el.item_name().unwrap_or_default());
            for entry in list.all("ContainerRefEntry") {
                if let Some(reference) = entry.attr("containerRef")
                    && let Ok(found) = self.tree.resolve(space, reference.value, Kind::Container)
                {
                    embedded.entry(id(&found.el)).or_insert(embedder);
                }
            }
        }
        embedded
    }

    /// A concrete container as a packet.
    fn packet(&mut self, space: usize, el: El<'d>) -> Option<Packet<'d>> {
        let chain = self.chain(space, el)?;
        let mut entries = Vec::new();
        let mut fields: Fields<'d> = Vec::new();
        let mut identify: BTreeMap<String, (i64, Span)> = BTreeMap::new();
        let mut complete = true;
        for (container_space, container) in chain {
            let Some(part) = self.part(container_space, container, &fields) else {
                complete = false;
                continue;
            };
            for (key, value, span) in part.identify {
                match identify.get(&key) {
                    Some((earlier, _)) if *earlier != value => {
                        self.report.error(
                            Code::Invalid,
                            span,
                            format!(
                                "this requires {key} to be {value}, but a container above \
                                 requires {earlier}, so no packet matches"
                            ),
                        );
                        complete = false;
                    }
                    _ => {
                        identify.insert(key, (value, span));
                    }
                }
            }
            entries.extend(part.entries);
            fields.extend(part.fields);
        }
        if !complete {
            return None;
        }
        self.check_absent_locations(&entries)?;
        self.check_entries(&entries, "packet")?;
        Some(Packet {
            name: el.item_name()?.to_string(),
            space,
            el,
            entries,
            identify,
        })
    }

    /// The container and those it inherits from, root first.
    fn chain(&mut self, space: usize, el: El<'d>) -> Option<Vec<(usize, El<'d>)>> {
        let mut chain = vec![(space, el)];
        let mut seen = BTreeSet::from([id(&el)]);
        let (mut at_space, mut at) = (space, el);
        while let Some(base) = at.child("BaseContainer") {
            let reference = base.required("containerRef", self.report)?;
            let found = match self
                .tree
                .resolve(at_space, reference.value, Kind::Container)
            {
                Ok(found) => found,
                Err(message) => {
                    self.report.error(Code::Reference, reference.span, message);
                    return None;
                }
            };
            if !seen.insert(id(&found.el)) {
                self.report.error(
                    Code::Invalid,
                    reference.span,
                    format!(
                        "container {} inherits from itself",
                        found.el.item_name().unwrap_or_default()
                    ),
                );
                return None;
            }
            chain.push((found.space, found.el));
            (at_space, at) = (found.space, found.el);
        }
        chain.reverse();
        Some(chain)
    }

    /// What a container adds, given the fields of the containers above it.
    fn part(
        &mut self,
        space: usize,
        el: El<'d>,
        above: &[(ParamKey<'d>, Known)],
    ) -> Option<Part<'d>> {
        if let Some(part) = self.parts.get(&id(&el)) {
            return part.clone();
        }
        let part = self.convert_part(space, el, above);
        self.parts.insert(id(&el), part.clone());
        part
    }

    fn convert_part(
        &mut self,
        space: usize,
        el: El<'d>,
        above: &[(ParamKey<'d>, Known)],
    ) -> Option<Part<'d>> {
        el.check(&CONTAINER, self.report);
        if let Some(size) = el.child("BinaryEncoding") {
            self.report.error(
                Code::Unsupported,
                size.span(),
                "a container's own size (BinaryEncoding) isn't supported yet",
            );
            return None;
        }
        let identify = match el.child("BaseContainer") {
            Some(base) => self.restriction(space, base, above)?,
            None => Vec::new(),
        };
        let Some(list) = el.child("EntryList") else {
            self.report.error(
                Code::Invalid,
                el.span(),
                "a SequenceContainer has an EntryList",
            );
            return None;
        };
        let (entries, fields) = self.entries(space, list, above.to_vec())?;
        Some(Part {
            entries,
            fields,
            identify,
        })
    }

    /// Within one MDL container (a packet or a struct type): each
    /// parameter or member mapped at most once, counting the struct
    /// entries' types as MDL does, and no two entries with one name.
    fn check_entries(&mut self, entries: &[Entry<'d>], container: &str) -> Option<()> {
        let mut by_key: BTreeMap<ParamKey<'d>, Span> = BTreeMap::new();
        let mut by_name: BTreeMap<String, Span> = BTreeMap::new();
        let mut complete = true;
        for entry in entries {
            let Some(span) = entry.span() else {
                continue;
            };
            let mut twice = false;
            for key in self.mapped_keys(entry) {
                match by_key.get(&key) {
                    Some(first) if *first == span => {}
                    Some(first) => {
                        let message = format!(
                            "{} is in this {container} twice, and MDL maps a param from one \
                             entry per packet",
                            self.describe(&key)
                        );
                        self.report
                            .duplicate(Code::Unsupported, span, *first, message);
                        twice = true;
                    }
                    None => {
                        by_key.insert(key, span);
                    }
                }
            }
            // A parameter in twice has its name twice too; one report does.
            if twice {
                complete = false;
                continue;
            }
            if let Some(name) = entry.name() {
                if let Some(first) = by_name.get(name) {
                    self.report.duplicate(
                        Code::Name,
                        span,
                        *first,
                        format!("two entries of this {container} are named {name}"),
                    );
                    complete = false;
                } else {
                    by_name.insert(name.to_string(), span);
                }
            }
        }
        complete.then_some(())
    }

    /// The parameters an entry maps, as MDL counts them for one packet: a
    /// field's, a scalar array's, and those of a struct entry's type. An
    /// array of structs maps nothing itself; its type's fields are
    /// counted in the type.
    fn mapped_keys(&self, entry: &Entry<'d>) -> Vec<ParamKey<'d>> {
        match entry {
            Entry::Field(field) => field.key.iter().cloned().collect(),
            Entry::Array(array) => match array.element {
                Element::Scalar(_) => array.key.iter().cloned().collect(),
                Element::Struct(_) => Vec::new(),
            },
            Entry::Struct(inner) => self
                .structs
                .get(&inner.ty)
                .map(|ty| {
                    ty.entries
                        .iter()
                        .flat_map(|e| self.mapped_keys(e))
                        .collect()
                })
                .unwrap_or_default(),
            Entry::Padding { .. } => Vec::new(),
        }
    }

    /// XTCE doesn't place an absent entry, so the next one follows the
    /// entry before it; MDL starts the next one where the absent entry
    /// would have started. The two differ only for an absent entry at a
    /// fixed location followed by one placed after it. The entries one
    /// XTCE entry becomes (an aggregate's members) come and go together.
    fn check_absent_locations(&mut self, entries: &[Entry<'d>]) -> Option<()> {
        // The XTCE entry of a conditional entry at a fixed location, while
        // its entries go on.
        let mut placed: Option<(Span, &str, Span)> = None;
        for entry in entries {
            let span = entry.span();
            if let Some((first, name, at)) = placed {
                if span == Some(first) {
                    continue;
                }
                if entry.offset().is_none() {
                    self.report.error(
                        Code::Unsupported,
                        at,
                        format!(
                            "{name} is included on a condition at a fixed location, and the next \
                             entry follows it: when {name} is absent, XTCE places the next entry \
                             after the one before {name}, and MDL at {name}'s location; give the \
                             next entry a fixed location too"
                        ),
                    );
                    return None;
                }
            }
            placed = match (span, entry.name(), entry.offset()) {
                (Some(span), Some(name), Some((_, at))) if entry.conditional() => {
                    Some((span, name, at))
                }
                _ => None,
            };
        }
        Some(())
    }

    fn parameter_shape(&mut self, space: usize, el: El<'d>) -> Option<Shape<'d>> {
        if let Some(shape) = self.shapes.get(&id(&el)) {
            return shape.clone();
        }
        el.check(
            &Expect {
                attrs: &["name", "parameterTypeRef", "shortDescription"],
                children: &["AliasSet", "LongDescription", "ParameterProperties"],
                ignored: &["initialValue", "AncillaryDataSet"],
                text: false,
            },
            self.report,
        );
        if let Some(properties) = el.child("ParameterProperties") {
            properties.check(
                &Expect {
                    attrs: &[],
                    children: &["TimeAssociation"],
                    ignored: &[
                        "dataSource",
                        "readOnly",
                        "persistence",
                        "SystemName",
                        "PhysicalAddressSet",
                        "ValidityCondition",
                    ],
                    text: false,
                },
                self.report,
            );
            if conditions::time_association(properties, self.report).is_none() {
                self.shapes.insert(id(&el), None);
                return None;
            }
        }
        let shape = match el.required("parameterTypeRef", self.report) {
            Some(type_ref) => match self
                .tree
                .resolve(space, type_ref.value, Kind::ParameterType)
            {
                Ok(found) => self
                    .types
                    .shape(self.tree, found.space, found.el, self.report),
                Err(message) => {
                    self.report.error(Code::Reference, type_ref.span, message);
                    None
                }
            },
            None => None,
        };
        self.shapes.insert(id(&el), shape.clone());
        shape
    }

    /// The field name for a parameter or member: the parameter's, members
    /// joined by `_`, made an MDL identifier.
    fn field_name(&mut self, key: &ParamKey<'d>, el: El<'d>) -> String {
        let (base, shared) = self
            .field_names
            .get(&(key.0, key.1))
            .cloned()
            .unwrap_or_else(|| (identifier(key.1).0, false));
        let mut xtce = key.1.to_string();
        let mut name = base;
        for member in &key.2 {
            xtce.push('_');
            xtce.push_str(member);
            name.push('_');
            name.push_str(&identifier(member).0);
        }
        self.renamed(&Renaming {
            span: el.span(),
            space: key.0,
            what: ("parameter", "field"),
            xtce: &xtce,
            name: &name,
            shared,
        });
        name
    }

    /// Reports a name the importer changed: one shared with another item,
    /// or one that isn't an MDL identifier. Names qualified because the
    /// options ask for it aren't reported.
    fn renamed(&mut self, renaming: &Renaming<'_>) {
        let Renaming {
            span,
            space,
            what: (item, becomes),
            xtce,
            name,
            shared,
        } = *renaming;
        let why = if shared {
            format!("another {item} is also named {xtce}")
        } else if identifier(xtce).1 {
            "MDL names are letters, digits and '_'".to_string()
        } else {
            return;
        };
        if name != xtce {
            self.report.warning(
                Code::Renamed,
                span,
                format!(
                    "{item} {}/{xtce} is {becomes} {name}: {why}",
                    self.tree.path(space)
                ),
            );
        }
    }

    /// The catalog name of a parameter or member, registering its param
    /// the first time.
    fn register(
        &mut self,
        key: &ParamKey<'d>,
        el: El<'d>,
        scalar: &Scalar<'d>,
        member_description: Option<(String, Span)>,
    ) -> Option<(String, Span)> {
        if let Some(name) = self.names.get(key) {
            return Some(name.clone());
        }
        let (mut name, span) = self.base_name(key.0, el)?;
        for member in &key.2 {
            let Some(part) = catalog_part(member) else {
                self.report.error(
                    Code::Name,
                    el.span(),
                    format!("member {member} has no letters or digits for a catalog name"),
                );
                return None;
            };
            name.push('.');
            name.push_str(&part);
        }
        if let Some(first) = self.params.get(&name) {
            let first_span = first.el.span();
            let other = self.describe(&first.key);
            let this = self.describe(key);
            self.report.duplicate(
                Code::Name,
                el.span(),
                first_span,
                format!("{this} and {other} both have catalog name {name}"),
            );
            return None;
        }
        let description = if key.2.is_empty() {
            description(el)
        } else {
            member_description
        };
        self.params.insert(
            name.clone(),
            Param {
                key: key.clone(),
                el,
                scalar: scalar.clone(),
                description,
            },
        );
        self.names.insert(key.clone(), (name.clone(), span));
        Some((name, span))
    }

    /// A parameter's catalog name: its alias in the MDL namespace, else its
    /// SpaceSystem path and name.
    fn base_name(&mut self, space: usize, el: El<'d>) -> Option<(String, Span)> {
        if let Some(set) = el.child("AliasSet") {
            set.check(
                &Expect {
                    attrs: &[],
                    children: &["Alias"],
                    ignored: &[],
                    text: false,
                },
                self.report,
            );
            let mut mdl = Vec::new();
            let mut others = Vec::new();
            for alias in set.all("Alias") {
                let (Some(namespace), Some(value)) = (
                    alias.required("nameSpace", self.report),
                    alias.required("alias", self.report),
                ) else {
                    continue;
                };
                if namespace.value == "MDL" {
                    mdl.push(value);
                } else if !others.contains(&namespace.value) {
                    others.push(namespace.value);
                }
            }
            if !others.is_empty() {
                self.report.warning(
                    Code::Ignored,
                    set.span(),
                    format!(
                        "aliases in {} aren't imported: MDL reads only the MDL namespace",
                        others.join(", ")
                    ),
                );
            }
            match mdl.as_slice() {
                [] => {}
                [alias] => return Some((alias.value.trim().to_string(), alias.span)),
                [_, second, ..] => {
                    self.report.error(
                        Code::Invalid,
                        second.span,
                        "a parameter has one alias in the MDL namespace",
                    );
                    return None;
                }
            }
        }
        let name = el.item_name()?;
        let mut parts = Vec::new();
        for part in self.tree.names(space).into_iter().chain([name]) {
            match catalog_part(part) {
                Some(part) => parts.push(part),
                None => {
                    self.report.error(
                        Code::Name,
                        el.span(),
                        format!(
                            "{part} has no letters or digits for a catalog name; give the \
                             parameter an alias in the MDL namespace"
                        ),
                    );
                    return None;
                }
            }
        }
        let span = el.attr("name").map_or(el.span(), |a| a.span);
        Some((parts.join("."), span))
    }

    /// A parameter or member for messages: `/Root/Sub/Name.member`.
    fn describe(&self, key: &ParamKey<'d>) -> String {
        let mut text = format!("{}/{}", self.tree.path(key.0), key.1);
        for member in &key.2 {
            text.push('.');
            text.push_str(member);
        }
        text
    }

    /// Packet names: each container's, qualified by its SpaceSystems where
    /// containers in different ones share a name, or always if the
    /// options ask for it.
    fn name_packets(&mut self, packets: &mut [Packet<'d>]) {
        let items: Vec<(&'d str, usize, Span)> = packets
            .iter()
            .map(|p| (p.el.item_name().unwrap_or_default(), p.space, p.el.span()))
            .collect();
        let names = self.unique_names(&items, ("container", "packet"));
        for (packet, name) in packets.iter_mut().zip(names) {
            packet.name = name;
        }
        let mut seen: BTreeMap<&str, Span> = BTreeMap::new();
        for packet in packets.iter() {
            match seen.get(packet.name.as_str()) {
                Some(first) => self.report.duplicate(
                    Code::Name,
                    packet.el.span(),
                    *first,
                    format!("two packets are named {}", packet.name),
                ),
                None => {
                    seen.insert(&packet.name, packet.el.span());
                }
            }
        }
    }

    /// Struct type names, as packets are named; an element type is named
    /// for its field, which is unique already.
    fn name_types(&mut self) {
        let ids: Vec<TypeId<'d>> = self.structs.keys().cloned().collect();
        let containers: Vec<&TypeId<'d>> = ids
            .iter()
            .filter(|id| matches!(id, TypeId::Container(_)))
            .collect();
        let bases: Vec<String> = containers
            .iter()
            .map(|id| {
                self.structs
                    .get(*id)
                    .map(|t| t.base.clone())
                    .unwrap_or_default()
            })
            .collect();
        let items: Vec<(&str, usize, Span)> = containers
            .iter()
            .zip(&bases)
            .filter_map(|(id, base)| {
                self.structs
                    .get(*id)
                    .map(|t| (base.as_str(), t.space, t.span))
            })
            .collect();
        let names = self.unique_names(&items, ("container", "struct type"));
        let mut named: BTreeMap<TypeId<'d>, String> =
            containers.into_iter().cloned().zip(names).collect();
        for id in &ids {
            if let (TypeId::Element(..), Some(ty)) = (id, self.structs.get(id)) {
                named.insert(id.clone(), ty.base.clone());
            }
        }
        let mut seen: BTreeMap<String, Span> = BTreeMap::new();
        for (id, name) in named {
            let Some(ty) = self.structs.get_mut(&id) else {
                continue;
            };
            ty.name = name.clone();
            let span = ty.span;
            match seen.get(&name) {
                Some(first) => self.report.duplicate(
                    Code::Name,
                    span,
                    *first,
                    format!("two struct types are named {name}"),
                ),
                None => {
                    seen.insert(name, span);
                }
            }
        }
    }

    /// MDL names for items (their XTCE name, SpaceSystem and span), each
    /// qualified as the options and any shared names ask; changes are
    /// reported.
    fn unique_names(&mut self, items: &[(&str, usize, Span)], what: (&str, &str)) -> Vec<String> {
        let always = self.naming == Naming::Qualified;
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (index, (name, _, _)) in items.iter().enumerate() {
            groups.entry(identifier(name).0).or_default().push(index);
        }
        let mut names = vec![String::new(); items.len()];
        for indices in groups.values() {
            let group: Vec<(&str, Vec<&str>)> = indices
                .iter()
                .map(|&i| (items[i].0, self.tree.names(items[i].1)))
                .collect();
            let shared = indices.len() > 1 && !always;
            for (&index, name) in indices.iter().zip(qualified(&group, always)) {
                let (xtce, space, span) = items[index];
                self.renamed(&Renaming {
                    span,
                    space,
                    what,
                    xtce,
                    name: &name,
                    shared,
                });
                names[index] = name;
            }
        }
        names
    }

    /// Parameters no packet holds, when every packet imported; after an
    /// error, which packets hold what isn't known.
    fn report_unused(&mut self) {
        if self.report.has_errors() {
            return;
        }
        for (space, el) in self.tree.items(Kind::Parameter) {
            let Some(name) = el.item_name() else {
                continue;
            };
            if !self.seen.contains(&(space, name)) {
                self.report.warning(
                    Code::Ignored,
                    el.span(),
                    format!(
                        "parameter {}/{name} is in no packet, so it isn't imported",
                        self.tree.path(space)
                    ),
                );
            }
        }
    }

    /// Little-endian when every number wider than a byte is, else
    /// big-endian: a big-endian header ahead of little-endian data makes
    /// the family big-endian, its little-endian values naming their order.
    fn family_order(&self, packets: &[Packet<'_>]) -> Order {
        let entries = packets
            .iter()
            .flat_map(|packet| &packet.entries)
            .chain(self.structs.values().flat_map(|ty| &ty.entries));
        let mut orders = entries
            .filter_map(|entry| match entry {
                Entry::Field(field) => field.scalar.order.map(|(order, _)| order),
                Entry::Array(array) => match &array.element {
                    Element::Scalar(scalar) => scalar.order.map(|(order, _)| order),
                    Element::Struct(_) => None,
                },
                Entry::Struct(_) | Entry::Padding { .. } => None,
            })
            .peekable();
        if orders.peek().is_some() && orders.all(|order| order == Order::Little) {
            Order::Little
        } else {
            Order::Big
        }
    }
}

/// A name the importer may have changed, for reporting: where the item is,
/// what kind of item it is and what it becomes (`("container",
/// "packet")`), its XTCE name, its MDL name, and whether another item
/// shares its name.
#[derive(Clone, Copy)]
struct Renaming<'a> {
    span: Span,
    space: usize,
    what: (&'a str, &'a str),
    xtce: &'a str,
    name: &'a str,
    shared: bool,
}

/// Every parameter's field name: its own, or qualified by its SpaceSystems
/// when parameters in different ones share it (or always, if the options
/// ask), so a field's name is the same in every packet that holds it.
fn field_names<'d>(tree: &Tree<'d>, naming: Naming) -> BTreeMap<(usize, &'d str), (String, bool)> {
    let always = naming == Naming::Qualified;
    let mut groups: BTreeMap<String, Vec<(usize, &'d str)>> = BTreeMap::new();
    for (space, el) in tree.items(Kind::Parameter) {
        if let Some(name) = el.item_name() {
            groups
                .entry(identifier(name).0)
                .or_default()
                .push((space, name));
        }
    }
    let mut out = BTreeMap::new();
    for members in groups.values() {
        let items: Vec<(&str, Vec<&str>)> = members
            .iter()
            .map(|(space, name)| (*name, tree.names(*space)))
            .collect();
        let shared = members.len() > 1 && !always;
        for (key, name) in members.iter().zip(qualified(&items, always)) {
            out.insert(*key, (name, shared));
        }
    }
    out
}
