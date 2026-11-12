//! Composites (`spec/decoding.md` §7.4): what a vec3 or quat param is
//! built from, generated with the items that hold its members (fields, a
//! struct, an array, or an array of structs), found among a body's
//! laid-out entries, and the sample a message's values give.

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::{Index, select};
use serde_json::{Value, json};

use super::{
    ArraySpec, CountSpec, ElementNode, ElementSpec, Entry, ItemSpec, Node, TypeSpec, WhenSpec,
    interval, when_spec,
};
use crate::layout::meaning::{member_value, rank};
use crate::layout::{
    Expected, FieldSpec, Kind, Placed, Raw, Sample, UNIT_GROUPS, number_in, sentinels,
};

/// A composite (§7.4), and where its members are among the body's items.
#[derive(Debug, Clone)]
pub(super) struct CompositeSpec {
    head: Head,
    /// Each member, in the param's order: an item of the body and where in
    /// it.
    members: Vec<(usize, Inside)>,
}

/// What a composite maps to: a vec3 or quat param in a frame and unit,
/// from a source frame that converts to the param's.
#[derive(Debug, Clone, Copy)]
struct Head {
    quat: bool,
    frame: &'static str,
    param_frame: &'static str,
    unit: Option<&'static str>,
}

impl Head {
    fn size(&self) -> usize {
        if self.quat { 4 } else { 3 }
    }

    /// The unit member `k` converts into: degrees for a geodetic latitude
    /// or longitude, else the param's.
    fn target(&self, k: usize) -> Option<&'static str> {
        if self.frame == "GEODETIC" && k < 2 {
            Some("deg")
        } else {
            self.unit
        }
    }
}

/// Where a composite's member is in an item.
#[derive(Debug, Clone, Copy)]
enum Inside {
    /// The item is the member.
    Whole,
    /// Field `j` of a struct entry.
    Field(usize),
    /// Element `i` of an array.
    Element(u32),
    /// Field `j` of element `i` of an array of structs.
    Record(u32, usize),
}

/// A composite and the items holding its members, which the body takes in
/// among its own.
#[derive(Debug, Clone)]
pub(super) struct CompositeSource {
    head: Head,
    pub(super) items: Vec<ItemSpec>,
    /// Each member: one of `items`, and where in it.
    members: Vec<(usize, Inside)>,
}

/// What holds a composite's members.
#[derive(Debug, Clone, Copy)]
enum Holder {
    /// A field each.
    Fields,
    /// Fields of one struct entry.
    Struct,
    /// Elements of an array of numbers, which share a unit, so not a
    /// geodetic position's.
    Array,
    /// Fields of elements of an array of structs.
    Records,
}

/// For each frame a catalog param can have, the frames a vec3 source can
/// be in to convert to it; a quaternion keeps its frame (§7.4).
const FRAMES: &[(&str, &[&str])] = &[
    ("ECEF", &["ECEF", "ECI_J2000", "NED", "ENU", "GEODETIC"]),
    (
        "ECI_J2000",
        &["ECI_J2000", "ECEF", "NED", "ENU", "GEODETIC"],
    ),
    ("NED", &["NED", "ENU"]),
    ("ENU", &["ENU", "NED"]),
    ("BODY", &["BODY"]),
];

fn head() -> BoxedStrategy<Head> {
    let unit = proptest::option::of(select(UNIT_GROUPS).prop_flat_map(select));
    (any::<bool>(), select(FRAMES), unit)
        .prop_flat_map(|(quat, (param_frame, sources), unit)| {
            let frame = if quat {
                Just(param_frame).boxed()
            } else {
                select(sources).boxed()
            };
            (Just(quat), frame, Just(param_frame), Just(unit))
        })
        .prop_map(|(quat, frame, param_frame, unit)| Head {
            quat,
            frame,
            param_frame,
            unit,
        })
        .boxed()
}

/// The units of `unit`'s dimension.
fn group_of(unit: Option<&str>) -> Option<&'static [&'static str]> {
    let unit = unit?;
    UNIT_GROUPS
        .iter()
        .copied()
        .find(|group| group.contains(&unit))
}

/// A number in a unit of `group` (in none without one), through any
/// conversion but states: what a composite's member can be.
fn member_spec(group: Option<&'static [&'static str]>) -> BoxedStrategy<FieldSpec> {
    let kinds = prop_oneof![
        3 => (1_u32..=64).prop_map(Kind::Uint),
        3 => (1_u32..=64).prop_map(Kind::Int),
        2 => select(&[32_u32, 64][..]).prop_map(Kind::Float),
    ];
    (kinds, any::<bool>(), any::<bool>())
        .prop_flat_map(move |(kind, flip, mapped)| {
            let units = match group {
                Some(group) => (select(group), select(group)).prop_map(Some).boxed(),
                None => Just(None).boxed(),
            };
            (number_in(&kind, units), sentinels(&kind), Just(kind)).prop_map(
                move |(meaning, invalid, kind)| FieldSpec {
                    kind,
                    meaning,
                    invalid,
                    flip,
                    mapped,
                },
            )
        })
        .boxed()
}

/// A composite, its members held by fields, a struct, an array or an
/// array of structs, in any order there.
pub(super) fn composite_source() -> BoxedStrategy<CompositeSource> {
    head()
        .prop_flat_map(|head| {
            let n = head.size();
            let members: Vec<BoxedStrategy<FieldSpec>> = (0..n)
                .map(|k| member_spec(group_of(head.target(k))))
                .collect();
            let holders: &'static [Holder] = if head.frame == "GEODETIC" {
                &[Holder::Fields, Holder::Struct, Holder::Records]
            } else {
                &[
                    Holder::Fields,
                    Holder::Struct,
                    Holder::Array,
                    Holder::Records,
                ]
            };
            let fields: Vec<usize> = (0..n).collect();
            let elements: Vec<u32> = (0..n + 2).map(|i| i as u32).collect();
            let draws = (
                member_spec(group_of(head.target(0))),
                0_u32..=2,
                proptest::option::weighted(0.3, interval()),
                Just(fields).prop_shuffle(),
                Just(elements).prop_shuffle(),
                vec(any::<Index>(), n),
                proptest::option::weighted(0.25, when_spec()),
            )
                .prop_map(
                    |(element, extra, interval, fields, elements, records, when)| Draws {
                        element,
                        extra,
                        interval,
                        fields,
                        elements,
                        records,
                        when,
                    },
                );
            (Just(head), members, select(holders), draws)
        })
        .prop_map(|(head, members, holder, draws)| holding(head, members, holder, draws))
        .boxed()
}

/// What a holder of a composite's members draws: an array's element and
/// how many elements past the members it has, and its interval; field
/// `fields[k]` of a struct holds member `k`; an array's members are its
/// first elements in `elements` that it has; an array of structs holds
/// member `k` in element `records[k]`. With `when`, its first item is
/// there only when that holds, so the composite may have no sample
/// (§7.4).
#[derive(Debug, Clone)]
struct Draws {
    element: FieldSpec,
    extra: u32,
    interval: Option<f64>,
    fields: Vec<usize>,
    elements: Vec<u32>,
    records: Vec<Index>,
    when: Option<WhenSpec>,
}

fn holding(head: Head, members: Vec<FieldSpec>, holder: Holder, draws: Draws) -> CompositeSource {
    let n = members.len();
    let fields = draws.fields;
    let struct_type = |members: Vec<FieldSpec>| {
        let mut items: Vec<Option<ItemSpec>> = vec![None; n];
        for (k, member) in members.into_iter().enumerate() {
            items[fields[k]] = Some(ItemSpec::Field(member));
        }
        TypeSpec {
            items: items.into_iter().flatten().collect(),
        }
    };
    let (items, members): (Vec<ItemSpec>, Vec<(usize, Inside)>) = match holder {
        Holder::Fields => (
            members.into_iter().map(ItemSpec::Field).collect(),
            (0..n).map(|k| (k, Inside::Whole)).collect(),
        ),
        Holder::Struct => (
            vec![ItemSpec::Struct(struct_type(members))],
            (0..n).map(|k| (0, Inside::Field(fields[k]))).collect(),
        ),
        Holder::Array => {
            let count = n as u32 + draws.extra;
            let interval = draws.interval.filter(|_| draws.element.mapped);
            let array = ArraySpec {
                count: CountSpec::Fixed(count),
                element: ElementSpec::Scalar(draws.element),
                interval,
            };
            let members = draws
                .elements
                .iter()
                .filter(|i| **i < count)
                .take(n)
                .map(|i| (0, Inside::Element(*i)))
                .collect();
            (vec![ItemSpec::Array(array)], members)
        }
        Holder::Records => {
            let count = 1 + draws.extra;
            let array = ArraySpec {
                count: CountSpec::Fixed(count),
                element: ElementSpec::Struct(struct_type(members)),
                interval: None,
            };
            let members = (0..n)
                .map(|k| {
                    let i = draws.records[k].index(count as usize) as u32;
                    (0, Inside::Record(i, fields[k]))
                })
                .collect();
            (vec![ItemSpec::Array(array)], members)
        }
    };
    let items = match draws.when {
        Some(when) => {
            let mut items = items.into_iter();
            let first = items.next().map(|item| ItemSpec::Conditional {
                when,
                item: Box::new(item),
            });
            first.into_iter().chain(items).collect()
        }
        None => items,
    };
    CompositeSource {
        head,
        items,
        members,
    }
}

impl CompositeSource {
    /// The composite, its members' items found where the body put them:
    /// item `k` of this source at `positions[k]`.
    pub(super) fn placed(self, positions: &[usize]) -> CompositeSpec {
        CompositeSpec {
            head: self.head,
            members: self
                .members
                .into_iter()
                .map(|(item, inside)| (positions[item], inside))
                .collect(),
        }
    }
}

/// A composite, its members found.
#[derive(Debug, Clone)]
pub(super) struct CompositeNode {
    param: String,
    head: Head,
    members: Vec<Member>,
}

#[derive(Debug, Clone)]
struct Member {
    path: String,
    steps: Vec<Step>,
    field: Placed,
    target: Option<&'static str>,
}

/// A step of a record path.
#[derive(Debug, Clone)]
enum Step {
    Name(String),
    Index(usize),
}

/// A composite's members, found among the laid-out entries by the names
/// their items took.
pub(super) fn resolve(
    spec: CompositeSpec,
    param: &str,
    entries: &[Entry],
) -> Option<CompositeNode> {
    let members = spec
        .members
        .iter()
        .enumerate()
        .map(|(k, &(item, inside))| {
            let name = format!("f{item}");
            let node = &entries
                .iter()
                .find(|entry| node_name(&entry.node) == Some(name.as_str()))?
                .node;
            let first = Step::Name(name);
            let (steps, field) = match (node, inside) {
                (Node::Field(field), Inside::Whole) => (vec![first], field),
                (Node::Struct(node), Inside::Field(j)) => {
                    let inner = format!("f{j}");
                    let field = field_named(&node.ty.entries, &inner)?;
                    (vec![first, Step::Name(inner)], field)
                }
                (Node::Array(array), Inside::Element(i)) => match &array.element {
                    ElementNode::Scalar(element) => (vec![first, Step::Index(i as usize)], element),
                    ElementNode::Struct(_) => return None,
                },
                (Node::Array(array), Inside::Record(i, j)) => match &array.element {
                    ElementNode::Struct(ty) => {
                        let inner = format!("f{j}");
                        let field = field_named(&ty.entries, &inner)?;
                        (
                            vec![first, Step::Index(i as usize), Step::Name(inner)],
                            field,
                        )
                    }
                    ElementNode::Scalar(_) => return None,
                },
                _ => return None,
            };
            Some(Member {
                path: path_of(&steps),
                steps,
                field: field.clone(),
                target: spec.head.target(k),
            })
        })
        .collect::<Option<Vec<Member>>>()?;
    Some(CompositeNode {
        param: param.to_string(),
        head: spec.head,
        members,
    })
}

/// A record path as §6 writes it: names joined by `.`, an element's index
/// in brackets.
fn path_of(steps: &[Step]) -> String {
    let mut path = String::new();
    for step in steps {
        match step {
            Step::Name(name) if path.is_empty() => path.push_str(name),
            Step::Name(name) => {
                path.push('.');
                path.push_str(name);
            }
            Step::Index(index) => path.push_str(&format!("[{index}]")),
        }
    }
    path
}

fn node_name(node: &Node) -> Option<&str> {
    match node {
        Node::Padding(_) => None,
        Node::Field(field) => Some(&field.name),
        Node::Array(array) => Some(&array.name),
        Node::Struct(node) => Some(&node.name),
    }
}

fn field_named<'e>(entries: &'e [Entry], name: &str) -> Option<&'e Placed> {
    entries.iter().find_map(|entry| match &entry.node {
        Node::Field(field) if field.name == name => Some(field),
        _ => None,
    })
}

impl CompositeNode {
    pub(super) fn param(&self) -> &str {
        &self.param
    }

    /// Its param, as the catalog defines it.
    pub(super) fn param_json(&self) -> (String, Value) {
        let head = self.head;
        let mut param = json!({ "type": if head.quat { "quat" } else { "vec3" },
                                "frame": head.param_frame });
        if let Some(unit) = head.unit {
            param["unit"] = json!(unit);
        }
        (self.param.clone(), param)
    }

    /// The composite as IR JSON.
    pub(super) fn json(&self) -> Value {
        let members: Vec<&str> = self.members.iter().map(|m| m.path.as_str()).collect();
        json!({ "canonical": self.param, "frame": self.head.frame, "members": members })
    }

    /// Its sample (§7.4): its members through their conversions and into
    /// the param's unit when every one is good; else each raw, with the
    /// least trustworthy member's quality.
    pub(super) fn sample(&self, record: &[(String, Raw)]) -> Option<Sample> {
        let mut quality = "good";
        let mut values = Vec::new();
        for member in &self.members {
            let raw = value_at(record, &member.steps)?;
            let (number, converted, member_quality) =
                member_value(&member.field, raw, member.target);
            if rank(member_quality) > rank(quality) {
                quality = member_quality;
            }
            values.push((number, converted));
        }
        let members = values
            .into_iter()
            .map(|(number, converted)| if quality == "good" { converted } else { number })
            .collect();
        let paths: Vec<&str> = self.members.iter().map(|m| m.path.as_str()).collect();
        Some(Sample {
            param: self.param.clone(),
            value: Expected::Vector {
                frame: self.head.frame.to_string(),
                members,
            },
            path: paths.join(","),
            index: None,
            time_offset_ns: None,
            quality,
        })
    }
}

/// The value at a record path.
fn value_at<'r>(record: &'r [(String, Raw)], steps: &[Step]) -> Option<&'r Raw> {
    let (Step::Name(first), rest) = steps.split_first()? else {
        return None;
    };
    let mut value = &record.iter().find(|(name, _)| name == first)?.1;
    for step in rest {
        value = match (step, value) {
            (Step::Index(i), Raw::Array(items)) => items.get(*i)?,
            (Step::Name(name), Raw::Record(fields)) => &fields.iter().find(|(n, _)| n == name)?.1,
            _ => return None,
        };
    }
    Some(value)
}
