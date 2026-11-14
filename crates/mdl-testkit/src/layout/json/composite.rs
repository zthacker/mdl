//! Composites over a JSON packet's entries (`spec/decoding.md` §7.4):
//! their members held as the binary oracle holds them, by fields, a
//! struct, an array or an array of records, but with each entry optional
//! or under a condition now and then, at any pointer, and an array as long
//! as the message makes it, so a member can be absent every way JSON
//! allows. What a composite is and the sample it gives are
//! `layout/composite.rs`'s.

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::{Index, select};

use super::{
    ElementSpec, EntrySpec, JsonElement, JsonEntry, JsonNode, Placing, ShapeSpec, placing,
};
use crate::layout::composite::{
    CompositeNode, CompositeSource, CompositeSpec, Head, Holder, Inside, Step, head, member_spec,
};
use crate::layout::{FieldSpec, Kind, Placed, interval};

/// A JSON member's kind: a number, as a JSON field reads one (§5.3).
fn member_kind() -> BoxedStrategy<Kind> {
    prop_oneof![
        3 => Just(Kind::Uint(64)),
        3 => Just(Kind::Int(64)),
        2 => Just(Kind::Float(64)),
    ]
    .boxed()
}

/// Where a composite's entry is and when: optional or conditional less
/// often than other entries, as any member absent leaves the composite
/// without a sample.
fn member_placing() -> BoxedStrategy<Placing> {
    placing(0.15, 0.1)
}

/// The most records an array of records holding members reaches into.
const RECORDS: usize = 3;

/// A composite, its members held by fields, a struct, an array or an
/// array of records, in any order there.
pub(super) fn composite_source() -> BoxedStrategy<CompositeSource<EntrySpec>> {
    head()
        .prop_flat_map(|head| {
            let n = head.size();
            let members: Vec<BoxedStrategy<FieldSpec>> = (0..n)
                .map(|k| member_spec(&head, k, member_kind(), false))
                .collect();
            let fields: Vec<usize> = (0..n).collect();
            let elements: Vec<u32> = (0..n as u32 + 2).collect();
            let draws = (
                member_spec(&head, 0, member_kind(), false),
                proptest::option::weighted(0.3, interval()),
                vec(member_placing(), n),
                member_placing(),
                Just(fields).prop_shuffle(),
                Just(elements).prop_shuffle(),
                vec(any::<Index>(), n),
            )
                .prop_map(
                    |(element, interval, placings, holder, fields, elements, records)| Draws {
                        element,
                        interval,
                        placings,
                        holder,
                        fields,
                        elements,
                        records,
                    },
                );
            (Just(head), members, select(head.holders()), draws)
        })
        .prop_map(|(head, members, holder, draws)| holding(head, members, holder, draws))
        .boxed()
}

/// What a holder of a composite's members draws: an array's element and
/// interval; where and when each member's own entry is (`placings`), and
/// the holder's (`holder`); field `fields[k]` of a struct or record holds
/// member `k`; an array holds member `k` in its element `elements[k]`,
/// which a message's array may not reach; an array of records holds it in
/// element `records[k]`, one of the first [`RECORDS`].
#[derive(Debug, Clone)]
struct Draws {
    element: FieldSpec,
    interval: Option<f64>,
    placings: Vec<Placing>,
    holder: Placing,
    fields: Vec<usize>,
    elements: Vec<u32>,
    records: Vec<Index>,
}

fn holding(
    head: Head,
    members: Vec<FieldSpec>,
    holder: Holder,
    draws: Draws,
) -> CompositeSource<EntrySpec> {
    let n = members.len();
    let Draws {
        element,
        interval,
        placings,
        holder: placed,
        fields,
        elements,
        records,
    } = draws;
    // A struct or record type's entries: member `k` at position
    // `fields[k]`.
    let inner = |members: Vec<FieldSpec>, placings: Vec<Placing>| {
        let mut entries: Vec<Option<EntrySpec>> = vec![None; n];
        for (k, (member, placing)) in members.into_iter().zip(placings).enumerate() {
            entries[fields[k]] = Some(EntrySpec::new(ShapeSpec::Field(member), placing));
        }
        entries.into_iter().flatten().collect()
    };
    let (items, members): (Vec<EntrySpec>, Vec<(usize, Inside)>) = match holder {
        Holder::Fields => (
            members
                .into_iter()
                .zip(placings)
                .map(|(member, placing)| EntrySpec::new(ShapeSpec::Field(member), placing))
                .collect(),
            (0..n).map(|k| (k, Inside::Whole)).collect(),
        ),
        Holder::Struct => (
            vec![EntrySpec::new(
                ShapeSpec::Struct(inner(members, placings)),
                placed,
            )],
            (0..n).map(|k| (0, Inside::Field(fields[k]))).collect(),
        ),
        Holder::Array => {
            let array = ShapeSpec::Array {
                element: ElementSpec::Scalar(element),
                interval,
            };
            let members = elements
                .iter()
                .take(n)
                .map(|&i| (0, Inside::Element(i)))
                .collect();
            (vec![EntrySpec::new(array, placed)], members)
        }
        Holder::Records => {
            let array = ShapeSpec::Array {
                element: ElementSpec::Record(inner(members, placings)),
                interval: None,
            };
            let members = (0..n)
                .map(|k| {
                    let i = records[k].index(RECORDS) as u32;
                    (0, Inside::Record(i, fields[k]))
                })
                .collect();
            (vec![EntrySpec::new(array, placed)], members)
        }
    };
    CompositeSource {
        head,
        items,
        members,
    }
}

/// A composite's members, found among a packet's laid-out entries by the
/// names their entries took.
pub(super) fn resolve(
    spec: CompositeSpec,
    param: String,
    entries: &[JsonEntry],
) -> Option<CompositeNode> {
    let members = spec
        .members
        .iter()
        .map(|&(item, inside)| {
            let name = format!("f{item}");
            let entry = entries.iter().find(|entry| entry.name == name)?;
            let first = Step::Name(name);
            let (steps, field) = match (&entry.node, inside) {
                (JsonNode::Field(field), Inside::Whole) => (vec![first], field),
                (JsonNode::Struct(ty), Inside::Field(j)) => {
                    let (inner, field) = field_at(&ty.entries, j)?;
                    (vec![first, Step::Name(inner)], field)
                }
                (
                    JsonNode::Array {
                        element: JsonElement::Scalar(field),
                        ..
                    },
                    Inside::Element(i),
                ) => (vec![first, Step::Index(i as usize)], field),
                (
                    JsonNode::Array {
                        element: JsonElement::Record(ty),
                        ..
                    },
                    Inside::Record(i, j),
                ) => {
                    let (inner, field) = field_at(&ty.entries, j)?;
                    (
                        vec![first, Step::Index(i as usize), Step::Name(inner)],
                        field,
                    )
                }
                _ => return None,
            };
            Some((steps, field.clone()))
        })
        .collect::<Option<Vec<(Vec<Step>, Placed)>>>()?;
    Some(CompositeNode::new(param, spec.head, members))
}

/// The field a struct type's entry `j` is, and its name.
fn field_at(entries: &[JsonEntry], j: usize) -> Option<(String, &Placed)> {
    let name = format!("f{j}");
    entries.iter().find_map(|entry| match &entry.node {
        JsonNode::Field(field) if entry.name == name => Some((name.clone(), field)),
        _ => None,
    })
}
