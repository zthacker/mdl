//! JSON command packets (`spec/encoding.md` §5, §7): each field and array
//! at a JSON Pointer, the payload one object written as canonical JSON
//! (`canonical.rs`). A packet writes constants; arguments of every type
//! through each way §3.1 turns a value into a raw value, as the binary
//! packets draw them but in JSON's fields (integers of 64 bits, binary64
//! floats, strings of any text, and no bytes); maybe the target's native
//! id; and up to two arrays of a list argument's values or of records,
//! which JSON doesn't count. Packets are told apart by a constant at one
//! pointer they all have, an integer or a string (§7.2).
//!
//! Pointers go through objects of their own now and then, named with
//! digits, `/`, `~`, nothing, or text past the BMP, and end at the entry's
//! name, which may start with a character JSON escapes or that orders
//! differently by UTF-16 code units than by code points.

use proptest::collection::{btree_set, vec};
use proptest::prelude::*;
use proptest::sample::select;
use proptest::strategy::Union;

use super::Form;
use super::canonical::{Canon, write};
use super::encoders::{as_literal, defaulted, element_encoder, encoder, with_default};
use super::packets::{group_defaults, left_out_list};
use super::requests::{Packed, gathered, raws};
use super::values::text_for;
use super::{
    Arg, ArgType, Bounds, Const, Counted, Elements, Encoder, Encoding, Group, Layout, Len, Placed,
    Scale, Shape, Source, Target, encoder_of,
};
use crate::layout::json::doc::{self, Doc, Spell};
use crate::layout::{Raw, Text};

/// One entry of a packet after its key, before it's laid out.
#[derive(Debug, Clone)]
enum Item {
    /// The argument at this index of the command's.
    Argument(usize),
    Constant(Shape, Const),
    Target(Shape),
    /// The array at this index of the packet's.
    Array(usize),
}

/// A record's field: one that takes a list argument (`L`), or a constant.
#[derive(Debug, Clone)]
enum RecordField<L> {
    List(L),
    Constant(Shape, Const),
}

/// An array as it's generated: the bounds its lists share, the lengths a
/// request may give them, and its elements.
#[derive(Debug, Clone)]
struct ArrayDraft {
    bounds: Bounds,
    lengths: Vec<i64>,
    element: DraftElement,
}

#[derive(Debug, Clone)]
enum DraftElement {
    Scalar(Box<Encoder>),
    /// Records' fields, each at its own place in the record's object.
    Records(Vec<(RecordField<Encoder>, Place)>),
}

/// An array's elements once its lists have places among the arguments.
#[derive(Debug, Clone)]
enum ArrayElement {
    /// The list argument at this index of the command's.
    Scalar(usize),
    Records(Vec<(RecordField<usize>, Place)>),
}

/// Where an entry's pointer goes: through objects named `containers`,
/// then to the entry's name after `decoration`. Containers are never an
/// entry's name, and names are their entries' own, so no pointer equals
/// another or leads into one (`spec/encoding.md` §5).
#[derive(Debug, Clone)]
pub(super) struct Place {
    containers: Vec<&'static str>,
    decoration: &'static str,
}

/// Objects an entry's pointer goes through: a word, the empty name,
/// digits (which encoding writes as an object's member, never an array's
/// index), names with `/` and `~`, and text either side of the surrogates.
const CONTAINERS: [&str; 8] = ["hdr", "", "0", "01", "a/b", "~", "\u{ff5e}", "\u{1f600}"];

/// What an entry's name may start with: characters a pointer escapes (`/`
/// as `~1`, `~` as `~0`), text that looks escaped, a space, `é` composed
/// and not, characters JSON escapes in a member's name (`"`, `\`, a
/// control), DEL, which it doesn't, and characters that sort differently by
/// UTF-16 code units (U+FF5E after U+1F600) than by code points.
const DECORATIONS: [(u32, &str); 13] = [
    (4, ""),
    (2, "\u{ff5e}"),
    (2, "\u{1f600}"),
    (1, "/"),
    (1, "~"),
    (1, "~1"),
    (1, "a b"),
    (1, "\u{e9}"),
    (1, "e\u{301}"),
    (1, "\""),
    (1, "\\"),
    (1, "\u{1}"),
    (1, "\u{7f}"),
];

/// An entry's place: at the top of its object most of the time, else
/// through one or two objects of its own.
pub(super) fn place() -> BoxedStrategy<Place> {
    let containers = prop_oneof![
        3 => Just(Vec::new()),
        2 => vec(select(&CONTAINERS[..]), 1..=2),
    ];
    let decorations = DECORATIONS
        .iter()
        .map(|&(weight, decoration)| (weight, Just(decoration).boxed()))
        .collect();
    (containers, Union::new_weighted(decorations))
        .prop_map(|(containers, decoration)| Place {
            containers,
            decoration,
        })
        .boxed()
}

impl Place {
    /// The pointer's tokens for an entry named `name`.
    fn tokens(&self, name: &str) -> Vec<String> {
        let mut tokens: Vec<String> = self.containers.iter().map(|c| c.to_string()).collect();
        tokens.push(format!("{}{name}", self.decoration));
        tokens
    }
}

/// A packet's command, before it's laid out.
#[derive(Debug, Clone)]
pub(super) struct CommandSpec {
    /// The catalog's arguments, each with a field that encodes it, and
    /// whether this set's packet uses that field.
    arguments: Vec<(Encoder, bool)>,
    /// The entries after the key, in order.
    items: Vec<Item>,
    /// Item `k`'s place is `places[k]`, cycling.
    places: Vec<Place>,
    arrays: Vec<ArrayElement>,
    groups: Vec<Group>,
}

/// The field a JSON packet has for the encoder's: an integer of 64 bits, a
/// binary64 float, a bool, or a string of any text; none for bytes, which
/// JSON doesn't have (`spec/decoding.md` §5.3). The argument's range, made
/// for a narrower binary field, stays its own, so its raw values fit.
fn jsonify(mut encoder: Encoder) -> Option<Encoder> {
    encoder.shape = match encoder.shape {
        Shape::Uint(_) => Shape::Uint(64),
        Shape::Int(..) => Shape::Int(64, Form::TwosComplement),
        Shape::Float(_) => Shape::Float(64),
        Shape::Bool => Shape::Bool,
        Shape::Text(..) => string_shape(),
        Shape::Bytes(_) => return None,
    };
    Some(encoder)
}

fn string_shape() -> Shape {
    Shape::Text(Text::Utf8, Len::Remaining)
}

/// An argument a JSON field encodes, with a default or none.
fn json_encoder() -> BoxedStrategy<Encoder> {
    encoder()
        .prop_filter_map("an argument a JSON field holds", jsonify)
        .boxed()
}

/// An f64 argument a float field writes as it is, with no range: any
/// finite double, so every form ECMAScript writes one in (§5).
fn plain_double() -> BoxedStrategy<Encoder> {
    with_default(Just(encoder_of(ArgType::F64, Shape::Float(64))))
}

/// A list argument's element, as a JSON field.
fn json_element() -> BoxedStrategy<Encoder> {
    element_encoder(false, false)
        .prop_filter_map("an element a JSON field holds", jsonify)
        .boxed()
}

/// A constant: an integer of either sign out to 64 bits' ends, a bool, or
/// text.
fn constant() -> BoxedStrategy<(Shape, Const)> {
    let integer = prop_oneof![
        Just(Shape::Uint(64)),
        Just(Shape::Int(64, Form::TwosComplement))
    ]
    .prop_flat_map(|shape| {
        let held = shape.integers().unwrap_or(0..=0);
        let (low, high) = (*held.start(), *held.end());
        let value = prop_oneof![
            Just(low),
            Just(high),
            low.max(-1000)..=high.min(1000),
            low..=high,
        ];
        (Just(shape), value.prop_map(Const::Int))
    });
    prop_oneof![
        2 => integer,
        1 => any::<bool>().prop_map(|b| (Shape::Bool, Const::Bool(b))),
        1 => text_for(Text::Utf8, &Len::Remaining).prop_map(|text| (string_shape(), Const::Text(text))),
    ]
    .boxed()
}

/// Each packet's value of the key, a constant at one pointer every packet
/// has: distinct integers, or distinct strings, compared exactly (§7.2).
pub(super) fn key(count: usize) -> BoxedStrategy<(Shape, Vec<Const>)> {
    let integer = prop_oneof![
        Just(Shape::Uint(64)),
        Just(Shape::Int(64, Form::TwosComplement))
    ]
    .prop_flat_map(move |shape| {
        let held = shape.integers().unwrap_or(0..=0);
        let (low, high) = (*held.start(), *held.end());
        let value = prop_oneof![
            Just(low),
            Just(high),
            low.max(-50)..=high.min(50),
            low..=high
        ];
        let values = btree_set(value, count)
            .prop_map(|values| values.into_iter().map(Const::Int).collect::<Vec<_>>())
            .prop_shuffle();
        (Just(shape), values)
    });
    let text = btree_set(text_for(Text::Utf8, &Len::Remaining), count)
        .prop_map(|tags| tags.into_iter().map(Const::Text).collect::<Vec<_>>())
        .prop_shuffle()
        .prop_map(|tags| (string_shape(), tags));
    prop_oneof![integer, text].boxed()
}

/// How a set's packets are told apart (§7.2): by the key `op` alone; by it
/// and the `apid` metadata, which packets may share; or by the metadata
/// alone, each packet its own, as a transport's subject would carry a
/// command's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Keying {
    Op,
    OpAndMetadata,
    Metadata,
}

impl Keying {
    /// Whether the family has metadata.
    pub(super) fn metadata(self) -> bool {
        self != Keying::Op
    }

    /// Whether every packet has the key `op`.
    pub(super) fn op(self) -> bool {
        self != Keying::Metadata
    }
}

pub(super) fn keying() -> BoxedStrategy<Keying> {
    prop_oneof![
        2 => Just(Keying::Op),
        1 => Just(Keying::OpAndMetadata),
        1 => Just(Keying::Metadata),
    ]
    .boxed()
}

/// Each of `count` packets' `apid`, as `keying` keys them.
pub(super) fn apids(keying: Keying, count: usize) -> BoxedStrategy<Vec<Option<i64>>> {
    match keying {
        Keying::Op => Just(vec![None; count]).boxed(),
        Keying::OpAndMetadata => vec(0_i64..3, count)
            .prop_map(|apids| apids.into_iter().map(Some).collect())
            .boxed(),
        Keying::Metadata => btree_set(0_i64..8, count)
            .prop_map(|apids| apids.into_iter().map(Some).collect::<Vec<_>>())
            .prop_shuffle()
            .boxed(),
    }
}

fn target_shape() -> BoxedStrategy<Shape> {
    select(vec![
        Shape::Uint(64),
        Shape::Int(64, Form::TwosComplement),
        string_shape(),
    ])
    .boxed()
}

/// An array of a list argument's values or of records, its lists' bounds
/// any, either end open; the lengths a request gives run from the least
/// to the most, or three past the least without a most.
fn array_draft() -> BoxedStrategy<ArrayDraft> {
    (0_u32..=2, proptest::option::of(0_u32..=3))
        .prop_flat_map(|(min, extra)| {
            let max = extra.map(|extra| min + extra);
            let bounds = Bounds {
                min: (min > 0).then_some(min),
                max,
            };
            let lengths: Vec<i64> = (min..=max.unwrap_or(min + 3)).map(i64::from).collect();
            let scalar = json_element().prop_map(|encoder| DraftElement::Scalar(Box::new(encoder)));
            let field = prop_oneof![
                3 => json_element().prop_map(RecordField::List),
                1 => constant().prop_map(|(shape, value)| RecordField::Constant(shape, value)),
            ];
            let records = vec((field, place()), 1..=3)
                .prop_filter("a record takes a list", |fields| {
                    fields
                        .iter()
                        .any(|(field, _)| matches!(field, RecordField::List(_)))
                })
                .prop_map(DraftElement::Records);
            (
                Just(bounds),
                Just(lengths),
                prop_oneof![2 => scalar, 1 => records],
            )
        })
        .prop_map(|(bounds, lengths, element)| ArrayDraft {
            bounds,
            lengths,
            element,
        })
        .boxed()
}

/// A packet's command: up to five arguments a JSON field encodes, half
/// the time a plain double too, up to two constants, maybe a target, up to
/// two arrays, and now and then an argument and a list the set leaves out,
/// its entries in any order.
pub(super) fn command_spec() -> BoxedStrategy<CommandSpec> {
    (
        (
            vec((json_encoder(), proptest::bool::weighted(0.85)), 0..=5),
            proptest::option::weighted(0.5, plain_double()),
            vec(constant(), 0..=2),
            proptest::option::weighted(0.4, target_shape()),
        ),
        vec(array_draft(), 0..=2),
        // Left out by the set (§2.4), so with defaults: of any type, bytes
        // too, which no JSON field holds, and a list.
        proptest::option::weighted(0.3, defaulted()),
        proptest::option::weighted(0.2, left_out_list()),
        vec(place(), 10),
    )
        .prop_flat_map(
            |((arguments, double, constants, target), drafts, left_out, left_out_list, places)| {
                let mut arguments: Vec<(Encoder, bool)> = arguments
                    .into_iter()
                    .map(|(encoder, encoded)| {
                        let encoded = encoded || encoder.default.is_none();
                        (encoder, encoded)
                    })
                    .chain(double.map(|encoder| (encoder, true)))
                    .collect();
                let mut items: Vec<Item> = arguments
                    .iter()
                    .enumerate()
                    .filter(|(_, (_, encoded))| *encoded)
                    .map(|(index, _)| Item::Argument(index))
                    .collect();
                items.extend(
                    constants
                        .into_iter()
                        .map(|(shape, value)| Item::Constant(shape, value)),
                );
                let (arrays, groups) = place_arrays(drafts, &mut arguments);
                items.extend((0..arrays.len()).map(Item::Array));
                items.extend(target.map(Item::Target));
                for encoder in [left_out, left_out_list].into_iter().flatten() {
                    arguments.push((encoder, false));
                }
                let defaults = group_defaults(&groups, &arguments);
                (
                    Just(arguments),
                    Just(items).prop_shuffle(),
                    Just(arrays),
                    Just(groups),
                    defaults,
                    Just(places),
                )
            },
        )
        .prop_map(
            |(mut arguments, items, arrays, mut groups, defaults, places)| {
                for (group, (count, lists)) in groups.iter_mut().zip(defaults) {
                    group.default = count;
                    for ((member, _), list) in group.members.iter().zip(lists) {
                        arguments[*member].0.default = list.map(as_literal);
                    }
                }
                CommandSpec {
                    arguments,
                    items,
                    places,
                    arrays,
                    groups,
                }
            },
        )
        .boxed()
}

/// The arrays, their lists placed among the command's arguments, and each
/// array's group: a scalar array's one list, or its records' lists, which
/// have one length (§3.6). A JSON array has no count, so its lists'
/// lengths are their own.
fn place_arrays(
    drafts: Vec<ArrayDraft>,
    arguments: &mut Vec<(Encoder, bool)>,
) -> (Vec<ArrayElement>, Vec<Group>) {
    let mut arrays = Vec::new();
    let mut groups = Vec::new();
    for draft in drafts {
        let mut members = Vec::new();
        let mut place = |mut encoder: Encoder| {
            encoder.list = Some(draft.bounds);
            arguments.push((encoder, true));
            members.push((arguments.len() - 1, Scale::ONE));
            arguments.len() - 1
        };
        let element = match draft.element {
            DraftElement::Scalar(encoder) => ArrayElement::Scalar(place(*encoder)),
            DraftElement::Records(fields) => ArrayElement::Records(
                fields
                    .into_iter()
                    .map(|(field, at)| match field {
                        RecordField::List(encoder) => (RecordField::List(place(encoder)), at),
                        RecordField::Constant(shape, value) => {
                            (RecordField::Constant(shape, value), at)
                        }
                    })
                    .collect(),
            ),
        };
        groups.push(Group {
            members,
            counts: draft.lengths,
            default: None,
        });
        arrays.push(element);
    }
    (arrays, groups)
}

/// Lays a packet out: its key `op` at `key_place` holding `key`'s value,
/// if it has one, then its items as fields and arrays named `f0`, `f1` and
/// on, and a record's fields `r0`, `r1` and on, each at its place.
pub(super) fn lay_out(
    index: usize,
    spec: CommandSpec,
    key: Option<(Shape, Const)>,
    key_place: &Place,
    apid: Option<i64>,
) -> Layout {
    let field = |name: String, shape: Shape, source: Source, place: &Place| Placed::Field {
        pointer: place.tokens(&name),
        name,
        shape,
        little: false,
        explicit: false,
        source,
    };
    let argument_shape = |argument: usize| spec.arguments[argument].0.shape.clone();
    let mut entries: Vec<Placed> = key
        .into_iter()
        .map(|(shape, value)| field("op".into(), shape, Source::Constant(value), key_place))
        .collect();
    for (position, item) in spec.items.iter().enumerate() {
        let name = format!("f{position}");
        let place = &spec.places[position % spec.places.len()];
        entries.push(match item {
            Item::Argument(argument) => field(
                name,
                argument_shape(*argument),
                Source::Argument(*argument),
                place,
            ),
            Item::Constant(shape, value) => {
                field(name, shape.clone(), Source::Constant(value.clone()), place)
            }
            Item::Target(shape) => field(name, shape.clone(), Source::Target, place),
            Item::Array(array) => {
                let elements = match &spec.arrays[*array] {
                    ArrayElement::Scalar(argument) => Elements::Scalar {
                        argument: *argument,
                        shape: argument_shape(*argument),
                        little: false,
                        explicit: false,
                    },
                    ArrayElement::Records(fields) => Elements::Records {
                        name: format!("t{index}_{position}"),
                        entries: fields
                            .iter()
                            .enumerate()
                            .map(|(j, (record_field, at))| {
                                let name = format!("r{j}");
                                match record_field {
                                    RecordField::List(argument) => field(
                                        name,
                                        argument_shape(*argument),
                                        Source::Argument(*argument),
                                        at,
                                    ),
                                    RecordField::Constant(shape, value) => field(
                                        name,
                                        shape.clone(),
                                        Source::Constant(value.clone()),
                                        at,
                                    ),
                                }
                            })
                            .collect(),
                    },
                };
                Placed::Array {
                    pointer: place.tokens(&name),
                    name,
                    count: Counted::Remaining,
                    elements,
                }
            }
        });
    }
    let statics = vec![true; entries.len()];
    Layout {
        name: format!("C{index}"),
        command: format!("cmd.c{index}"),
        encoding: Encoding::Json,
        little: false,
        apid,
        arguments: spec.arguments,
        entries,
        statics,
        groups: spec.groups,
    }
}

/// What a JSON request encodes to (`spec/encoding.md` §5): each field's
/// raw value at its pointer and each array's elements at its, a record as
/// an object of its fields at their pointers, written as canonical JSON;
/// and what decoding the payload gives back, where −0.0, written `0`, is
/// 0.0.
pub(super) fn pack(layout: &Layout, values: &[Option<Arg>], target: Option<&Target>) -> Packed {
    let raws = read_back(raws(layout, values, target));
    let document = document(layout, &raws);
    let gathered = gathered(layout, raws);
    Packed {
        payload: write(&document).into_bytes(),
        record: gathered.record,
        arguments: gathered.arguments,
        target: gathered.target,
        places: Vec::new(),
        elements: Vec::new(),
    }
}

/// Each entry's raw value as decoding the payload reads it back: −0.0 is
/// written `0`, which reads as 0.0.
pub(super) fn read_back(raws: Vec<Option<Raw>>) -> Vec<Option<Raw>> {
    raws.into_iter().map(|raw| raw.map(positive_zero)).collect()
}

/// The payload's document: each field's raw value at its pointer and each
/// array's elements at its.
pub(super) fn document(layout: &Layout, raws: &[Option<Raw>]) -> Canon {
    let mut document = Canon::Object(Vec::new());
    for (entry, raw) in layout.entries.iter().zip(raws) {
        let Some(raw) = raw else { continue };
        match entry {
            Placed::Field { pointer, .. } => document.put(pointer, scalar(raw)),
            Placed::Array {
                pointer, elements, ..
            } => document.put(pointer, array(elements, raw)),
            Placed::Padding(_) | Placed::Checksum { .. } => {}
        }
    }
    document
}

/// A raw value as decoding a JSON payload reads it back: −0.0 is written
/// `0`, which reads as 0.0.
fn positive_zero(raw: Raw) -> Raw {
    match raw {
        Raw::Float(x) => Raw::Float(if x == 0.0 { 0.0 } else { x }),
        Raw::Array(items) => Raw::Array(items.into_iter().map(positive_zero).collect()),
        Raw::Record(fields) => Raw::Record(
            fields
                .into_iter()
                .map(|(name, raw)| (name, positive_zero(raw)))
                .collect(),
        ),
        raw => raw,
    }
}

/// A field's raw value as JSON.
pub(super) fn scalar(raw: &Raw) -> Canon {
    match raw {
        Raw::Uint(n) => Canon::Int(i128::from(*n)),
        Raw::Int(n) => Canon::Int(i128::from(*n)),
        Raw::Float(x) => Canon::Float(*x),
        Raw::Bool(b) => Canon::Bool(*b),
        Raw::Str(text) => Canon::Str(text.clone()),
        // A JSON packet has no bytes, and fields hold scalars.
        Raw::Bytes(_) | Raw::Array(_) | Raw::Record(_) | Raw::Null => Canon::Array(Vec::new()),
    }
}

/// An array's elements as a JSON array: values, or records as objects.
fn array(elements: &Elements, raw: &Raw) -> Canon {
    let Raw::Array(items) = raw else {
        return Canon::Array(Vec::new());
    };
    let item = |raw: &Raw| match (elements, raw) {
        (Elements::Records { entries, .. }, Raw::Record(fields)) => {
            let mut object = Canon::Object(Vec::new());
            for entry in entries {
                if let Placed::Field { name, pointer, .. } = entry
                    && let Some((_, raw)) = fields.iter().find(|(field, _)| field == name)
                {
                    object.put(pointer, scalar(raw));
                }
            }
            object
        }
        (_, raw) => scalar(raw),
    };
    Canon::Array(items.iter().map(item).collect())
}

/// A pointer's text (RFC 6901): `~` written `~0` before `/` is written
/// `~1`.
pub(super) fn pointer_text(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|token| format!("/{}", token.replace('~', "~0").replace('/', "~1")))
        .collect()
}

/// The payload written another way (`spec/decoding.md` §2.3): members in
/// any order, any whitespace, characters escaped or not, integers spelled
/// any way that keeps them integers (`42.0`, `4.2e1`), doubles any way that
/// reads back as them; and now and then members no field reads, which
/// decoding ignores (`spec/encoding.md` §7.3). It decodes as the payload
/// does.
pub(super) fn respelled(
    layout: &Layout,
    values: &[Option<Arg>],
    target: Option<&Target>,
    seed: u64,
) -> Vec<u8> {
    let document = document(layout, &read_back(raws(layout, values, target)));
    let mut spell = Spell::new(seed);
    let written = respell(&document, &mut spell);
    doc::write(&written, &mut spell).into_bytes()
}

fn respell(value: &Canon, spell: &mut Spell) -> Doc {
    match value {
        Canon::Null => Doc::Null,
        Canon::Int(n) => Doc::Int(*n),
        Canon::Float(x) => Doc::Float(*x),
        Canon::Bool(b) => Doc::Bool(*b),
        Canon::Str(text) => Doc::Str(text.clone()),
        Canon::Literal(text) => Doc::Literal(text.clone()),
        Canon::Array(items) => Doc::Array(items.iter().map(|item| respell(item, spell)).collect()),
        Canon::Object(members) => {
            let mut members: Vec<(String, Doc)> = members
                .iter()
                .map(|(name, member)| (name.clone(), respell(member, spell)))
                .collect();
            // Now and then a member or two no field reads, named so no
            // pointer goes there.
            for k in 0..spell.pick(4).saturating_sub(1) {
                let extra = match spell.pick(5) {
                    0 => Doc::Null,
                    1 => Doc::Bool(true),
                    2 => Doc::Int(spell.pick(100) as i128),
                    3 => Doc::Str("extra".into()),
                    _ => Doc::Array(vec![Doc::Object(Vec::new())]),
                };
                members.push((format!("x{k}"), extra));
            }
            spell.shuffle(&mut members);
            Doc::Object(members)
        }
    }
}
