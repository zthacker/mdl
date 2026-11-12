//! A packet's body: the fields, arrays and structs after its header and
//! time fields, and the composites over its numbers. Laid out as
//! `spec/decoding.md` §5.2 reads them: entries one after another; arrays
//! of fixed, field (§5.4) and remaining counts, each element starting
//! where the last ended; struct entries and elements read from their own
//! start, of struct types that nest. And the values a message holds in
//! them, written bit by bit (§2.1, §2.2), with the record (§6) and samples
//! (§7.3, §7.4) they decode to.
//!
//! What varies in size (an array counted by a field, or running to the
//! end) is whole bytes, so where each later entry starts within a byte is
//! known, and the alignment rules apply as they do to a fixed layout.

use std::collections::BTreeMap;
use std::iter::once;
use std::ops::RangeInclusive;

use proptest::collection::{btree_set, vec};
use proptest::prelude::*;
use proptest::sample::select;
use proptest::strategy::Union;
use serde_json::{Map, Value, json};

use super::meaning::sample;
use super::{
    Failure, FieldSpec, Kind, Length, Meaning, Placed, Problem, Raw, Sample, Sentinel, Slot, Text,
    encode, field_json, field_spec, fit, inside_one_byte, integer_json, kind, length_field,
    meaning, param_json, put_bits, raw_value, remaining_kind, scalar_json, sentinels, spec_of,
};
use composite::{CompositeNode, CompositeSource, CompositeSpec, composite_source, resolve};
use faults::{Place, Short, Step};

/// The most elements an array whose count varies has in a message.
const MOST_ELEMENTS: u32 = 5;

// ---- What a body is -------------------------------------------------------

/// A packet's body, before it's laid out.
#[derive(Debug, Clone)]
pub(super) struct BodySpec {
    items: Vec<ItemSpec>,
    /// What runs to the packet's end, last (§5.2).
    trailer: Option<Trailer>,
    composites: Vec<CompositeSpec>,
    /// The order the packet lists its composites in, which needn't be
    /// their names' (samples follow the names').
    listed: Vec<usize>,
}

impl BodySpec {
    /// A body of fields alone.
    pub(super) fn fields(fields: Vec<FieldSpec>) -> BodySpec {
        BodySpec {
            items: fields.into_iter().map(ItemSpec::Field).collect(),
            trailer: None,
            composites: Vec::new(),
            listed: Vec::new(),
        }
    }
}

/// An entry of a body or struct type, before it's laid out.
#[derive(Debug, Clone)]
enum ItemSpec {
    Field(FieldSpec),
    Array(ArraySpec),
    /// A struct entry, of a struct type of its own.
    Struct(TypeSpec),
    /// Spare bits: padding of this many.
    Spare(u32),
    /// An item `gap` bits past where it would start, at an explicit
    /// offset, where the position before it is known (§5.2).
    Offset {
        gap: u32,
        item: Box<ItemSpec>,
    },
    /// An item present only when a condition on a selector holds (§5.4).
    Conditional {
        when: WhenSpec,
        item: Box<ItemSpec>,
    },
    /// Two readings of the same bits ([`Alternatives`]).
    Alternatives(Box<Alternatives>),
}

/// Two fields reading the same bits at one offset, at most one of them
/// present: a union a selector picks a reading of. The first is there
/// when `when` holds, the second when `other` does, never both.
#[derive(Debug, Clone)]
struct Alternatives {
    when: WhenSpec,
    other: Test,
    first: FieldSpec,
    second: FieldSpec,
}

/// A condition on a selector field, before it's laid out.
#[derive(Debug, Clone)]
struct WhenSpec {
    selector: Selector,
    /// A new selector field: any meaning a field of its kind can have, so
    /// a condition compares its raw value, not what it converts to (§5.4).
    field: FieldSpec,
    /// Whether it tests the container's last selector, when that's of its
    /// kind.
    share: bool,
    test: Test,
}

/// A selector field's kind: what conditions can compare (§5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Selector {
    Uint(u32),
    Int(u32),
    Bool,
    /// One ASCII letter.
    Text,
}

impl Selector {
    fn kind(self) -> Kind {
        match self {
            Selector::Uint(bits) => Kind::Uint(bits),
            Selector::Int(bits) => Kind::Int(bits),
            Selector::Bool => Kind::Bool,
            Selector::Text => Kind::Text {
                length: Length::Fixed {
                    bytes: 1,
                    terminator: None,
                },
                text: Text::Ascii,
            },
        }
    }
}

/// What runs to the end of the packet.
#[derive(Debug, Clone)]
enum Trailer {
    /// An array of a `remaining` count.
    Array(ArraySpec),
    /// A string or bytes of a `remaining` length.
    Field(FieldSpec),
}

#[derive(Debug, Clone)]
struct ArraySpec {
    count: CountSpec,
    element: ElementSpec,
    /// For mapped scalar elements, sometimes: the time between samples, in
    /// milliseconds.
    interval: Option<f64>,
}

#[derive(Debug, Clone)]
enum CountSpec {
    Fixed(u32),
    /// From a count field of `bits`, as `raw * multiply + add` (§5.4): the
    /// container's last count field if `share` and the arrays can agree on
    /// its values, else a new one right before the array. The field may
    /// mean anything a uint can (a count is its raw value), and have
    /// sentinels.
    Field {
        bits: u32,
        multiply: u32,
        add: i64,
        share: bool,
        meaning: Meaning,
        invalid: Vec<Sentinel>,
        mapped: bool,
    },
    Remaining,
}

#[derive(Debug, Clone)]
enum ElementSpec {
    Scalar(FieldSpec),
    Struct(TypeSpec),
}

/// A struct type: its entries, the first a field.
#[derive(Debug, Clone)]
struct TypeSpec {
    items: Vec<ItemSpec>,
}

/// Any body: fields, arrays, structs and spare bits in any order,
/// sometimes an array, string or bytes that runs to the end, and up to two
/// composites, whose members' items mix with the rest.
pub(super) fn body_spec() -> BoxedStrategy<BodySpec> {
    (
        vec(item_spec(2, true), 1..=6),
        proptest::option::weighted(0.3, trailer_spec()),
        vec(composite_source(), 0..=2),
    )
        .prop_flat_map(|(items, trailer, sources)| {
            let total = items.len() + sources.iter().map(|s| s.items.len()).sum::<usize>();
            let order = Just((0..total).collect::<Vec<usize>>()).prop_shuffle();
            let listed = Just((0..sources.len()).collect::<Vec<usize>>()).prop_shuffle();
            (Just(items), Just(trailer), Just(sources), order, listed)
        })
        .prop_map(|(items, trailer, sources, order, listed)| {
            mix(items, trailer, sources, &order, listed)
        })
        .boxed()
}

/// The body's items and its composites' items, in `order` (a permutation
/// of them all, the body's first), with each member found where its item
/// went.
fn mix(
    items: Vec<ItemSpec>,
    trailer: Option<Trailer>,
    sources: Vec<CompositeSource>,
    order: &[usize],
    listed: Vec<usize>,
) -> BodySpec {
    let mut all: Vec<Option<ItemSpec>> = items.into_iter().map(Some).collect();
    let mut firsts = Vec::new();
    for source in &sources {
        firsts.push(all.len());
        all.extend(source.items.iter().cloned().map(Some));
    }
    let mut position = vec![0; all.len()];
    for (at, &item) in order.iter().enumerate() {
        position[item] = at;
    }
    let items = order.iter().filter_map(|&item| all[item].take()).collect();
    let composites = sources
        .into_iter()
        .zip(firsts)
        .map(|(source, first)| source.placed(&position[first..]))
        .collect();
    BodySpec {
        items,
        trailer,
        composites,
        listed,
    }
}

/// An entry of a body or struct type: struct entries and arrays of
/// structs only while `depth` allows another level; now and then under a
/// condition. At a packet's `top` level, also at an offset (a conditional
/// one too), or a pair of alternative readings. In a struct type an entry
/// with an offset makes the struct end, for what follows, wherever any of
/// its entries can (§5.2), so offsets there come only in fixed records
/// ([`record_spec`]), whose size is exact.
fn item_spec(depth: u32, top: bool) -> BoxedStrategy<ItemSpec> {
    let mut items = vec![
        (6, field_spec().prop_map(ItemSpec::Field).boxed()),
        (4, array_spec(depth).prop_map(ItemSpec::Array).boxed()),
        (2, (1_u32..=16).prop_map(ItemSpec::Spare).boxed()),
    ];
    if depth > 0 {
        items.push((2, type_spec(depth - 1).prop_map(ItemSpec::Struct).boxed()));
    }
    let plain = Union::new_weighted(items).boxed();
    let offset = (0_u32..=24, plain.clone()).prop_map(|(gap, item)| ItemSpec::Offset {
        gap,
        item: Box::new(item),
    });
    let gap = if top {
        proptest::option::weighted(0.4, 0_u32..=16).boxed()
    } else {
        Just(None).boxed()
    };
    let conditional = (when_spec(), plain.clone(), gap).prop_map(|(when, item, gap)| {
        let item = match gap {
            Some(gap) => ItemSpec::Offset {
                gap,
                item: Box::new(item),
            },
            None => item,
        };
        ItemSpec::Conditional {
            when,
            item: Box::new(item),
        }
    });
    if !top {
        return prop_oneof![10 => plain, 3 => conditional].boxed();
    }
    prop_oneof![
        10 => plain,
        2 => offset,
        3 => conditional,
        1 => alternatives_spec(),
    ]
    .boxed()
}

/// A condition on a selector of any kind, new or the container's last.
fn when_spec() -> BoxedStrategy<WhenSpec> {
    let selector = prop_oneof![
        3 => (2_u32..=6).prop_map(Selector::Uint),
        2 => (2_u32..=6).prop_map(Selector::Int),
        1 => Just(Selector::Bool),
        1 => Just(Selector::Text),
    ];
    (selector, proptest::bool::weighted(0.4))
        .prop_flat_map(|(selector, share)| {
            (spec_of(Just(selector.kind()).boxed()), test_for(selector)).prop_map(
                move |(field, test)| WhenSpec {
                    selector,
                    field,
                    share,
                    test,
                },
            )
        })
        .boxed()
}

/// A test a selector of this kind can be put to: a comparison, mostly,
/// else `not` one, or `all` or `any` of two.
fn test_for(selector: Selector) -> BoxedStrategy<Test> {
    let leaf = leaf_for(selector);
    prop_oneof![
        4 => leaf.clone(),
        1 => leaf.clone().prop_map(|test| Test::Not(Box::new(test))),
        1 => vec(leaf.clone(), 2).prop_map(Test::All),
        1 => vec(leaf, 2).prop_map(Test::Any),
    ]
    .boxed()
}

/// A comparison a selector of this kind takes (validation's rules): any
/// on an unsigned integer, no mask on a signed one, equality on a bool or
/// text; or `in` with values it holds.
fn leaf_for(selector: Selector) -> BoxedStrategy<Test> {
    let numbers = |values: RangeInclusive<i128>, ops: &'static [Op]| {
        prop_oneof![
            3 => (select(ops), values.clone())
                .prop_map(|(op, n)| Test::Compare(op, Operand::Int(n))),
            1 => btree_set(values, 1..=3)
                .prop_map(|set| Test::In(set.into_iter().map(Operand::Int).collect())),
        ]
        .boxed()
    };
    let all = &[
        Op::Eq,
        Op::Ne,
        Op::Lt,
        Op::Le,
        Op::Gt,
        Op::Ge,
        Op::MaskAny,
        Op::MaskAll,
    ];
    match selector {
        Selector::Uint(bits) => numbers(0..=(1 << bits) - 1, all),
        Selector::Int(bits) => numbers(-(1 << (bits - 1))..=(1 << (bits - 1)) - 1, &all[..6]),
        Selector::Bool => numbers(0..=1, &all[..2]),
        Selector::Text => {
            let letter = select(&["a", "b", "c", "d"][..]).prop_map(|t| Operand::Text(t.into()));
            prop_oneof![
                3 => (select(&all[..2]), letter.clone())
                    .prop_map(|(op, text)| Test::Compare(op, text)),
                1 => vec(letter, 1..=2).prop_map(Test::In),
            ]
            .boxed()
        }
    }
}

/// Kinds of a fixed size: numbers, and text and bytes of a fixed length.
fn fixed_kind() -> BoxedStrategy<Kind> {
    prop_oneof![
        3 => (1_u32..=64).prop_map(Kind::Uint),
        3 => (1_u32..=64).prop_map(Kind::Int),
        2 => select(&[32_u32, 64][..]).prop_map(Kind::Float),
        1 => (1_u32..=8).prop_map(|bytes| Kind::Text {
            length: Length::Fixed { bytes, terminator: None },
            text: Text::Ascii,
        }),
        1 => (1_u32..=8).prop_map(|bytes| Kind::Bytes(Length::Fixed { bytes, terminator: None })),
    ]
    .boxed()
}

/// A fixed record: one to four fields of a fixed size (bools too), each
/// after the first at an offset 0 to 16 bits past where it would start.
/// Its size is exact, so a struct of it ends at one place within a byte.
fn record_spec() -> BoxedStrategy<TypeSpec> {
    let kinds = prop_oneof![4 => fixed_kind(), 1 => Just(Kind::Bool)].boxed();
    (
        spec_of(kinds.clone()),
        vec((0_u32..=16, spec_of(kinds)), 0..=3),
    )
        .prop_map(|(first, rest)| TypeSpec {
            items: once(ItemSpec::Field(first))
                .chain(rest.into_iter().map(|(gap, spec)| ItemSpec::Offset {
                    gap,
                    item: Box::new(ItemSpec::Field(spec)),
                }))
                .collect(),
        })
        .boxed()
}

/// Two fixed-size fields, no bools, reading the same bits: under a test
/// and its `not`, or, on an unsigned selector, equal to two values.
fn alternatives_spec() -> BoxedStrategy<ItemSpec> {
    let kinds = fixed_kind();
    (
        when_spec(),
        spec_of(kinds.clone()),
        spec_of(kinds),
        any::<bool>(),
        any::<u64>(),
    )
        .prop_map(|(mut when, first, second, values, picked)| {
            let other = match when.selector {
                Selector::Uint(bits) if values => {
                    let a = i128::from(picked % (1 << bits));
                    let b = (a + 1) % (1 << bits);
                    when.test = Test::Compare(Op::Eq, Operand::Int(a));
                    Test::Compare(Op::Eq, Operand::Int(b))
                }
                _ => Test::Not(Box::new(when.test.clone())),
            };
            ItemSpec::Alternatives(Box::new(Alternatives {
                when,
                other,
                first,
                second,
            }))
        })
        .boxed()
}

/// An array of a fixed count (now and then none) or one from a field.
fn array_spec(depth: u32) -> BoxedStrategy<ArraySpec> {
    let count = prop_oneof![
        3 => prop_oneof![1 => Just(0_u32), 8 => 1_u32..=4].prop_map(CountSpec::Fixed),
        2 => count_field(),
    ];
    count
        .prop_flat_map(move |count| {
            let varying = !matches!(count, CountSpec::Fixed(_));
            (Just(count), element_spec(depth, varying))
        })
        .prop_flat_map(|(count, element)| (Just(count), interval_for(&element), Just(element)))
        .prop_map(|(count, interval, element)| ArraySpec {
            count,
            element,
            interval,
        })
        .boxed()
}

/// An array, string or bytes that runs to the end of the packet.
fn trailer_spec() -> BoxedStrategy<Trailer> {
    let array = element_spec(1, true)
        .prop_flat_map(|element| (interval_for(&element), Just(element)))
        .prop_map(|(interval, element)| {
            Trailer::Array(ArraySpec {
                count: CountSpec::Remaining,
                element,
                interval,
            })
        });
    // Terminated strings and bytes, often: one more element of them, cut
    // short, finds no terminator.
    let kinds = prop_oneof![
        3 => (select(&[Text::Utf8, Text::Ascii, Text::Latin1][..]), any::<u8>())
            .prop_map(|(text, byte)| Kind::Text { length: Length::Terminated(byte), text }),
        1 => any::<u8>().prop_map(|byte| Kind::Bytes(Length::Terminated(byte))),
    ];
    let terminated = spec_of(kinds.boxed()).prop_map(|spec| {
        Trailer::Array(ArraySpec {
            count: CountSpec::Remaining,
            element: ElementSpec::Scalar(spec),
            interval: None,
        })
    });
    prop_oneof![
        3 => array,
        1 => terminated,
        1 => spec_of(remaining_kind()).prop_map(Trailer::Field),
    ]
    .boxed()
}

/// A count field: 2 to 16 bits, scaled now and then. An array that would
/// share the last one takes a negative `add` a third of the time, so a
/// raw value can give the arrays before it a count and it none.
fn count_field() -> BoxedStrategy<CountSpec> {
    (
        2_u32..=16,
        prop_oneof![3 => Just(1_u32), 1 => 2_u32..=3],
        proptest::bool::weighted(0.6),
        any::<bool>(),
    )
        .prop_flat_map(|(bits, multiply, share, mapped)| {
            let add = if share {
                prop_oneof![Just(0_i64), -2_i64..=-1, 1_i64..=2].boxed()
            } else {
                prop_oneof![3 => Just(0_i64), 1 => -2_i64..=2].boxed()
            };
            let kind = Kind::Uint(bits);
            (add, meaning(&kind), sentinels(&kind)).prop_map(move |(add, meaning, invalid)| {
                CountSpec::Field {
                    bits,
                    multiply,
                    add,
                    share,
                    meaning,
                    invalid,
                    mapped,
                }
            })
        })
        .boxed()
}

/// An array's element: a scalar, not a bool if the count varies (a varying
/// count keeps whole bytes, and a bool is one bit), or a struct while
/// `depth` allows another level.
fn element_spec(depth: u32, varying: bool) -> BoxedStrategy<ElementSpec> {
    // Integers narrower than a byte, often: bit-packed elements, and
    // little-endian ones that must each sit inside a byte.
    let kinds = prop_oneof![
        3 => kind(Slot::Element { varying }),
        1 => (1_u32..=7).prop_map(Kind::Uint),
        1 => (1_u32..=7).prop_map(Kind::Int),
    ];
    let scalar = spec_of(kinds.boxed()).prop_map(ElementSpec::Scalar);
    if depth == 0 {
        return scalar.boxed();
    }
    prop_oneof![
        2 => scalar,
        1 => type_spec(depth - 1).prop_map(ElementSpec::Struct),
    ]
    .boxed()
}

/// A struct type: a field, then up to three entries more; or a fixed
/// record.
fn type_spec(depth: u32) -> BoxedStrategy<TypeSpec> {
    let general =
        (field_spec(), vec(item_spec(depth, false), 0..=3)).prop_map(|(first, rest)| TypeSpec {
            items: once(ItemSpec::Field(first)).chain(rest).collect(),
        });
    prop_oneof![4 => general, 1 => record_spec()].boxed()
}

/// An interval between samples, sometimes, for mapped scalar elements.
fn interval_for(element: &ElementSpec) -> BoxedStrategy<Option<f64>> {
    match element {
        ElementSpec::Scalar(spec) if spec.mapped => {
            proptest::option::weighted(0.35, interval()).boxed()
        }
        _ => Just(None).boxed(),
    }
}

/// The time between a block's samples, in milliseconds: some at half a
/// nanosecond's tie (2.5e-6 puts element 1 at 2.5 ns, which rounds to 3),
/// and any from a microsecond to ten seconds.
fn interval() -> BoxedStrategy<f64> {
    prop_oneof![
        1 => select(&[5e-7, 1.5e-6, 2.5e-6, 0.125, 1.0, 1000.0 / 3.0][..]),
        2 => 1e-3..1e4_f64,
    ]
    .boxed()
}

// ---- Laid out -------------------------------------------------------------

/// A body's or struct type's entry, laid out: where it is when it says,
/// when it's present, and what it reads.
#[derive(Debug, Clone)]
pub(super) struct Entry {
    /// Its `offset_bits`, from its container's start: a packet's, for a
    /// body's entries.
    offset: Option<u64>,
    /// Present only when this holds (§5.4).
    when: Option<Condition>,
    node: Node,
}

impl Entry {
    /// Whether its size, or whether it's there, varies between messages.
    fn varies(&self) -> bool {
        self.when.is_some()
            || node_bits(&self.node, Bound::Least) != node_bits(&self.node, Bound::Most)
    }
}

/// What an entry reads.
#[derive(Debug, Clone)]
pub(super) enum Node {
    Padding(u64),
    Field(Placed),
    Array(ArrayNode),
    Struct(StructNode),
}

/// A condition (§5.4) on a selector field of the entry's container: every
/// comparison in it is on that field's raw value.
#[derive(Debug, Clone)]
pub(super) struct Condition {
    field: String,
    test: Test,
}

impl Condition {
    /// Whether it holds for its container's values.
    fn holds(&self, record: &[(String, Raw)]) -> bool {
        record
            .iter()
            .find(|(name, _)| *name == self.field)
            .is_some_and(|(_, raw)| self.test.holds(raw))
    }
}

/// What a condition asks of its field's raw value.
#[derive(Debug, Clone)]
enum Test {
    Compare(Op, Operand),
    In(Vec<Operand>),
    Not(Box<Test>),
    All(Vec<Test>),
    Any(Vec<Test>),
}

impl Test {
    fn holds(&self, raw: &Raw) -> bool {
        match self {
            Test::Compare(op, operand) => compare(raw, *op, operand),
            Test::In(operands) => operands.iter().any(|operand| compare(raw, Op::Eq, operand)),
            Test::Not(test) => !test.holds(raw),
            Test::All(tests) => tests.iter().all(|test| test.holds(raw)),
            Test::Any(tests) => tests.iter().any(|test| test.holds(raw)),
        }
    }

    /// Every operand it compares with, for a selector's values.
    fn operands(&self, out: &mut Vec<(Op, Operand)>) {
        match self {
            Test::Compare(op, operand) => out.push((*op, operand.clone())),
            Test::In(operands) => out.extend(operands.iter().map(|o| (Op::Eq, o.clone()))),
            Test::Not(test) => test.operands(out),
            Test::All(tests) | Test::Any(tests) => {
                for test in tests {
                    test.operands(out);
                }
            }
        }
    }
}

/// A comparison (§5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// `field & value != 0`
    MaskAny,
    /// `field & value == value`
    MaskAll,
}

impl Op {
    /// Its name, as the IR spells it.
    fn name(self) -> &'static str {
        match self {
            Op::Eq => "eq",
            Op::Ne => "ne",
            Op::Lt => "lt",
            Op::Le => "le",
            Op::Gt => "gt",
            Op::Ge => "ge",
            Op::MaskAny => "mask_any",
            Op::MaskAll => "mask_all",
        }
    }
}

/// What a condition compares a field with: an integer (a bool is 0 or 1),
/// or text, exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Operand {
    Int(i128),
    Text(String),
}

/// A comparison of a raw value with an operand (§5.4): by value, a bool as
/// 0 or 1; text only for equality.
fn compare(raw: &Raw, op: Op, operand: &Operand) -> bool {
    let value = match (raw, operand) {
        (Raw::Str(text), Operand::Text(expected)) => {
            return match op {
                Op::Eq => text == expected,
                Op::Ne => text != expected,
                _ => false,
            };
        }
        (Raw::Uint(n), Operand::Int(_)) => i128::from(*n),
        (Raw::Int(n), Operand::Int(_)) => i128::from(*n),
        (Raw::Bool(b), Operand::Int(_)) => i128::from(*b),
        _ => return false,
    };
    let Operand::Int(expected) = *operand else {
        return false;
    };
    match op {
        Op::Eq => value == expected,
        Op::Ne => value != expected,
        Op::Lt => value < expected,
        Op::Le => value <= expected,
        Op::Gt => value > expected,
        Op::Ge => value >= expected,
        Op::MaskAny => value & expected != 0,
        Op::MaskAll => value & expected == expected,
    }
}

/// The entries of a container that its values say are present, each with
/// its value: padding and absent entries left out.
fn present<'e>(
    entries: &'e [Entry],
    record: &'e [(String, Raw)],
) -> impl Iterator<Item = (&'e Entry, &'e (String, Raw))> {
    entries
        .iter()
        .filter(|entry| !matches!(entry.node, Node::Padding(_)))
        .filter(move |entry| entry.when.as_ref().is_none_or(|when| when.holds(record)))
        .zip(record)
}

#[derive(Debug, Clone)]
pub(super) struct ArrayNode {
    name: String,
    count: Counted,
    element: ElementNode,
    interval: Option<f64>,
}

#[derive(Debug, Clone)]
enum Counted {
    Fixed(u32),
    Field {
        field: String,
        multiply: u32,
        add: i64,
    },
    Remaining,
}

#[derive(Debug, Clone)]
enum ElementNode {
    Scalar(Placed),
    Struct(TypeNode),
}

/// A struct type, laid out from its own start.
#[derive(Debug, Clone)]
pub(super) struct TypeNode {
    name: String,
    entries: Vec<Entry>,
}

#[derive(Debug, Clone)]
pub(super) struct StructNode {
    name: String,
    ty: TypeNode,
}

/// A packet's body, laid out.
#[derive(Debug, Clone)]
pub(super) struct Body {
    /// Where it starts, in every message.
    pub(super) start: u64,
    pub(super) entries: Vec<Entry>,
    /// Its struct types, for the set.
    types: Vec<TypeNode>,
    composites: Vec<CompositeNode>,
    /// The order the packet lists its composites in.
    listed: Vec<usize>,
    /// The furthest bit a message reaches.
    pub(super) most_bits: u64,
}

/// How a body's params and struct types are named, and the types laid
/// out so far.
struct Names {
    packet: String,
    params: String,
    types: Vec<TypeNode>,
}

/// Lays a body out from bit `start`, in a family of the given byte order.
/// Its entries are named `f0`, `f1` and on, and their params and struct
/// types after the packet's.
pub(super) fn lay_out(
    spec: BodySpec,
    little: bool,
    start: u64,
    packet: &str,
    params: &str,
) -> Body {
    let mut names = Names {
        packet: packet.to_string(),
        params: params.to_string(),
        types: Vec::new(),
    };
    let mut placer = Placer::new(little, start, params.to_string());
    for (position, item) in spec.items.into_iter().enumerate() {
        placer.item(format!("f{position}"), item, &mut names);
    }
    match spec.trailer {
        Some(Trailer::Array(trailer)) => placer.array("rest".into(), trailer, None, &mut names),
        Some(Trailer::Field(trailer)) => placer.field("rest".into(), trailer, None),
        None => {}
    }
    let entries = placer.entries;
    let composites = spec
        .composites
        .into_iter()
        .enumerate()
        .filter_map(|(index, composite)| {
            resolve(composite, &format!("{params}.c{index}"), &entries)
        })
        .collect();
    let most_bits = extent(&entries, 0, start, Bound::Most);
    Body {
        start,
        entries,
        types: names.types,
        composites,
        listed: spec.listed,
        most_bits,
    }
}

/// Places entries one after another (§5.2), or at offsets. `at` is where
/// the next entry starts, exactly while `exact` holds: until something
/// whose size varies, or that may be absent. After that only `at` modulo 8
/// is known, which is exact anyway, since what varies is whole bytes.
struct Placer {
    little: bool,
    at: u64,
    /// Whether `at` is where the next entry starts in every message.
    exact: bool,
    /// Whether the next entry is placed at an explicit offset: `at`.
    pending_offset: bool,
    /// What this container's params' names start with.
    params: String,
    entries: Vec<Entry>,
    counts: Vec<CountField>,
    /// The container's selector fields, and their kinds.
    selectors: Vec<(String, Selector)>,
}

/// A container's count field, and the scales, `(multiply, add)`, of the
/// arrays it counts.
struct CountField {
    name: String,
    bits: u32,
    scales: Vec<(u32, i64)>,
}

impl Placer {
    fn new(little: bool, at: u64, params: String) -> Placer {
        Placer {
            little,
            at,
            exact: true,
            pending_offset: false,
            params,
            entries: Vec::new(),
            counts: Vec::new(),
            selectors: Vec::new(),
        }
    }

    /// Adds an entry at `at`, which it doesn't move: at an explicit offset
    /// if one is pending, and under `when`. A pending offset goes on the
    /// first entry an item adds, which may be its selector, count or
    /// length field or padding: the item's entries move together. A
    /// conditional entry is whole bytes, so what follows starts at one
    /// place within a byte whether it's there or not; one that can't be is
    /// always there.
    fn push(&mut self, node: Node, when: Option<Condition>) {
        let whole = node_bits(&node, Bound::Least).is_multiple_of(8)
            && node_bits(&node, Bound::Most).is_multiple_of(8);
        let entry = Entry {
            offset: std::mem::take(&mut self.pending_offset).then_some(self.at),
            when: when.filter(|_| whole),
            node,
        };
        if entry.varies() {
            self.exact = false;
        }
        self.entries.push(entry);
    }

    fn pad_to_byte(&mut self) {
        let skip = (8 - self.at % 8) % 8;
        if skip > 0 {
            self.push(Node::Padding(skip), None);
            self.at += skip;
        }
    }

    fn item(&mut self, name: String, item: ItemSpec, names: &mut Names) {
        self.place(name, item, None, names);
    }

    /// Places an item, under `when` if given.
    fn place(&mut self, name: String, item: ItemSpec, when: Option<Condition>, names: &mut Names) {
        match item {
            ItemSpec::Field(spec) => self.field(name, spec, when),
            ItemSpec::Array(spec) => self.array(name, spec, when, names),
            ItemSpec::Struct(spec) => {
                let whole = when.is_some();
                let ty = struct_type(self.little, spec, false, false, whole, names);
                if needs_byte(&ty.entries) {
                    self.pad_to_byte();
                }
                let bits = least_bits(&ty.entries);
                self.push(Node::Struct(StructNode { name, ty }), when);
                self.at += bits;
            }
            ItemSpec::Spare(bits) => {
                let bits = if when.is_some() {
                    bits.div_ceil(8) * 8
                } else {
                    bits
                };
                self.push(Node::Padding(u64::from(bits)), when);
                self.at += u64::from(bits);
            }
            // Only where the position before it is known: an offset past
            // what may vary would have to clear its largest size.
            ItemSpec::Offset { gap, item } => {
                if self.exact {
                    self.at += u64::from(gap);
                    self.pending_offset = true;
                }
                self.place(name, *item, when, names);
                self.pending_offset = false;
            }
            ItemSpec::Conditional { when: spec, item } => {
                let field = self.selector(&name, spec.selector, spec.field, spec.share);
                let condition = Condition {
                    field,
                    test: spec.test,
                };
                self.place(name, *item, Some(condition), names);
            }
            ItemSpec::Alternatives(pair) => {
                let Alternatives {
                    when,
                    other,
                    first,
                    second,
                } = *pair;
                self.alternatives(&name, when, other, first, second);
            }
        }
    }

    /// The selector a condition tests: the container's last, if `share`
    /// and it's of this kind; else a new field, named after the item,
    /// right before it.
    fn selector(&mut self, item: &str, selector: Selector, spec: FieldSpec, share: bool) -> String {
        if share
            && let Some((name, kind)) = self.selectors.last()
            && *kind == selector
        {
            return name.clone();
        }
        let name = format!("{item}_sel");
        self.field(name.clone(), spec, None);
        self.selectors.push((name.clone(), selector));
        name
    }

    /// Two fields at one offset on a byte, the first present under the
    /// test, the second under `other`, and what follows at an offset past
    /// both: the second, absent, would end where it started. Where the
    /// position isn't known, the first alone, under its test.
    fn alternatives(
        &mut self,
        name: &str,
        spec: WhenSpec,
        other: Test,
        first: FieldSpec,
        second: FieldSpec,
    ) {
        let field = self.selector(name, spec.selector, spec.field, spec.share);
        let first_when = Condition {
            field: field.clone(),
            test: spec.test,
        };
        if !self.exact {
            self.field(format!("{name}_a"), first, Some(first_when));
            return;
        }
        self.pad_to_byte();
        let start = self.at;
        self.pending_offset = true;
        self.field(format!("{name}_a"), first, Some(first_when));
        let first_end = self.at;
        self.at = start;
        self.pending_offset = true;
        let second_when = Condition { field, test: other };
        self.field(format!("{name}_b"), second, Some(second_when));
        self.at = self.at.max(first_end);
        self.pending_offset = true;
        self.exact = true;
    }

    /// A field: strings and bytes start on a byte, after the field that
    /// holds their length if one does; little-endian numbers that span
    /// bytes are whole bytes on byte boundaries (widened if need be); a
    /// byte order is spelled only where it differs from the family's and
    /// matters (§2.1, §2.2, §5.4).
    fn field(&mut self, name: String, spec: FieldSpec, when: Option<Condition>) {
        if let Some(Length::Field { bits, .. }) = spec.kind.length() {
            let length = FieldSpec {
                kind: Kind::Uint(bits),
                meaning: Meaning::U64,
                invalid: Vec::new(),
                flip: false,
                mapped: false,
            };
            self.field(length_field(&name), length, None);
        }
        let param = spec.mapped.then(|| format!("{}.{name}", self.params));
        let FieldSpec {
            mut kind,
            mut meaning,
            invalid,
            flip,
            ..
        } = spec;
        // A conditional number is whole bytes (see `push`).
        if when.is_some() && kind.ordered() && kind.bits() % 8 != 0 {
            (kind, meaning) = widened(&kind, meaning);
        }
        let mut little = self.little;
        let mut explicit = false;
        if flip && kind.ordered() {
            little = !self.little;
            explicit = true;
        }
        let ordered_little = little && kind.ordered();
        if kind.on_a_byte() || (ordered_little && !inside_one_byte(self.at, kind.bits())) {
            self.pad_to_byte();
        }
        if ordered_little && !inside_one_byte(self.at, kind.bits()) && kind.bits() % 8 != 0 {
            (kind, meaning) = widened(&kind, meaning);
        }
        // Inside one byte the order doesn't apply, so it isn't spelled.
        if explicit && inside_one_byte(self.at, kind.bits()) {
            explicit = false;
            little = self.little;
        }
        let bits = kind.bits();
        self.push(
            Node::Field(Placed {
                name,
                param,
                kind,
                meaning,
                invalid,
                little,
                explicit,
            }),
            when,
        );
        self.at += u64::from(bits);
    }

    /// An array (§5.2), after its count field. A remaining count starts it
    /// on a byte, since it reads to the packet's end. A conditional one has
    /// whole-byte elements, as one whose count varies does.
    fn array(&mut self, name: String, spec: ArraySpec, when: Option<Condition>, names: &mut Names) {
        let count = match spec.count {
            CountSpec::Fixed(count) => Counted::Fixed(count),
            CountSpec::Remaining => {
                self.pad_to_byte();
                Counted::Remaining
            }
            CountSpec::Field {
                bits,
                multiply,
                add,
                share,
                meaning,
                invalid,
                mapped,
            } => Counted::Field {
                field: self.count_field(
                    &name,
                    (multiply, add),
                    share,
                    FieldSpec {
                        kind: Kind::Uint(bits),
                        meaning,
                        invalid,
                        flip: false,
                        mapped,
                    },
                ),
                multiply,
                add,
            },
        };
        let fixed = match count {
            Counted::Fixed(count) => Some(count),
            Counted::Field { .. } | Counted::Remaining => None,
        };
        let shaped = fixed.filter(|_| when.is_none());
        let element = match spec.element {
            ElementSpec::Scalar(spec) => ElementNode::Scalar(self.element(&name, spec, shaped)),
            ElementSpec::Struct(spec) => {
                let ty = struct_type(self.little, spec, true, shaped.is_none(), false, names);
                if needs_byte(&ty.entries) {
                    self.pad_to_byte();
                }
                ElementNode::Struct(ty)
            }
        };
        let bits = fixed.map_or(0, |count| {
            u64::from(count) * element_bits(&element, Bound::Least)
        });
        self.push(
            Node::Array(ArrayNode {
                name,
                count,
                element,
                interval: spec.interval,
            }),
            when,
        );
        self.at += bits;
    }

    /// A scalar element, its array starting at the next position. Its byte
    /// order is spelled only at 16 bits and more, which never sit inside
    /// one byte. A varying count, or a little-endian number that can't sit
    /// inside one byte wherever an element starts, takes whole bytes; text,
    /// bytes and little-endian whole bytes start the array on a byte.
    fn element(&mut self, array: &str, spec: FieldSpec, fixed: Option<u32>) -> Placed {
        let param = spec.mapped.then(|| format!("{}.{array}", self.params));
        let FieldSpec {
            mut kind,
            mut meaning,
            invalid,
            flip,
            ..
        } = spec;
        let mut little = self.little;
        let mut explicit = false;
        if flip && kind.ordered() && kind.bits() >= 16 {
            little = !little;
            explicit = true;
        }
        let bits = kind.bits();
        if kind.ordered() && bits % 8 != 0 {
            let widen = match fixed {
                None => true,
                Some(_) if !little => false,
                Some(_) if bits > 8 => true,
                Some(count) => {
                    if !narrow_fits(self.at, bits, count) {
                        self.pad_to_byte();
                    }
                    !narrow_fits(self.at, bits, count)
                }
            };
            if widen {
                (kind, meaning) = widened(&kind, meaning);
            }
        }
        if kind.on_a_byte() || (kind.ordered() && little && kind.bits() % 8 == 0) {
            self.pad_to_byte();
        }
        Placed {
            name: array.to_string(),
            param,
            kind,
            meaning,
            invalid,
            little,
            explicit,
        }
    }

    /// The count field for an array: the container's last, if `share` and
    /// it can hold values that give every array it counts, this one too, a
    /// count of 0 to [`MOST_ELEMENTS`]; else a new one, named after the
    /// array, right before it.
    fn count_field(
        &mut self,
        array: &str,
        scale: (u32, i64),
        share: bool,
        spec: FieldSpec,
    ) -> String {
        if share && let Some(last) = self.counts.last_mut() {
            let mut scales = last.scales.clone();
            scales.push(scale);
            if raw_range(last.bits, &scales).is_some() {
                last.scales = scales;
                return last.name.clone();
            }
        }
        let name = format!("{array}_n");
        let bits = spec.kind.bits();
        self.field(name.clone(), spec, None);
        // Placed in a little-endian family, it may have been widened.
        let bits = match self.entries.last().map(|entry| &entry.node) {
            Some(Node::Field(field)) => field.kind.bits(),
            _ => bits,
        };
        self.counts.push(CountField {
            name: name.clone(),
            bits,
            scales: vec![scale],
        });
        name
    }
}

/// The same kind of integer in whole bytes, and its meaning. Widening
/// keeps every value, state, point and sentinel it held, but an i64 param
/// takes a uint of at most 63 bits.
fn widened(kind: &Kind, meaning: Meaning) -> (Kind, Meaning) {
    let kind = kind.widened(kind.bits().div_ceil(8) * 8);
    let meaning = if kind == Kind::Uint(64) && meaning == Meaning::I64 {
        Meaning::U64
    } else {
        meaning
    };
    (kind, meaning)
}

/// Whether little-endian elements of `bits`, under a byte, each sit inside
/// one byte from `start` on, `count` of them (§5.2 checks element 0 even
/// for a count of 0).
fn narrow_fits(start: u64, bits: u32, count: u32) -> bool {
    (0..u64::from(count.max(1))).all(|k| inside_one_byte(start + k * u64::from(bits), bits))
}

/// A struct type laid out from its own start (§5.2: places within a byte
/// inside it count from 0), named and kept for the set. As an array's
/// element it's padded to whole bytes when it needs a byte start (each
/// element does then), when the count varies, or when its own size does:
/// §5.2 counts every residue after elements whose size varies, unless
/// they're whole bytes. A conditional struct is padded to whole bytes too
/// (`whole`).
fn struct_type(
    little: bool,
    spec: TypeSpec,
    element: bool,
    varying: bool,
    whole: bool,
    names: &mut Names,
) -> TypeNode {
    let index = names.types.len();
    let name = format!("{}_t{index}", names.packet);
    // Its place is kept while the types inside it are laid out.
    names.types.push(TypeNode {
        name: name.clone(),
        entries: Vec::new(),
    });
    let mut inner = Placer::new(little, 0, format!("{}.t{index}", names.params));
    for (position, item) in spec.items.into_iter().enumerate() {
        inner.item(format!("f{position}"), item, names);
    }
    let sized = least_bits(&inner.entries) == most_bits(&inner.entries);
    if whole || (element && (varying || !sized || needs_byte(&inner.entries))) {
        inner.pad_to_byte();
    }
    let ty = TypeNode {
        name,
        entries: inner.entries,
    };
    names.types[index] = ty.clone();
    ty
}

/// Whether a struct of these entries starts on a byte (§5.2): it holds,
/// directly, in an array or in a nested struct, a string, bytes, a
/// little-endian value, or a value of at most 8 bits that names its own
/// byte order. A bool has no byte order (§2.1), so it doesn't count.
fn needs_byte(entries: &[Entry]) -> bool {
    let scalar = |field: &Placed| {
        field.kind.on_a_byte()
            || (field.kind.ordered()
                && (field.little || (field.explicit && field.kind.bits() <= 8)))
    };
    entries.iter().any(|entry| match &entry.node {
        Node::Padding(_) => false,
        Node::Field(field) => scalar(field),
        Node::Array(array) => match &array.element {
            ElementNode::Scalar(element) => scalar(element),
            ElementNode::Struct(ty) => needs_byte(&ty.entries),
        },
        Node::Struct(node) => needs_byte(&node.ty.entries),
    })
}

/// Which end of a size that varies.
#[derive(Debug, Clone, Copy)]
enum Bound {
    /// Every count that varies at 0, every length at its least, every
    /// conditional entry absent.
    Least,
    /// Every count that varies at [`MOST_ELEMENTS`], every length at the
    /// most a message gives it, every conditional entry present.
    Most,
}

/// The fewest bits a struct type's entries take.
fn least_bits(entries: &[Entry]) -> u64 {
    extent(entries, 0, 0, Bound::Least)
}

/// The most bits a struct type's entries take in a message.
fn most_bits(entries: &[Entry]) -> u64 {
    extent(entries, 0, 0, Bound::Most)
}

/// The furthest end of a container's entries at `bound`, those without an
/// offset placed one after another from `start`, those with one at it
/// past `origin`, the container's start (§5.2). An absent entry ends
/// where it would have started.
fn extent(entries: &[Entry], origin: u64, start: u64, bound: Bound) -> u64 {
    let mut at = start;
    let mut furthest = start;
    for entry in entries {
        if let Some(offset) = entry.offset {
            at = origin + offset;
        }
        if entry.when.is_none() || matches!(bound, Bound::Most) {
            at += node_bits(&entry.node, bound);
        }
        furthest = furthest.max(at);
    }
    furthest
}

/// What a node reads, at `bound` where it varies.
fn node_bits(node: &Node, bound: Bound) -> u64 {
    match node {
        Node::Padding(bits) => *bits,
        Node::Field(field) => scalar_bits(field, bound),
        Node::Array(array) => {
            let count = match (&array.count, bound) {
                (Counted::Fixed(count), _) => *count,
                (Counted::Field { .. } | Counted::Remaining, Bound::Least) => 0,
                (Counted::Field { .. } | Counted::Remaining, Bound::Most) => MOST_ELEMENTS,
            };
            u64::from(count) * element_bits(&array.element, bound)
        }
        Node::Struct(node) => extent(&node.ty.entries, 0, 0, bound),
    }
}

/// An element's size, at `bound` where it varies.
fn element_bits(element: &ElementNode, bound: Bound) -> u64 {
    match element {
        ElementNode::Scalar(element) => scalar_bits(element, bound),
        ElementNode::Struct(ty) => extent(&ty.entries, 0, 0, bound),
    }
}

/// An element's exact size (§5.2), if it has one: a number's, a bool's, a
/// fixed length's, or a struct's whose entries all have one. An array has
/// one only with a `fixed` count of elements that have one, even a count
/// of 0, which makes every element the same size without giving it an
/// exact one.
fn exact_bits(element: &ElementNode) -> Option<u64> {
    match element {
        ElementNode::Scalar(element) => exact_scalar(element),
        ElementNode::Struct(ty) => exact_entries(&ty.entries),
    }
}

/// A struct type's exact size: the furthest end of its entries, when every
/// one has an exact size, none of them conditional.
fn exact_entries(entries: &[Entry]) -> Option<u64> {
    let mut at = 0;
    let mut furthest = 0;
    for entry in entries {
        if entry.when.is_some() {
            return None;
        }
        if let Some(offset) = entry.offset {
            at = offset;
        }
        at += match &entry.node {
            Node::Padding(bits) => *bits,
            Node::Field(field) => exact_scalar(field)?,
            Node::Array(array) => match array.count {
                Counted::Fixed(count) => exact_bits(&array.element)? * u64::from(count),
                Counted::Field { .. } | Counted::Remaining => return None,
            },
            Node::Struct(node) => exact_entries(&node.ty.entries)?,
        };
        furthest = furthest.max(at);
    }
    Some(furthest)
}

fn exact_scalar(field: &Placed) -> Option<u64> {
    match field.kind.length() {
        None | Some(Length::Fixed { .. }) => Some(u64::from(field.kind.bits())),
        Some(_) => None,
    }
}

fn scalar_bits(field: &Placed, bound: Bound) -> u64 {
    u64::from(match bound {
        Bound::Least => field.kind.bits(),
        Bound::Most => field.kind.most_bits(),
    })
}

/// The raw values a count field of `bits` can hold that give each array
/// it counts, by its `(multiply, add)`, a count of 0 to
/// [`MOST_ELEMENTS`]: the lowest and highest, or `None` if there are none.
fn raw_range(bits: u32, scales: &[(u32, i64)]) -> Option<(u64, u64)> {
    let largest = if bits >= 64 {
        u64::MAX
    } else {
        (1 << bits) - 1
    };
    let mut low = 0_i128;
    let mut high = i128::from(largest);
    for &(multiply, add) in scales {
        let (multiply, add) = (i128::from(multiply), i128::from(add));
        low = low.max((multiply - 1 - add).div_euclid(multiply));
        high = high.min((i128::from(MOST_ELEMENTS) - add).div_euclid(multiply));
    }
    if low > high {
        return None;
    }
    Some((u64::try_from(low).ok()?, u64::try_from(high).ok()?))
}

// ---- As IR JSON -----------------------------------------------------------

impl Body {
    /// Its entries, padding included.
    pub(super) fn entries_json(&self) -> Vec<Value> {
        self.entries.iter().map(entry_json).collect()
    }

    /// Every param it maps, as the catalog defines them.
    pub(super) fn params(&self) -> Vec<(String, Value)> {
        let mut params = Vec::new();
        params_of(&self.entries, &mut params);
        params.extend(self.composites.iter().map(CompositeNode::param_json));
        params
    }

    /// Its struct types, by name.
    pub(super) fn types_json(&self) -> Vec<(String, Value)> {
        self.types
            .iter()
            .map(|ty| {
                let entries: Vec<Value> = ty.entries.iter().map(entry_json).collect();
                (ty.name.clone(), json!({ "entries": entries }))
            })
            .collect()
    }

    /// Its composites, in the order the packet lists them.
    pub(super) fn composites_json(&self) -> Vec<Value> {
        self.listed
            .iter()
            .filter_map(|&index| self.composites.get(index))
            .map(CompositeNode::json)
            .collect()
    }
}

fn entry_json(entry: &Entry) -> Value {
    let mut json = node_json(&entry.node);
    if let Some(offset) = entry.offset {
        json["offset_bits"] = json!(offset);
    }
    if let Some(when) = &entry.when {
        json["when"] = condition_json(&when.field, &when.test);
    }
    json
}

/// A condition as IR JSON, every comparison in it on `field`.
fn condition_json(field: &str, test: &Test) -> Value {
    let operand = |operand: &Operand| match operand {
        Operand::Int(n) => integer_json(*n),
        Operand::Text(text) => json!(text),
    };
    match test {
        Test::Compare(op, value) => {
            json!({ "kind": "compare", "field": field, "op": op.name(), "value": operand(value) })
        }
        Test::In(values) => {
            let values: Vec<Value> = values.iter().map(operand).collect();
            json!({ "kind": "in", "field": field, "values": values })
        }
        Test::Not(test) => json!({ "kind": "not", "condition": condition_json(field, test) }),
        Test::All(tests) | Test::Any(tests) => {
            let kind = if matches!(test, Test::All(_)) {
                "all"
            } else {
                "any"
            };
            let conditions: Vec<Value> = tests.iter().map(|t| condition_json(field, t)).collect();
            json!({ "kind": kind, "conditions": conditions })
        }
    }
}

fn node_json(node: &Node) -> Value {
    match node {
        Node::Padding(bits) => json!({ "kind": "padding", "size_bits": bits }),
        Node::Field(field) => field_json(field),
        Node::Array(array) => {
            let element = match &array.element {
                ElementNode::Scalar(element) => {
                    let mut json = scalar_json(element);
                    json.insert("kind".into(), json!("scalar"));
                    Value::Object(json)
                }
                ElementNode::Struct(ty) => json!({ "kind": "struct", "name": ty.name }),
            };
            let count = match &array.count {
                Counted::Fixed(count) => json!({ "kind": "fixed", "count": count }),
                Counted::Field {
                    field,
                    multiply,
                    add,
                } => {
                    let mut count = Map::new();
                    count.insert("kind".into(), json!("field"));
                    count.insert("field".into(), json!(field));
                    if *multiply != 1 {
                        count.insert("multiply".into(), json!(multiply));
                    }
                    if *add != 0 {
                        count.insert("add".into(), json!(add));
                    }
                    Value::Object(count)
                }
                Counted::Remaining => json!({ "kind": "remaining" }),
            };
            let mut json = json!({ "kind": "array", "name": array.name, "count": count,
                                   "element": element });
            if let ElementNode::Scalar(Placed {
                param: Some(param), ..
            }) = &array.element
            {
                json["canonical"] = json!(param);
            }
            if let Some(interval) = array.interval {
                json["sample_interval_ms"] = json!(interval);
            }
            json
        }
        Node::Struct(node) => {
            json!({ "kind": "struct", "name": node.name, "struct": node.ty.name })
        }
    }
}

fn params_of(entries: &[Entry], params: &mut Vec<(String, Value)>) {
    let mut add = |field: &Placed| {
        if let Some(param) = &field.param {
            params.push((param.clone(), param_json(&field.meaning)));
        }
    };
    let mut inner = Vec::new();
    for entry in entries {
        match &entry.node {
            Node::Padding(_) => {}
            Node::Field(field) => add(field),
            Node::Array(array) => match &array.element {
                ElementNode::Scalar(element) => add(element),
                ElementNode::Struct(ty) => inner.push(&ty.entries),
            },
            Node::Struct(node) => inner.push(&node.ty.entries),
        }
    }
    for entries in inner {
        params_of(entries, params);
    }
}

// ---- Values ---------------------------------------------------------------

/// Values for a container's present entries: each one's raw value, by
/// name, in order, its count fields holding counts its arrays can have,
/// and its length fields the lengths of their values. An entry whose
/// condition its selector's value fails is absent.
pub(super) fn values(entries: &[Entry]) -> BoxedStrategy<Vec<(String, Raw)>> {
    let counts = count_ranges(entries);
    let selected = selector_values(entries);
    let raws: Vec<BoxedStrategy<u64>> = counts
        .iter()
        .map(|(_, (low, high))| (*low..=*high).boxed())
        .collect();
    let entries = entries.to_vec();
    raws.prop_flat_map(move |raws| {
        let held: BTreeMap<String, u64> = counts
            .iter()
            .map(|(name, _)| name.clone())
            .zip(raws)
            .collect();
        let drawn: Vec<BoxedStrategy<Option<(String, Raw)>>> = entries
            .iter()
            .map(|entry| entry_value(&entry.node, &held, &selected))
            .collect();
        let entries = entries.clone();
        drawn.prop_map(move |drawn| {
            let mut record: Vec<(String, Raw)> = Vec::new();
            for (entry, value) in entries.iter().zip(drawn) {
                let Some(value) = value else {
                    continue;
                };
                if entry.when.as_ref().is_none_or(|when| when.holds(&record)) {
                    record.push(value);
                }
            }
            measure(&entries, &mut record);
            record
        })
    })
    .boxed()
}

/// Gives each length field of a container the raw value its entry's value
/// takes: `(bytes - add) / multiply` (§5.4).
fn measure(entries: &[Entry], record: &mut [(String, Raw)]) {
    for entry in entries {
        let Node::Field(field) = &entry.node else {
            continue;
        };
        let Some(length @ Length::Field { .. }) = field.kind.length() else {
            continue;
        };
        let bytes = record
            .iter()
            .find(|(name, _)| *name == field.name)
            .and_then(|(_, raw)| value_bytes(&field.kind, raw));
        let Some(raw) = bytes.and_then(|bytes| length.field_raw(bytes)) else {
            continue;
        };
        let name = length_field(&field.name);
        if let Some(entry) = record.iter_mut().find(|(entry, _)| *entry == name) {
            entry.1 = Raw::Uint(raw);
        }
    }
}

/// How many bytes a string's or bytes' value is, in its encoding.
fn value_bytes(kind: &Kind, raw: &Raw) -> Option<usize> {
    match (kind, raw) {
        (Kind::Text { text, .. }, Raw::Str(value)) => Some(encode(value, *text).len()),
        (Kind::Bytes(_), Raw::Bytes(value)) => Some(value.len()),
        _ => None,
    }
}

/// The values each selector field of a container leans on: what its
/// conditions compare with and the values beside it (for a mask, the mask,
/// none of it, all but its lowest bit, and every other bit), so each
/// condition holds about half the time.
fn selector_values(entries: &[Entry]) -> BTreeMap<String, Vec<Raw>> {
    let mut operands: BTreeMap<&str, Vec<(Op, Operand)>> = BTreeMap::new();
    for entry in entries {
        if let Some(when) = &entry.when {
            when.test
                .operands(operands.entry(when.field.as_str()).or_default());
        }
    }
    entries
        .iter()
        .filter_map(|entry| {
            let Node::Field(field) = &entry.node else {
                return None;
            };
            let compared = operands.get(field.name.as_str())?;
            let mut raws = Vec::new();
            for (op, operand) in compared {
                match operand {
                    Operand::Text(text) => {
                        raws.push(Raw::Str(text.clone()));
                        raws.push(Raw::Str("z".to_string()));
                    }
                    Operand::Int(n) => {
                        let largest = field.kind.integers().map_or(0, |range| *range.end());
                        let near = match op {
                            Op::MaskAny | Op::MaskAll => [*n, 0, n & (n - 1), largest & !n],
                            _ => [n - 1, *n, n + 1, *n],
                        };
                        raws.extend(
                            near.into_iter()
                                .filter_map(|value| int_raw(&field.kind, value)),
                        );
                    }
                }
            }
            (!raws.is_empty()).then(|| (field.name.clone(), raws))
        })
        .collect()
}

/// An integer as a raw value of a field of this kind, if it holds it: a
/// bool holds 0 and 1.
fn int_raw(kind: &Kind, value: i128) -> Option<Raw> {
    match kind {
        Kind::Bool => match value {
            0 => Some(Raw::Bool(false)),
            1 => Some(Raw::Bool(true)),
            _ => None,
        },
        Kind::Uint(_) if kind.integers()?.contains(&value) => {
            u64::try_from(value).ok().map(Raw::Uint)
        }
        Kind::Int(_) if kind.integers()?.contains(&value) => {
            i64::try_from(value).ok().map(Raw::Int)
        }
        _ => None,
    }
}

/// Each count field of a container, and the raw values it can hold.
fn count_ranges(entries: &[Entry]) -> Vec<(String, (u64, u64))> {
    let mut scales: BTreeMap<&str, Vec<(u32, i64)>> = BTreeMap::new();
    for entry in entries {
        if let Node::Array(ArrayNode {
            count:
                Counted::Field {
                    field,
                    multiply,
                    add,
                },
            ..
        }) = &entry.node
        {
            scales.entry(field).or_default().push((*multiply, *add));
        }
    }
    entries
        .iter()
        .filter_map(|entry| match &entry.node {
            Node::Field(field) => {
                let scales = scales.get(field.name.as_str())?;
                Some((field.name.clone(), raw_range(field.kind.bits(), scales)?))
            }
            _ => None,
        })
        .collect()
}

fn entry_value(
    node: &Node,
    held: &BTreeMap<String, u64>,
    selected: &BTreeMap<String, Vec<Raw>>,
) -> BoxedStrategy<Option<(String, Raw)>> {
    let named = |name: &str, values: BoxedStrategy<Raw>| {
        let name = name.to_string();
        values
            .prop_map(move |raw| Some((name.clone(), raw)))
            .boxed()
    };
    match node {
        Node::Padding(_) => Just(None).boxed(),
        Node::Field(field) => match (held.get(&field.name), selected.get(&field.name)) {
            (Some(raw), _) => named(&field.name, Just(Raw::Uint(*raw)).boxed()),
            (None, Some(leaned)) => {
                let values = prop_oneof![3 => select(leaned.clone()), 1 => raw_value(field)];
                named(&field.name, values.boxed())
            }
            (None, None) => named(&field.name, raw_value(field)),
        },
        Node::Array(array) => {
            let element = match &array.element {
                ElementNode::Scalar(element) => raw_value(element),
                ElementNode::Struct(ty) => values(&ty.entries).prop_map(Raw::Record).boxed(),
            };
            let items = match &array.count {
                Counted::Fixed(count) => vec(element, *count as usize).boxed(),
                Counted::Field {
                    field,
                    multiply,
                    add,
                } => {
                    let raw = held.get(field).copied().unwrap_or(0);
                    let count = i128::from(raw) * i128::from(*multiply) + i128::from(*add);
                    vec(element, usize::try_from(count).unwrap_or(0)).boxed()
                }
                Counted::Remaining => vec(element, 0..=MOST_ELEMENTS as usize).boxed(),
            };
            named(&array.name, items.prop_map(Raw::Array).boxed())
        }
        Node::Struct(node) => named(
            &node.name,
            values(&node.ty.entries).prop_map(Raw::Record).boxed(),
        ),
    }
}

// ---- Writing --------------------------------------------------------------

/// Writes a message's values into its payload as decoding reads them back
/// (§2.1, §2.2, §5.2), and notes each step decoding takes and the bits it
/// needs ([`Step`]). Counts and lengths are worked out from their fields
/// as decoding works them out (§5.4), so a planted count or length is
/// followed as far as the values go.
pub(super) struct Writer<'p> {
    payload: &'p mut [u8],
    /// A string's path, and the bytes written for its value instead.
    garbled: Option<(String, Vec<u8>)>,
    steps: Vec<Step>,
}

impl<'p> Writer<'p> {
    /// A writer into `payload`, after the steps already taken.
    pub(super) fn new(
        payload: &'p mut [u8],
        garbled: Option<(String, Vec<u8>)>,
        steps: Vec<Step>,
    ) -> Writer<'p> {
        Writer {
            payload,
            garbled,
            steps,
        }
    }

    pub(super) fn steps(self) -> Vec<Step> {
        self.steps
    }

    /// Writes a container's present values, leaving padding as it was, its
    /// entries named under `path`: entries without an offset one after
    /// another from `at`, those with one at it past `origin`, the
    /// container's start; an absent entry ends where it would have started
    /// (§5.2). Returns where it ends, or `None` where decoding stops
    /// inside it.
    pub(super) fn container(
        &mut self,
        entries: &[Entry],
        record: &[(String, Raw)],
        origin: u64,
        at: u64,
        path: &str,
    ) -> Option<u64> {
        let mut at = at;
        let mut furthest = at;
        let mut values = record.iter();
        let padding = if path.is_empty() {
            Place::Padding
        } else if path.ends_with(']') {
            Place::PaddingInElement
        } else {
            Place::PaddingInStruct
        };
        for entry in entries {
            if let Some(offset) = entry.offset {
                at = origin + offset;
            }
            if entry.when.as_ref().is_some_and(|when| !when.holds(record)) {
                continue;
            }
            if let Node::Padding(bits) = &entry.node {
                at += bits;
                furthest = furthest.max(at);
                self.reach(path, at, Short::Padding, padding);
                continue;
            }
            let Some((name, raw)) = values.next() else {
                return self.unknown();
            };
            let here = if path.is_empty() {
                name.clone()
            } else {
                format!("{path}.{name}")
            };
            at = match (&entry.node, raw) {
                (Node::Field(field), raw) => {
                    self.scalar(field, raw, at, &here, record, Place::Field)?
                }
                (Node::Array(array), Raw::Array(items)) => {
                    self.array(array, items, at, &here, record)?
                }
                (Node::Struct(node), Raw::Record(fields)) => {
                    self.container(&node.ty.entries, fields, at, at, &here)?
                }
                _ => return self.unknown(),
            };
            furthest = furthest.max(at);
        }
        Some(furthest)
    }

    /// A field or scalar element at `at`. `scope` is its container's
    /// values, which hold the field its length is taken from, if any.
    fn scalar(
        &mut self,
        field: &Placed,
        raw: &Raw,
        at: u64,
        path: &str,
        scope: &[(String, Raw)],
        place: Place,
    ) -> Option<u64> {
        let Some(length) = field.kind.length() else {
            let end = put(field, raw, self.payload, at);
            self.reach(path, end, Short::PastEnd, place);
            return Some(end);
        };
        let (value, text) = match (&field.kind, raw) {
            (Kind::Text { text, .. }, Raw::Str(value)) => (encode(value, *text), Some(*text)),
            (Kind::Bytes(_), Raw::Bytes(value)) => (value.clone(), None),
            _ => return self.unknown(),
        };
        let garbled = match &self.garbled {
            Some((spot, bytes)) if spot == path => Some(bytes.clone()),
            _ => None,
        };
        let bytes = garbled.clone().unwrap_or(value);
        // Its start is checked first: one past the packet's end, at an
        // offset, is past the end there, with its start as the bits it
        // needs. That figure is MDL's report; the spec gives the code and
        // the path.
        self.reach(path, at, Short::PastEnd, place);
        let end = match length {
            Length::Fixed { .. } => {
                let end = put_run(self.payload, at, &bytes, length);
                self.reach(path, end, Short::PastEnd, place);
                end
            }
            // Decoding looks for the terminator in what the packet has.
            Length::Terminated(byte) => {
                let end = put_run(self.payload, at, &bytes, length);
                self.reach(path, end, Short::NoTerminator(byte), Place::Terminator);
                end
            }
            // The length is checked against the bits left before anything
            // is copied (§5.5).
            Length::Field { multiply, add, .. } => {
                let Some(raw) = held(scope, &length_field(&field.name)) else {
                    return self.unknown();
                };
                let said = raw * i128::from(multiply) + i128::from(add);
                let Ok(said) = u64::try_from(said) else {
                    return self.fail(path, Problem::BadLength { value: said });
                };
                let needs = at.saturating_add(said.saturating_mul(8));
                if said != bytes.len() as u64 {
                    self.reach(path, needs, Short::PastEnd, place);
                    return self.unknown();
                }
                let end = put_run(self.payload, at, &bytes, length);
                self.reach(path, end, Short::PastEnd, place);
                end
            }
            // It reads whatever is left. A packet that ends inside its
            // planted bytes may leave them out, so that's no longer known.
            Length::Remaining => {
                let end = put_run(self.payload, at, &bytes, length);
                if garbled.is_some() {
                    self.steps.push(Step::Opaque { needs: end });
                }
                end
            }
        };
        match (garbled, text) {
            (Some(_), Some(text)) => self.fail(
                path,
                Problem::InvalidText {
                    encoding: text.name(),
                },
            ),
            _ => Some(end),
        }
    }

    /// An array at `at` (§5.2): elements one after another, as many as its
    /// count, or until the packet ends.
    fn array(
        &mut self,
        array: &ArrayNode,
        items: &[Raw],
        at: u64,
        path: &str,
        scope: &[(String, Raw)],
    ) -> Option<u64> {
        let mut at = at;
        let count = match &array.count {
            Counted::Fixed(count) => i128::from(*count),
            Counted::Field {
                field,
                multiply,
                add,
            } => match held(scope, field) {
                Some(raw) => raw * i128::from(*multiply) + i128::from(*add),
                None => return self.unknown(),
            },
            Counted::Remaining => {
                for (index, item) in items.iter().enumerate() {
                    self.steps.push(Step::More { at });
                    at = self.element(&array.element, item, at, &format!("{path}[{index}]"))?;
                }
                return Some(at);
            }
        };
        let Ok(count) = u64::try_from(count) else {
            return self.fail(path, Problem::BadLength { value: count });
        };
        // Checked against the bits left before any element is read (§5.5):
        // as count × size for elements with an exact size, else a bit each.
        // Validation refuses an element that never takes a bit (§5.2), so
        // an exact size is at least 1.
        let exact = exact_bits(&array.element);
        let each = exact.unwrap_or(1);
        let needs = at.saturating_add(count.saturating_mul(each));
        let place = match exact {
            Some(_) => Place::Array,
            None => Place::ArrayBitEach,
        };
        self.reach(path, needs, Short::PastEnd, place);
        if count > items.len() as u64 {
            return self.unknown();
        }
        for (index, item) in items.iter().take(count as usize).enumerate() {
            at = self.element(&array.element, item, at, &format!("{path}[{index}]"))?;
        }
        Some(at)
    }

    fn element(&mut self, element: &ElementNode, item: &Raw, at: u64, path: &str) -> Option<u64> {
        match (element, item) {
            (ElementNode::Scalar(element), raw) => {
                self.scalar(element, raw, at, path, &[], Place::Element)
            }
            (ElementNode::Struct(ty), Raw::Record(fields)) => {
                self.container(&ty.entries, fields, at, at, path)
            }
            _ => self.unknown(),
        }
    }

    fn reach(&mut self, path: &str, needs: u64, short: Short, place: Place) {
        self.steps.push(Step::Reach {
            path: path.to_string(),
            needs,
            short,
            place,
        });
    }

    /// Decoding fails here: the walk stops.
    fn fail(&mut self, path: &str, problem: Problem) -> Option<u64> {
        self.steps.push(Step::Fail(Failure {
            path: path.to_string(),
            problem,
        }));
        None
    }

    /// What decoding does from here can't be told from the values.
    fn unknown(&mut self) -> Option<u64> {
        self.steps.push(Step::Unknown);
        None
    }
}

/// A count or length field's raw value, among its container's values.
fn held(scope: &[(String, Raw)], name: &str) -> Option<i128> {
    scope.iter().find_map(|(field, raw)| match raw {
        Raw::Uint(n) if field == name => Some(i128::from(*n)),
        _ => None,
    })
}

/// Writes one scalar at `at`; returns where it ends.
fn put(field: &Placed, raw: &Raw, payload: &mut [u8], at: u64) -> u64 {
    let bits = field.kind.bits();
    match (&field.kind, raw) {
        (_, Raw::Uint(n)) => put_bits(payload, at, bits, *n, field.little),
        (_, Raw::Int(n)) => put_bits(payload, at, bits, fit(*n as u64, bits), field.little),
        (Kind::Float(32), Raw::Float(x)) => put_bits(
            payload,
            at,
            32,
            u64::from((*x as f32).to_bits()),
            field.little,
        ),
        (_, Raw::Float(x)) => put_bits(payload, at, 64, x.to_bits(), field.little),
        (_, Raw::Bool(b)) => put_bits(payload, at, 1, u64::from(*b), false),
        (Kind::Text { length, text }, Raw::Str(value)) => {
            return put_run(payload, at, &encode(value, *text), *length);
        }
        (Kind::Bytes(length), Raw::Bytes(value)) => return put_run(payload, at, value, *length),
        (_, Raw::Str(_) | Raw::Bytes(_) | Raw::Array(_) | Raw::Record(_)) => {}
    }
    at + u64::from(bits)
}

/// Writes a string's or bytes' value at `at`, a byte (§2.2), and the
/// terminator its length ends it with, if any; returns where the entry
/// ends: a fixed length takes every byte, a terminator takes one more.
fn put_run(payload: &mut [u8], at: u64, value: &[u8], length: Length) -> u64 {
    let start = (at / 8) as usize;
    payload[start..start + value.len()].copy_from_slice(value);
    let taken = match length {
        Length::Fixed { bytes, terminator } => {
            if let Some(byte) = terminator
                && value.len() < bytes as usize
            {
                payload[start + value.len()] = byte;
            }
            bytes as usize
        }
        Length::Terminated(byte) => {
            payload[start + value.len()] = byte;
            value.len() + 1
        }
        Length::Field { .. } | Length::Remaining => value.len(),
    };
    at + 8 * taken as u64
}

// ---- Samples --------------------------------------------------------------

impl Body {
    /// The samples a message's values give (§7.3): each mapped field and
    /// scalar element in record order, then the composites by name (§7.4).
    pub(super) fn samples(&self, record: &[(String, Raw)]) -> Vec<Sample> {
        let mut samples = Vec::new();
        samples_of(&self.entries, record, "", None, &mut samples);
        let mut composites: Vec<&CompositeNode> = self.composites.iter().collect();
        composites.sort_by(|a, b| a.param().cmp(b.param()));
        samples.extend(composites.into_iter().filter_map(|c| c.sample(record)));
        samples
    }
}

/// Samples of a container's values, at paths under `prefix`, inside an
/// array element at `index` (the innermost array's).
fn samples_of(
    entries: &[Entry],
    record: &[(String, Raw)],
    prefix: &str,
    index: Option<u64>,
    samples: &mut Vec<Sample>,
) {
    for (entry, (name, raw)) in present(entries, record) {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        match (&entry.node, raw) {
            (Node::Field(field), raw) => samples.extend(sample(field, raw, path, index, None)),
            (Node::Array(array), Raw::Array(items)) => {
                for (position, item) in (0_u64..).zip(items) {
                    let at = format!("{path}[{position}]");
                    match (&array.element, item) {
                        (ElementNode::Scalar(element), raw) => {
                            let offset = array.interval.map(|ms| offset_ns(position, ms));
                            samples.extend(sample(element, raw, at, Some(position), offset));
                        }
                        (ElementNode::Struct(ty), Raw::Record(fields)) => {
                            samples_of(&ty.entries, fields, &at, Some(position), samples);
                        }
                        _ => {}
                    }
                }
            }
            (Node::Struct(node), Raw::Record(fields)) => {
                samples_of(&node.ty.entries, fields, &path, index, samples);
            }
            _ => {}
        }
    }
}

/// How long after the packet's event time element `position` of a block
/// was taken, in whole nanoseconds: `round(i × interval_ms × 1000000)` in
/// binary64, left to right, ties away from zero (§7.3).
fn offset_ns(position: u64, interval_ms: f64) -> i64 {
    (position as f64 * interval_ms * 1_000_000.0).round() as i64
}

mod composite;
pub(super) mod faults;
#[cfg(test)]
mod tests;
