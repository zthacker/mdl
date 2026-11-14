//! Composites (`spec/decoding.md` §7.4), in either encoding: what a vec3
//! or quat param is built from, the items that can hold its members (a
//! field each, a struct, an array, or an array of structs), and the sample
//! a message's record gives. Each oracle lays the holders out among its
//! own entries (`body/composite.rs`, `json/composite.rs`) and finds the
//! members where they went.

use proptest::prelude::*;
use proptest::sample::select;
use serde_json::{Value, json};

use super::meaning::{member_value, rank};
use super::{Expected, FieldSpec, Kind, Placed, Raw, Sample, UNIT_GROUPS, number_in, sentinels};

/// What a composite maps to: a vec3 or quat param in a frame and unit,
/// from a source frame that converts to the param's.
#[derive(Debug, Clone, Copy)]
pub(super) struct Head {
    quat: bool,
    frame: &'static str,
    param_frame: &'static str,
    unit: Option<&'static str>,
}

impl Head {
    pub(super) fn size(&self) -> usize {
        if self.quat { 4 } else { 3 }
    }

    /// The unit member `k` converts into: degrees for a geodetic latitude
    /// or longitude, else the param's.
    pub(super) fn target(&self, k: usize) -> Option<&'static str> {
        if self.frame == "GEODETIC" && k < 2 {
            Some("deg")
        } else {
            self.unit
        }
    }

    /// What can hold its members: any holder, but not an array for a
    /// geodetic position, whose members don't share a unit as an array's
    /// elements do.
    pub(super) fn holders(&self) -> &'static [Holder] {
        if self.frame == "GEODETIC" {
            &[Holder::Fields, Holder::Struct, Holder::Records]
        } else {
            &[
                Holder::Fields,
                Holder::Struct,
                Holder::Array,
                Holder::Records,
            ]
        }
    }
}

/// What holds a composite's members.
#[derive(Debug, Clone, Copy)]
pub(super) enum Holder {
    /// A field each.
    Fields,
    /// Fields of one struct entry.
    Struct,
    /// Elements of an array of numbers.
    Array,
    /// Fields of elements of an array of structs.
    Records,
}

/// Where a composite's member is in an item.
#[derive(Debug, Clone, Copy)]
pub(super) enum Inside {
    /// The item is the member.
    Whole,
    /// Field `j` of a struct entry.
    Field(usize),
    /// Element `i` of an array.
    Element(u32),
    /// Field `j` of element `i` of an array of structs.
    Record(u32, usize),
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

pub(super) fn head() -> BoxedStrategy<Head> {
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

/// Member `k` of a composite with `head`: a number of a kind `kinds`
/// draws, in a unit of the dimension it converts into (in none without
/// one), through any conversion but states. With `flips`, its byte order
/// is now and then its family's other one.
pub(super) fn member_spec(
    head: &Head,
    k: usize,
    kinds: BoxedStrategy<Kind>,
    flips: bool,
) -> BoxedStrategy<FieldSpec> {
    let group = group_of(head.target(k));
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
                    flip: flip && flips,
                    mapped,
                },
            )
        })
        .boxed()
}

/// A composite and the items holding its members, which a packet takes in
/// among its own: a binary body's items, or a JSON packet's entries.
#[derive(Debug, Clone)]
pub(super) struct CompositeSource<I> {
    pub(super) head: Head,
    pub(super) items: Vec<I>,
    /// Each member, in the param's order: one of `items`, and where in it.
    pub(super) members: Vec<(usize, Inside)>,
}

/// A composite, and where its members are among a packet's items.
#[derive(Debug, Clone)]
pub(super) struct CompositeSpec {
    pub(super) head: Head,
    /// Each member, in the param's order: an item of the packet and where
    /// in it.
    pub(super) members: Vec<(usize, Inside)>,
}

/// A packet's items and its composites' items, in `order` (a permutation
/// of them all, the packet's first), with each member found where its
/// item went.
pub(super) fn mix<I>(
    items: Vec<I>,
    sources: Vec<CompositeSource<I>>,
    order: &[usize],
) -> (Vec<I>, Vec<CompositeSpec>) {
    let mut all: Vec<Option<I>> = items.into_iter().map(Some).collect();
    let mut heads = Vec::new();
    for source in sources {
        heads.push((source.head, source.members, all.len()));
        all.extend(source.items.into_iter().map(Some));
    }
    let mut position = vec![0; all.len()];
    for (at, &item) in order.iter().enumerate() {
        position[item] = at;
    }
    let items = order.iter().filter_map(|&item| all[item].take()).collect();
    let composites = heads
        .into_iter()
        .map(|(head, members, first)| CompositeSpec {
            head,
            members: members
                .into_iter()
                .map(|(item, inside)| (position[first + item], inside))
                .collect(),
        })
        .collect();
    (items, composites)
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
pub(super) enum Step {
    Name(String),
    Index(usize),
}

impl CompositeNode {
    /// The composite mapping to `param`, each member the field at the end
    /// of its steps, in the param's order.
    pub(super) fn new(param: String, head: Head, members: Vec<(Vec<Step>, Placed)>) -> Self {
        let members = members
            .into_iter()
            .enumerate()
            .map(|(k, (steps, field))| Member {
                path: path_of(&steps),
                steps,
                field,
                target: head.target(k),
            })
            .collect();
        CompositeNode {
            param,
            head,
            members,
        }
    }

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

    /// For each array holding members, by the name of its entry: how many
    /// elements it needs to hold them all.
    pub(super) fn reads(&self) -> impl Iterator<Item = (&str, usize)> {
        self.members
            .iter()
            .filter_map(|member| match member.steps.as_slice() {
                [Step::Name(name), Step::Index(index), ..] => Some((name.as_str(), index + 1)),
                _ => None,
            })
    }

    /// Its sample (§7.4): its members through their conversions and into
    /// the param's unit when every one is good; else each raw, with the
    /// least trustworthy member's quality. None when a member is absent.
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

/// The value at a record path, if there is one: nothing for an entry left
/// out of the record (absent or skipped), an index past an array's end,
/// or a `null` element, which is no reading (§5.3).
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
    match value {
        Raw::Null => None,
        value => Some(value),
    }
}
