//! Composites over a binary body's entries (`spec/decoding.md` §7.4):
//! generated with the items that hold their members (fields, a struct, an
//! array, or an array of structs), found among the body's laid-out
//! entries. What a composite is and the sample it gives are
//! `layout/composite.rs`'s.

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::{Index, select};

use super::{
    ArraySpec, CountSpec, ElementNode, ElementSpec, Entry, ItemSpec, Node, TypeSpec, WhenSpec,
    interval, when_spec,
};
use crate::layout::composite::{
    CompositeNode, CompositeSource, CompositeSpec, Head, Holder, Inside, Step, head, member_spec,
};
use crate::layout::{FieldSpec, Kind, Placed};

/// A binary member's kind: an integer of any width, or a float.
fn member_kind() -> BoxedStrategy<Kind> {
    prop_oneof![
        3 => (1_u32..=64).prop_map(Kind::Uint),
        3 => (1_u32..=64).prop_map(Kind::Int),
        2 => select(&[32_u32, 64][..]).prop_map(Kind::Float),
    ]
    .boxed()
}

/// A composite, its members held by fields, a struct, an array or an
/// array of structs, in any order there.
pub(super) fn composite_source() -> BoxedStrategy<CompositeSource<ItemSpec>> {
    head()
        .prop_flat_map(|head| {
            let n = head.size();
            let members: Vec<BoxedStrategy<FieldSpec>> = (0..n)
                .map(|k| member_spec(&head, k, member_kind(), true))
                .collect();
            let fields: Vec<usize> = (0..n).collect();
            let elements: Vec<u32> = (0..n + 2).map(|i| i as u32).collect();
            let draws = (
                member_spec(&head, 0, member_kind(), true),
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
            (Just(head), members, select(head.holders()), draws)
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

fn holding(
    head: Head,
    members: Vec<FieldSpec>,
    holder: Holder,
    draws: Draws,
) -> CompositeSource<ItemSpec> {
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
        .map(|&(item, inside)| {
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
            Some((steps, field.clone()))
        })
        .collect::<Option<Vec<(Vec<Step>, Placed)>>>()?;
    Some(CompositeNode::new(param.to_string(), spec.head, members))
}

fn node_name(node: &Node) -> Option<&str> {
    match node {
        Node::Padding(_) => None,
        Node::Field(field) => Some(&field.name),
        Node::Array(array) => Some(&array.name),
        Node::Struct(node) => Some(&node.name),
        Node::Checksum(node) => Some(&node.name),
    }
}

fn field_named<'e>(entries: &'e [Entry], name: &str) -> Option<&'e Placed> {
    entries.iter().find_map(|entry| match &entry.node {
        Node::Field(field) if field.name == name => Some(field),
        _ => None,
    })
}
