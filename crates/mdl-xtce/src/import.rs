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

use std::collections::{BTreeMap, BTreeSet};

use mdl::v2::ir::{IrVersion, Locator};
use mdl::v2::source::{SourceId, SourceMap, Span};
use serde_json::Value;

use crate::names::{catalog_part, identifier, qualified, token};
use crate::out::{List, Object};
use crate::report::Report;
use crate::space::{Kind, Tree};
use crate::types::{Order, Raw, Scalar, Shape, Types, Wanted, description, raw_value};
use crate::xml::{self, El, Expect};
use crate::{Code, Options};

/// A parameter, or a member of an aggregate one: its SpaceSystem, its
/// name, and the path of members.
type ParamKey<'d> = (usize, &'d str, Vec<&'d str>);

/// An element's identity, across files.
type ElId = (SourceId, usize);

/// A packet's fields so far, by the parameter or member each reads.
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

/// A field of a packet, as conditions and identify keys see it.
#[derive(Clone)]
struct Known {
    name: String,
    ty: &'static str,
    raw: Raw,
}

#[derive(Clone)]
enum Entry<'d> {
    Field(Box<Field<'d>>),
    Padding {
        bits: u32,
        span: Span,
        when: Option<Object>,
    },
}

#[derive(Clone)]
struct Field<'d> {
    name: String,
    key: ParamKey<'d>,
    span: Span,
    scalar: Scalar<'d>,
    canonical: (String, Span),
    offset: Option<(u32, Span)>,
    when: Option<Object>,
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

/// One comparison, resolved.
struct Comparison<'d> {
    el: El<'d>,
    field: (String, Span),
    op: &'static str,
    value: (Wanted, Span),
}

/// Where an entry's location puts it.
enum Location {
    /// This many bits from the start of the packet.
    Start(u32, Span),
    /// This many bits after the previous entry ends.
    After(u32, Span),
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

const ENTRIES: &[&str] = &[
    "ParameterRefEntry",
    "ParameterSegmentRefEntry",
    "ContainerRefEntry",
    "ContainerSegmentRefEntry",
    "StreamSegmentEntry",
    "IndirectParameterRefEntry",
    "ArrayParameterRefEntry",
];

const CRITERIA: &[&str] = &[
    "Comparison",
    "ComparisonList",
    "BooleanExpression",
    "CustomAlgorithm",
];

pub(crate) struct Import<'d, 'r> {
    tree: &'r Tree<'d>,
    report: &'r mut Report,
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
    /// Each parameter's field name, before members, and whether another
    /// parameter shares its name.
    field_names: BTreeMap<(usize, &'d str), (String, bool)>,
}

impl<'d, 'r> Import<'d, 'r> {
    pub(crate) fn new(tree: &'r Tree<'d>, report: &'r mut Report) -> Self {
        Import {
            tree,
            report,
            types: Types::default(),
            params: BTreeMap::new(),
            names: BTreeMap::new(),
            shapes: BTreeMap::new(),
            seen: BTreeSet::new(),
            parts: BTreeMap::new(),
            field_names: field_names(tree),
        }
    }

    /// The bundle, as JSON with its source map. `None` when there's nothing
    /// to build it from; every problem is reported.
    pub(crate) fn bundle(mut self, options: &Options) -> Option<(Value, SourceMap)> {
        let tree = self.tree;
        let mut packets = Vec::new();
        for (space, el) in tree.items(Kind::Container) {
            let concrete = match el.attr("abstract") {
                Some(attr) => !xml::boolean(attr, self.report).unwrap_or(true),
                None => true,
            };
            if concrete && let Some(packet) = self.packet(space, el) {
                packets.push(packet);
            }
        }
        self.name_packets(&mut packets);
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
        let order = family_order(&packets);

        let mut params = Object::default();
        for (name, param) in &self.params {
            params.put_object(name, param_object(param));
        }
        let mut catalog = Object::at(root_span);
        catalog.put_object("params", params);

        let mut family_object = Object::at(root_span);
        family_object.put("name", family.as_str(), None);
        family_object.put("encoding", "binary", None);
        family_object.put("byte_order", order.as_str(), None);
        family_object.put("alias", alias, None);
        let mut families = List::default();
        families.push_object(family_object);

        let mut packet_list = List::default();
        for packet in &packets {
            packet_list.push_object(packet_object(packet, order));
        }
        let mut set_object = Object::at(root_span);
        set_object.put("name", set, None);
        set_object.put("family", family, None);
        set_object.put_list("packets", packet_list);
        let mut sets = List::default();
        sets.push_object(set_object);

        let mut bundle = Object::at(root_span);
        bundle.put("mdl_version", IrVersion::CURRENT.to_string(), None);
        bundle.put_object("catalog", catalog);
        bundle.put_list("families", families);
        bundle.put_list("sets", sets);
        Some(bundle.finish())
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
        self.check_fields(&entries)?;
        Some(Packet {
            name: el.item_name()?.to_string(),
            space,
            el,
            entries,
            identify,
        })
    }

    /// Each parameter or member at most once in a packet, and no two
    /// fields with one name.
    fn check_fields(&mut self, entries: &[Entry<'d>]) -> Option<()> {
        let mut by_key: BTreeMap<&ParamKey<'d>, &Field<'d>> = BTreeMap::new();
        let mut by_name: BTreeMap<&str, &Field<'d>> = BTreeMap::new();
        let mut complete = true;
        for entry in entries {
            let Entry::Field(field) = entry else {
                continue;
            };
            if let Some(first) = by_key.get(&field.key) {
                let message = format!(
                    "{} is in this packet twice, and MDL maps a param from one field per packet",
                    self.describe(&field.key)
                );
                self.report
                    .duplicate(Code::Unsupported, field.span, first.span, message);
                complete = false;
            } else if let Some(first) = by_name.get(field.name.as_str()) {
                let message = format!(
                    "{} and {} are both field {} of this packet",
                    self.describe(&first.key),
                    self.describe(&field.key),
                    field.name
                );
                self.report
                    .duplicate(Code::Name, field.span, first.span, message);
                complete = false;
            } else {
                by_key.insert(&field.key, field);
                by_name.insert(&field.name, field);
            }
        }
        complete.then_some(())
    }

    /// Packet names: each container's, qualified by its SpaceSystems where
    /// containers in different ones share a name.
    fn name_packets(&mut self, packets: &mut [Packet<'d>]) {
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (index, packet) in packets.iter().enumerate() {
            groups
                .entry(identifier(&packet.name).0)
                .or_default()
                .push(index);
        }
        for indices in groups.values() {
            let items: Vec<(&str, Vec<&str>)> = indices
                .iter()
                .map(|&i| {
                    let xtce = packets[i].el.item_name().unwrap_or_default();
                    (xtce, self.tree.names(packets[i].space))
                })
                .collect();
            let names = qualified(&items);
            for ((&index, (xtce, _)), name) in indices.iter().zip(&items).zip(names) {
                if name != *xtce {
                    let packet = &packets[index];
                    let why = if indices.len() > 1 {
                        format!("another container is also named {xtce}")
                    } else {
                        "MDL names are letters, digits and '_'".to_string()
                    };
                    self.report.warning(
                        Code::Renamed,
                        packet.el.span(),
                        format!(
                            "container {}/{xtce} is packet {name}: {why}",
                            self.tree.path(packet.space)
                        ),
                    );
                }
                packets[index].name = name;
            }
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
        list.check(
            &Expect {
                attrs: &[],
                children: ENTRIES,
                ignored: &[],
                text: false,
            },
            self.report,
        );
        let mut known: Fields<'d> = above.to_vec();
        let mut entries = Vec::new();
        let mut fields = Vec::new();
        let mut complete = true;
        for entry in list.children() {
            match entry.name() {
                "ParameterRefEntry" => match self.parameter_entry(space, entry, &known) {
                    Some((new_entries, new_fields)) => {
                        entries.extend(new_entries);
                        known.extend(new_fields.iter().cloned());
                        fields.extend(new_fields);
                    }
                    None => complete = false,
                },
                name @ ("ContainerRefEntry" | "ArrayParameterRefEntry") => {
                    self.report.error(
                        Code::Unsupported,
                        entry.span(),
                        format!("{name} isn't supported yet"),
                    );
                    complete = false;
                }
                name @ ("ParameterSegmentRefEntry"
                | "ContainerSegmentRefEntry"
                | "StreamSegmentEntry"
                | "IndirectParameterRefEntry") => {
                    self.report.error(
                        Code::Unsupported,
                        entry.span(),
                        format!("{name} isn't supported: MDL reads whole values at known places"),
                    );
                    complete = false;
                }
                _ => {}
            }
        }
        complete.then_some(Part {
            entries,
            fields,
            identify,
        })
    }

    /// A BaseContainer's restriction criteria as identify keys.
    fn restriction(
        &mut self,
        space: usize,
        base: El<'d>,
        above: &[(ParamKey<'d>, Known)],
    ) -> Option<Vec<(String, i64, Span)>> {
        base.check(
            &Expect {
                attrs: &["containerRef"],
                children: &["RestrictionCriteria"],
                ignored: &[],
                text: false,
            },
            self.report,
        );
        let Some(criteria) = base.child("RestrictionCriteria") else {
            return Some(Vec::new());
        };
        let mut children = CRITERIA.to_vec();
        children.push("NextContainer");
        criteria.check(
            &Expect {
                attrs: &[],
                children: &children,
                ignored: &[],
                text: false,
            },
            self.report,
        );
        if let Some(next) = criteria.child("NextContainer") {
            self.report.error(
                Code::Unsupported,
                next.span(),
                "identification by the next container isn't supported",
            );
            return None;
        }
        let comparisons = self.comparisons(criteria, "identification")?;
        let mut keys = Vec::new();
        for el in comparisons {
            let comparison = self.comparison(space, el, above, Use::Identify)?;
            let field = &comparison.field.0;
            let problem = match (comparison.op, &comparison.value.0) {
                ("eq", Wanted::One(value)) => {
                    keys.push((field.clone(), *value, el.span()));
                    continue;
                }
                ("eq", Wanted::Any(_)) => format!(
                    "this label of {field} stands for several values, and MDL identifies a packet \
                     by one value of each key"
                ),
                ("eq", Wanted::NonZero) => format!(
                    "{field} is a boolean wider than a bit, true for any value but 0, and MDL \
                     identifies a packet by one value of each key"
                ),
                _ => "MDL identifies packets by equal values; this comparison isn't one".into(),
            };
            self.report.error(Code::Unsupported, el.span(), problem);
            return None;
        }
        Some(keys)
    }

    /// The comparisons of a match criteria that holds a Comparison or a
    /// ComparisonList; anything else is reported.
    fn comparisons(&mut self, criteria: El<'d>, what: &str) -> Option<Vec<El<'d>>> {
        if let Some(comparison) = criteria.child("Comparison") {
            return Some(vec![comparison]);
        }
        if let Some(list) = criteria.child("ComparisonList") {
            list.check(
                &Expect {
                    attrs: &[],
                    children: &["Comparison"],
                    ignored: &[],
                    text: false,
                },
                self.report,
            );
            return Some(list.all("Comparison").collect());
        }
        if let Some(expression) = criteria.child("BooleanExpression") {
            self.report.error(
                Code::Unsupported,
                expression.span(),
                format!("{what} by a boolean expression isn't supported yet"),
            );
            return None;
        }
        if let Some(algorithm) = criteria.child("CustomAlgorithm") {
            self.report.error(
                Code::Unsupported,
                algorithm.span(),
                format!("{what} by an algorithm isn't supported"),
            );
            return None;
        }
        self.report.error(
            Code::Invalid,
            criteria.span(),
            format!("{} holds a comparison", criteria.name()),
        );
        None
    }

    fn comparison(
        &mut self,
        space: usize,
        el: El<'d>,
        known: &[(ParamKey<'d>, Known)],
        purpose: Use,
    ) -> Option<Comparison<'d>> {
        el.check(
            &Expect {
                attrs: &[
                    "parameterRef",
                    "value",
                    "comparisonOperator",
                    "useCalibratedValue",
                    "instance",
                ],
                children: &[],
                ignored: &[],
                text: false,
            },
            self.report,
        );
        let reference = el.required("parameterRef", self.report)?;
        let value = el.required("value", self.report)?;
        if let Some(instance) = el.attr("instance")
            && xml::long(instance, self.report)? != 0
        {
            self.report.error(
                Code::Unsupported,
                instance.span,
                "a comparison with an earlier instance of a parameter isn't supported",
            );
            return None;
        }
        let op = match el.attr("comparisonOperator") {
            None => "eq",
            Some(attr) => match attr.value.trim() {
                "==" => "eq",
                "!=" => "ne",
                "<" => "lt",
                "<=" => "le",
                ">" => "gt",
                ">=" => "ge",
                other => {
                    self.report.error(
                        Code::Invalid,
                        attr.span,
                        format!("{other:?} isn't a comparison operator"),
                    );
                    return None;
                }
            },
        };
        let calibrated = match el.attr("useCalibratedValue") {
            Some(attr) => xml::boolean(attr, self.report)?,
            None => true,
        };
        let found = match self.tree.resolve(space, reference.value, Kind::Parameter) {
            Ok(found) => found,
            Err(message) => {
                self.report.error(Code::Reference, reference.span, message);
                return None;
            }
        };
        let name = found.el.item_name()?;
        self.seen.insert((found.space, name));
        let key = (found.space, name, found.members);
        let Some((_, field)) = known.iter().find(|(k, _)| *k == key) else {
            let message = match purpose {
                Use::Identify => format!(
                    "{} isn't a field of the containers this one inherits from, and MDL \
                     identifies a packet by its own fields",
                    reference.value
                ),
                Use::Condition => format!(
                    "{} isn't a field decoded earlier in the packet, and MDL conditions compare \
                     only those",
                    reference.value
                ),
            };
            self.report
                .error(Code::Unsupported, reference.span, message);
            return None;
        };
        let labelled = matches!(field.raw, Raw::Labels(_) | Raw::Boolean { .. });
        if calibrated && !matches!(op, "eq" | "ne") && labelled {
            self.report.error(
                Code::Unsupported,
                el.span(),
                "ordering comparisons of labelled values aren't supported",
            );
            return None;
        }
        let raw = match raw_value(field.ty, &field.raw, value.value, calibrated) {
            Ok(raw) => raw,
            Err(reason) => {
                self.report.error(
                    Code::Unsupported,
                    value.span,
                    format!(
                        "this comparison of {} can't be imported: {reason}",
                        reference.value
                    ),
                );
                return None;
            }
        };
        Some(Comparison {
            el,
            field: (field.name.clone(), reference.span),
            op,
            value: (raw, value.span),
        })
    }

    /// An IncludeCondition as an MDL condition.
    fn condition(
        &mut self,
        space: usize,
        el: El<'d>,
        known: &[(ParamKey<'d>, Known)],
    ) -> Option<Object> {
        el.check(
            &Expect {
                attrs: &[],
                children: CRITERIA,
                ignored: &[],
                text: false,
            },
            self.report,
        );
        let comparisons = self.comparisons(el, "inclusion")?;
        let mut objects = Vec::new();
        for comparison in comparisons {
            let comparison = self.comparison(space, comparison, known, Use::Condition)?;
            let at = comparison.el.span();
            let compare = |op: &str, value: i64| {
                let mut object = Object::at(at);
                object.put("kind", "compare", None);
                object.put(
                    "field",
                    comparison.field.0.as_str(),
                    Some(comparison.field.1),
                );
                object.put("op", op, None);
                object.put("value", value, Some(comparison.value.1));
                object
            };
            objects.push(match (&comparison.value.0, comparison.op) {
                (Wanted::One(value), op) => compare(op, *value),
                // A wide boolean is true for any value but 0.
                (Wanted::NonZero, "eq") => compare("ne", 0),
                (Wanted::NonZero, _) => compare("eq", 0),
                // A label on several values: one of them, or none.
                (Wanted::Any(values), "eq") => group(
                    "any",
                    values.iter().map(|v| compare("eq", *v)).collect(),
                    at,
                ),
                (Wanted::Any(values), _) => group(
                    "all",
                    values.iter().map(|v| compare("ne", *v)).collect(),
                    at,
                ),
            });
        }
        if objects.len() == 1 {
            return objects.pop();
        }
        Some(group("all", objects, el.span()))
    }

    /// A ParameterRefEntry's fields: one, or one per member of an
    /// aggregate, after any padding its location asks for.
    fn parameter_entry(
        &mut self,
        space: usize,
        entry: El<'d>,
        known: &[(ParamKey<'d>, Known)],
    ) -> Option<(Vec<Entry<'d>>, Fields<'d>)> {
        entry.check(
            &Expect {
                attrs: &["parameterRef"],
                children: &[
                    "LocationInContainerInBits",
                    "IncludeCondition",
                    "RepeatEntry",
                    "TimeAssociation",
                ],
                ignored: &["shortDescription", "AncillaryDataSet"],
                text: false,
            },
            self.report,
        );
        time_association(entry, self.report)?;
        if let Some(repeat) = entry.child("RepeatEntry") {
            self.report.error(
                Code::Unsupported,
                repeat.span(),
                "repeated entries aren't supported yet",
            );
            return None;
        }
        let reference = entry.required("parameterRef", self.report)?;
        let found = match self.tree.resolve(space, reference.value, Kind::Parameter) {
            Ok(found) => found,
            Err(message) => {
                self.report.error(Code::Reference, reference.span, message);
                return None;
            }
        };
        let name = found.el.item_name()?;
        self.seen.insert((found.space, name));
        if !found.members.is_empty() {
            self.report.error(
                Code::Invalid,
                reference.span,
                "an entry names a whole parameter, not a member of one",
            );
            return None;
        }
        let shape = self.parameter_shape(found.space, found.el)?;
        let when = match entry.child("IncludeCondition") {
            Some(condition) => Some(self.condition(space, condition, known)?),
            None => None,
        };
        let location = match entry.child("LocationInContainerInBits") {
            Some(location) => Some(self.location(location)?),
            None => None,
        };
        let mut leaves = Vec::new();
        flatten(&shape, Vec::new(), None, &mut leaves);
        let mut entries = Vec::new();
        let mut fields = Vec::new();
        if let Some(Location::After(bits, span)) = location
            && bits > 0
        {
            entries.push(Entry::Padding {
                bits,
                span,
                when: when.clone(),
            });
        }
        for (index, leaf) in leaves.into_iter().enumerate() {
            let Leaf {
                members,
                scalar,
                description,
            } = leaf;
            let key = (found.space, name, members);
            let canonical = self.register(&key, found.el, &scalar, description)?;
            let field_name = self.field_name(&key, found.el);
            let offset = match (index, &location) {
                (0, Some(Location::Start(bits, span))) => Some((*bits, *span)),
                _ => None,
            };
            fields.push((
                key.clone(),
                Known {
                    name: field_name.clone(),
                    ty: scalar.ty.0,
                    raw: scalar.raw.clone(),
                },
            ));
            entries.push(Entry::Field(Box::new(Field {
                name: field_name,
                key,
                span: entry.span(),
                scalar,
                canonical,
                offset,
                when: when.clone(),
            })));
        }
        Some((entries, fields))
    }

    fn location(&mut self, el: El<'d>) -> Option<Location> {
        el.check(
            &Expect {
                attrs: &["referenceLocation"],
                children: &["FixedValue", "DynamicValue", "DiscreteLookupList"],
                ignored: &[],
                text: false,
            },
            self.report,
        );
        let Some(fixed) = el.child("FixedValue") else {
            self.report.error(
                Code::Unsupported,
                el.span(),
                "a location worked out from other values isn't supported",
            );
            return None;
        };
        let (text, span) = fixed.text();
        let bits = xml::long_text(&text, "FixedValue", span, self.report)?;
        let reference = el.attr("referenceLocation");
        match reference.map_or("previousEntry", |a| a.value.trim()) {
            "containerStart" => match u32::try_from(bits) {
                Ok(bits) => Some(Location::Start(bits, span)),
                Err(_) => {
                    self.report.error(
                        Code::Unsupported,
                        span,
                        format!("{bits} isn't a bit position MDL can place an entry at"),
                    );
                    None
                }
            },
            "previousEntry" => match u32::try_from(bits) {
                Ok(bits) => Some(Location::After(bits, span)),
                Err(_) => {
                    self.report.error(
                        Code::Unsupported,
                        span,
                        "an entry placed before the end of the previous one isn't supported",
                    );
                    None
                }
            },
            "containerEnd" | "nextEntry" => {
                self.report.error(
                    Code::Unsupported,
                    reference.map_or(el.span(), |a| a.span),
                    "locations from the container's end or the next entry aren't supported: MDL \
                     places entries from the start",
                );
                None
            }
            other => {
                self.report.error(
                    Code::Invalid,
                    reference.map_or(el.span(), |a| a.span),
                    format!("{other:?} isn't a reference location"),
                );
                None
            }
        }
    }

    /// XTCE doesn't place an absent entry, so the next one follows the
    /// entry before it; MDL starts the next one where the absent entry
    /// would have started. The two differ only for an absent entry at a
    /// fixed location followed by one placed after it. An aggregate's
    /// members are one entry: present or absent together.
    fn check_absent_locations(&mut self, entries: &[Entry<'d>]) -> Option<()> {
        // The ParameterRefEntry of a conditional field at a fixed location,
        // while its fields go on.
        let mut placed: Option<&Field<'d>> = None;
        for entry in entries {
            let (span, offset) = match entry {
                Entry::Field(field) => (Some(field.span), field.offset),
                Entry::Padding { .. } => (None, None),
            };
            if let Some(first) = placed {
                if span == Some(first.span) {
                    continue;
                }
                if let (None, Some((_, at))) = (offset, first.offset) {
                    let name = &first.name;
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
            placed = match entry {
                Entry::Field(field) if field.when.is_some() && field.offset.is_some() => {
                    Some(field)
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
            if time_association(properties, self.report).is_none() {
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
        if name != xtce {
            let why = if shared {
                format!("another parameter is also named {}", key.1)
            } else {
                "MDL names are letters, digits and '_'".to_string()
            };
            self.report.warning(
                Code::Renamed,
                el.span(),
                format!(
                    "parameter {}/{xtce} is field {name}: {why}",
                    self.tree.path(key.0)
                ),
            );
        }
        name
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
}

#[derive(Clone, Copy)]
enum Use {
    Identify,
    Condition,
}

/// Refuses a TimeAssociation: it times a value by another parameter, and
/// an MDL sample takes its packet's time.
fn time_association(el: El<'_>, report: &mut Report) -> Option<()> {
    match el.child("TimeAssociation") {
        Some(association) => {
            report.error(
                Code::Unsupported,
                association.span(),
                "a time association isn't supported: it times the value by another parameter, \
                 and an MDL sample takes its packet's time",
            );
            None
        }
        None => Some(()),
    }
}

/// An `all` or `any` condition of these.
fn group(kind: &str, conditions: Vec<Object>, span: Span) -> Object {
    let mut object = Object::at(span);
    object.put("kind", kind, None);
    let mut list = List::default();
    for condition in conditions {
        list.push_object(condition);
    }
    object.put_list("conditions", list);
    object
}

/// Every parameter's field name: its own, or qualified by its SpaceSystems
/// when parameters in different ones share it, so a field's name is the
/// same in every packet that holds it.
fn field_names<'d>(tree: &Tree<'d>) -> BTreeMap<(usize, &'d str), (String, bool)> {
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
        let shared = members.len() > 1;
        for (key, name) in members.iter().zip(qualified(&items)) {
            out.insert(*key, (name, shared));
        }
    }
    out
}

/// One scalar of a parameter: the parameter itself, or a member of it.
struct Leaf<'d> {
    members: Vec<&'d str>,
    scalar: Scalar<'d>,
    /// A member's description; a parameter's own is on its element.
    description: Option<(String, Span)>,
}

/// The scalars of a shape, with each one's member path, depth first.
fn flatten<'d>(
    shape: &Shape<'d>,
    path: Vec<&'d str>,
    description: Option<(String, Span)>,
    out: &mut Vec<Leaf<'d>>,
) {
    match shape {
        Shape::Scalar(scalar) => out.push(Leaf {
            members: path,
            scalar: (**scalar).clone(),
            description,
        }),
        Shape::Aggregate(members) => {
            for member in members {
                let mut path = path.clone();
                path.push(member.name);
                flatten(&member.shape, path, member.description.clone(), out);
            }
        }
    }
}

/// Little-endian when every number wider than a byte is, else big-endian:
/// a big-endian header ahead of little-endian data makes the family
/// big-endian, its little-endian fields naming their order.
fn family_order(packets: &[Packet<'_>]) -> Order {
    let mut orders = packets
        .iter()
        .flat_map(|packet| &packet.entries)
        .filter_map(|entry| match entry {
            Entry::Field(field) => field.scalar.order.map(|(order, _)| order),
            Entry::Padding { .. } => None,
        })
        .peekable();
    if orders.peek().is_some() && orders.all(|order| order == Order::Little) {
        Order::Little
    } else {
        Order::Big
    }
}

fn param_object(param: &Param<'_>) -> Object {
    let scalar = &param.scalar;
    let mut object = Object::at(param.el.span());
    object.put("type", scalar.param, Some(scalar.el.span()));
    if let Some((unit, span)) = &scalar.unit {
        object.put("unit", unit.as_str(), Some(*span));
    }
    if !scalar.states.is_empty() {
        let mut states = List::default();
        for (state, span) in &scalar.states {
            states.push(state.as_str(), Some(*span));
        }
        object.put_list("states", states);
    }
    if let Some((text, span)) = &param.description {
        object.put("description", text.as_str(), Some(*span));
    }
    object
}

fn packet_object(packet: &Packet<'_>, order: Order) -> Object {
    let mut object = Object::at(packet.el.span());
    object.put(
        "name",
        packet.name.as_str(),
        packet.el.attr("name").map(|a| a.span),
    );
    if !packet.identify.is_empty() {
        let mut identify = Object::default();
        for (key, (value, span)) in &packet.identify {
            identify.put(key, *value, Some(*span));
        }
        object.put_object("identify", identify);
    }
    let mut entries = List::default();
    for entry in &packet.entries {
        entries.push_object(entry_object(entry, order));
    }
    object.put_list("entries", entries);
    object
}

fn entry_object(entry: &Entry<'_>, order: Order) -> Object {
    match entry {
        Entry::Padding { bits, span, when } => {
            let mut object = Object::at(*span);
            object.put("kind", "padding", None);
            object.put("size_bits", *bits, Some(*span));
            if let Some(when) = when {
                object.put_object("when", when.clone());
            }
            object
        }
        Entry::Field(field) => {
            let scalar = &field.scalar;
            let mut object = Object::at(field.span);
            object.put("kind", "field", None);
            object.put("name", field.name.as_str(), None);
            object.put("type", scalar.ty.0, Some(scalar.ty.1));
            if let Some((bits, span)) = field.offset {
                object.put("offset_bits", bits, Some(span));
            }
            if let Some((bits, span)) = scalar.size_bits {
                object.put("size_bits", bits, Some(span));
            }
            if let Some((own, span)) = scalar.order
                && own != order
            {
                object.put("byte_order", own.as_str(), Some(span));
            }
            if let Some((length, span)) = &scalar.length {
                object.put("length", length.clone(), Some(*span));
            }
            if let Some((text, span)) = scalar.text {
                object.put("text", text, Some(span));
            }
            if let Some(conversion) = &scalar.conversion {
                object.put_object("conversion", conversion.clone());
            }
            if let Some((unit, span)) = &scalar.unit {
                object.put("unit", unit.as_str(), Some(*span));
            }
            object.put(
                "canonical",
                field.canonical.0.as_str(),
                Some(field.canonical.1),
            );
            if let Some(when) = &field.when {
                object.put_object("when", when.clone());
            }
            object
        }
    }
}
