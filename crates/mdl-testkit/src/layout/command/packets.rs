//! Command packets: a set's key, each packet's entries after it, and their
//! places, as `spec/decoding.md` §5.2 reads them.

use proptest::collection::{btree_set, vec};
use proptest::prelude::*;
use proptest::sample::{Index, select};

use super::encoders::{
    as_literal, element_encoder, encoder, length_field, terminated_encoder, with_default,
};
use super::values::{list_value, text_for};
use super::{
    Arg, ArgType, Bounds, Const, Counted, Elements, Encoder, Group, Layout, Len, Placed, Scale,
    Shape, Source, TEXTS, encoder_of,
};
use crate::checksum::Algorithm;
use crate::layout::{Text, inside_one_byte};

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

/// The last entry of a packet, whose length is the rest of the payload.
#[derive(Debug, Clone)]
enum Trailer {
    Argument(Box<Encoder>),
    Target(Shape),
    Array(Box<ArrayDraft>),
}

/// How an array's count is given, before it's laid out (§3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CountKind {
    Fixed(u32),
    /// A count field, which the arrays with one `id` share.
    Field {
        id: usize,
        bits: u32,
        multiply: u32,
        add: i64,
    },
    Remaining,
}

impl CountKind {
    fn scale(self) -> Scale {
        match self {
            CountKind::Field { multiply, add, .. } => Scale { multiply, add },
            CountKind::Fixed(_) | CountKind::Remaining => Scale::ONE,
        }
    }
}

/// What an array holds, before it's laid out.
#[derive(Debug, Clone)]
enum ElementSpec {
    /// The values of the list argument at this index of the command's.
    Scalar(usize),
    /// Records, whose fields take list arguments or constants. Bit records
    /// are big-endian integers and bools, not padded to whole bytes.
    Records {
        fields: Vec<RecordField<usize>>,
        bits: bool,
    },
}

/// A record's field: one that takes a list argument (`L`), or a constant.
#[derive(Debug, Clone)]
enum RecordField<L> {
    List(L),
    Constant(Shape, Const),
}

#[derive(Debug, Clone)]
struct ArraySpec {
    element: ElementSpec,
    count: CountKind,
}

/// An array as it's generated, before its lists take places among the
/// command's arguments: their element encoders, the bounds they share and
/// the counts it may be given (a count field's raw values, else lengths).
#[derive(Debug, Clone)]
struct ArrayDraft {
    count: CountKind,
    bounds: Bounds,
    counts: Vec<i64>,
    /// A scalar array's one list, or records' fields.
    element: DraftElement,
}

/// A group's default count, if any member has a default, and each
/// member's default list, of the length that count gives it, or none.
type GroupDefaults = (Option<i64>, Vec<Option<Arg>>);

#[derive(Debug, Clone)]
enum DraftElement {
    Scalar(Box<Encoder>),
    Records {
        fields: Vec<RecordField<Encoder>>,
        bits: bool,
    },
}

#[derive(Debug, Clone)]
pub(super) struct CommandSpec {
    /// The catalog's arguments, each with a field that encodes it, and
    /// whether this set's packet uses that field.
    arguments: Vec<(Encoder, bool)>,
    /// The entries after the key, in order.
    items: Vec<Item>,
    /// Whether each item asks for the other byte order than its family's.
    flips: Vec<bool>,
    /// Whether each item whose length a field holds has that field right
    /// after the key, rather than right before the item.
    early: Vec<bool>,
    /// Each checksum: before which item it goes, its algorithm, and which
    /// earlier byte it covers from.
    checksums: Vec<(Index, Algorithm, Index)>,
    trailer: Option<Item>,
    arrays: Vec<ArraySpec>,
    groups: Vec<Group>,
}

pub(super) fn command_spec(little: bool) -> BoxedStrategy<CommandSpec> {
    (
        (
            vec((encoder(), proptest::bool::weighted(0.85)), 0..=5),
            vec(constant(), 0..=2),
            proptest::option::weighted(0.4, target_shape()),
            proptest::option::weighted(0.25, trailer(little)),
        ),
        vec((any::<Index>(), checksum(), any::<Index>()), 0..=2),
        vec(any::<bool>(), 10),
        vec(any::<bool>(), 10),
        vec(array_draft(little), 0..=2),
        any::<bool>(),
        proptest::option::weighted(0.2, left_out_list()),
    )
        .prop_flat_map(
            |(
                (arguments, constants, target, trailer),
                checksums,
                flips,
                early,
                drafts,
                share,
                left_out,
            )| {
                // An argument the set leaves out has a default (§2.4).
                let mut arguments: Vec<(Encoder, bool)> = arguments
                    .into_iter()
                    .map(|(encoder, encoded)| {
                        let encoded = encoded || encoder.default.is_none();
                        (encoder, encoded)
                    })
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
                let trailer_draft = match &trailer {
                    Some(Trailer::Array(draft)) => Some((**draft).clone()),
                    _ => None,
                };
                let (arrays, groups) = place_arrays(drafts, trailer_draft, share, &mut arguments);
                let trailer_array = matches!(trailer, Some(Trailer::Array(_)));
                for index in 0..arrays.len() - usize::from(trailer_array) {
                    items.push(Item::Array(index));
                }
                if let Some(encoder) = left_out {
                    arguments.push((encoder, false));
                }
                let trailer = trailer.map(|trailer| match trailer {
                    Trailer::Argument(encoder) => {
                        arguments.push((*encoder, true));
                        Item::Argument(arguments.len() - 1)
                    }
                    Trailer::Target(shape) => Item::Target(shape),
                    Trailer::Array(_) => Item::Array(arrays.len() - 1),
                });
                // One target field at most.
                if let (Some(shape), false) = (target, matches!(trailer, Some(Item::Target(_)))) {
                    items.push(Item::Target(shape));
                }
                let defaults = group_defaults(&groups, &arguments);
                (
                    Just(arguments),
                    Just(items).prop_shuffle(),
                    Just(flips),
                    Just(early),
                    Just(checksums),
                    Just(trailer),
                    Just(arrays),
                    Just(groups),
                    defaults,
                )
            },
        )
        .prop_map(
            |(
                mut arguments,
                items,
                flips,
                early,
                checksums,
                trailer,
                arrays,
                mut groups,
                defaults,
            )| {
                for (group, (count, lists)) in groups.iter_mut().zip(defaults) {
                    group.default = count;
                    for ((member, _), list) in group.members.iter().zip(lists) {
                        arguments[*member].0.default = list.map(as_literal);
                    }
                }
                CommandSpec {
                    arguments,
                    items,
                    flips,
                    early,
                    checksums,
                    trailer,
                    arrays,
                    groups,
                }
            },
        )
        .boxed()
}

/// The arrays, their lists placed among the command's arguments, and the
/// groups those lists form: each array's own, which the second of two
/// arrays that share a count field joins. When `share` and both arrays
/// have count fields, the second takes its count from the first's field,
/// so the field's size and the counts it holds are the first's (those that
/// give the second array a length of at least 0); its `multiply` and `add`
/// stay its own.
fn place_arrays(
    drafts: Vec<ArrayDraft>,
    trailer: Option<ArrayDraft>,
    share: bool,
    arguments: &mut Vec<(Encoder, bool)>,
) -> (Vec<ArraySpec>, Vec<Group>) {
    let mut drafts = drafts;
    let mut shared = false;
    if share
        && let [first, second] = drafts.as_mut_slice()
        && let (CountKind::Field { bits, .. }, CountKind::Field { multiply, add, .. }) =
            (first.count, second.count)
    {
        let scale = Scale { multiply, add };
        let counts: Vec<i64> = first
            .counts
            .iter()
            .copied()
            .filter(|count| scale.reaches(*count))
            .collect();
        if !counts.is_empty() {
            second.count = CountKind::Field {
                id: 0,
                bits,
                multiply,
                add,
            };
            second.bounds = bounds_of(scale, &counts);
            second.counts = counts;
            shared = true;
        }
    }
    let mut arrays = Vec::new();
    let mut groups: Vec<Group> = Vec::new();
    for (index, draft) in drafts.into_iter().chain(trailer).enumerate() {
        // A count field's id: the array's own, or the first's for the
        // second array of two that share it.
        let joins = shared && index == 1;
        let count = match draft.count {
            CountKind::Field {
                bits,
                multiply,
                add,
                ..
            } => CountKind::Field {
                id: if joins { 0 } else { index },
                bits,
                multiply,
                add,
            },
            other => other,
        };
        let scale = count.scale();
        let mut place = |encoder: Encoder| {
            let mut encoder = encoder;
            encoder.list = Some(draft.bounds);
            arguments.push((encoder, true));
            (arguments.len() - 1, scale)
        };
        let (element, members) = match draft.element {
            DraftElement::Scalar(encoder) => {
                let member = place(*encoder);
                (ElementSpec::Scalar(member.0), vec![member])
            }
            DraftElement::Records { fields, bits } => {
                let mut members = Vec::new();
                let fields = fields
                    .into_iter()
                    .map(|field| match field {
                        RecordField::List(encoder) => {
                            let member = place(encoder);
                            members.push(member);
                            RecordField::List(member.0)
                        }
                        RecordField::Constant(shape, value) => RecordField::Constant(shape, value),
                    })
                    .collect();
                (ElementSpec::Records { fields, bits }, members)
            }
        };
        let counts = draft.counts;
        match groups.first_mut() {
            Some(group) if joins => {
                group.members.extend(members);
                // The counts both arrays can be given.
                group.counts.retain(|count| counts.contains(count));
            }
            _ => groups.push(Group {
                members,
                counts,
                default: None,
            }),
        }
        arrays.push(ArraySpec { element, count });
    }
    (arrays, groups)
}

/// Each group's defaults: maybe a count, then for most members a list of
/// the length that count gives it, the others left without one (§2.5).
fn group_defaults(
    groups: &[Group],
    arguments: &[(Encoder, bool)],
) -> Vec<BoxedStrategy<GroupDefaults>> {
    groups
        .iter()
        .map(|group| {
            let members: Vec<(Encoder, Scale)> = group
                .members
                .iter()
                .map(|(member, scale)| (arguments[*member].0.clone(), *scale))
                .collect();
            let size = members.len();
            (
                proptest::option::weighted(0.5, select(group.counts.clone())),
                vec(proptest::bool::weighted(0.8), size),
            )
                .prop_flat_map(move |(count, with)| {
                    let lists: Vec<BoxedStrategy<Option<Arg>>> = members
                        .iter()
                        .zip(&with)
                        .map(|((encoder, scale), with)| match count {
                            Some(count) if *with => list_value(encoder, scale.length(count))
                                .prop_map(Some)
                                .boxed(),
                            _ => Just(None).boxed(),
                        })
                        .collect();
                    let count = count.filter(|_| with.iter().any(|with| *with));
                    (Just(count), lists)
                })
                .boxed()
        })
        .collect()
}

/// The bounds of a list whose array takes these counts: the lengths they
/// give, from the least to the most. A `min` of 0 is the default, which is
/// left out.
fn bounds_of(scale: Scale, counts: &[i64]) -> Bounds {
    let least = counts.first().map_or(0, |count| scale.length(*count)) as u32;
    let most = counts.last().map_or(0, |count| scale.length(*count)) as u32;
    Bounds {
        min: (least > 0).then_some(least),
        max: Some(most),
    }
}

/// An array's count other than `remaining`, the bounds its lists then
/// have, and the counts it may be given. A fixed count is the bounds' `min`
/// and `max` both; a count field holds `max`, and its `add` is no more than
/// `min` (`spec/encoding.md` §3.6).
fn counted() -> BoxedStrategy<(CountKind, Bounds, Vec<i64>)> {
    let fixed = (0_u32..=4).prop_map(|count| {
        let kind = CountKind::Fixed(count);
        let counts = vec![i64::from(count)];
        (kind, bounds_of(kind.scale(), &counts), counts)
    });
    let field = (2_u32..=6, 1_u32..=3, -2_i64..=2)
        .prop_flat_map(|(bits, multiply, add)| {
            let step = i64::from(multiply);
            // The least raw value that counts at least 0 values.
            let least = if add >= 0 {
                0
            } else {
                (-add + step - 1) / step
            };
            let most = (1_i64 << bits) - 1;
            (
                Just((bits, multiply, add)),
                least..=(least + 2).min(most),
                0_i64..=2,
            )
                .prop_map(move |(shape, low, span)| (shape, low, (low + span).min(most)))
        })
        .prop_map(|((bits, multiply, add), low, high)| {
            // Its id is set where the array takes its place.
            let kind = CountKind::Field {
                id: 0,
                bits,
                multiply,
                add,
            };
            let counts: Vec<i64> = (low..=high).collect();
            (kind, bounds_of(kind.scale(), &counts), counts)
        });
    prop_oneof![1 => fixed, 2 => field].boxed()
}

/// An array of a list argument's values or of records. Where the count
/// varies, its elements are whole bytes (integers laid out whole), so what
/// follows keeps its bits; bit records go only in a big-endian family,
/// with a fixed count.
fn array_draft(little: bool) -> BoxedStrategy<ArrayDraft> {
    counted()
        .prop_flat_map(move |(count, bounds, counts)| {
            let variable = !matches!(count, CountKind::Fixed(_));
            draft(count, bounds, counts, little, variable)
        })
        .boxed()
}

fn draft(
    count: CountKind,
    bounds: Bounds,
    counts: Vec<i64>,
    little: bool,
    variable: bool,
) -> BoxedStrategy<ArrayDraft> {
    let make = move |element: DraftElement| ArrayDraft {
        count,
        bounds,
        counts: counts.clone(),
        element,
    };
    let scalar = element_encoder(variable, false)
        .prop_map(|encoder| DraftElement::Scalar(Box::new(encoder)));
    // A fixed count of bools, one bit each: flags packed into bits.
    let flags = Just(DraftElement::Scalar(Box::new(encoder_of(
        ArgType::Bool,
        Shape::Bool,
    ))));
    let records = |bits: bool| {
        vec(record_field(bits), 1..=3)
            .prop_filter("a record takes a list", |fields| {
                fields
                    .iter()
                    .any(|field| matches!(field, RecordField::List(_)))
            })
            .prop_map(move |fields| DraftElement::Records { fields, bits })
    };
    let element = match (little, variable) {
        (_, true) => prop_oneof![2 => scalar, 1 => records(false)].boxed(),
        (true, false) => prop_oneof![4 => scalar, 1 => flags, 2 => records(false)].boxed(),
        (false, false) => prop_oneof![
            4 => scalar,
            1 => flags,
            2 => records(false),
            2 => records(true),
        ]
        .boxed(),
    };
    element.prop_map(make).boxed()
}

/// A record's field: a list argument's element, or a constant. A bit
/// record's are big-endian integers and bools.
fn record_field(bits: bool) -> BoxedStrategy<RecordField<Encoder>> {
    let constant = if bits {
        prop_oneof![
            (1_u32..=12).prop_map(Shape::Uint),
            (2_u32..=12).prop_map(Shape::Int),
            Just(Shape::Bool),
        ]
        .prop_flat_map(|shape| {
            let value = match shape {
                Shape::Bool => any::<bool>().prop_map(Const::Bool).boxed(),
                _ => shape
                    .integers()
                    .unwrap_or(0..=0)
                    .prop_map(Const::Int)
                    .boxed(),
            };
            (Just(shape), value)
        })
        .boxed()
    } else {
        constant()
    };
    prop_oneof![
        3 => element_encoder(false, bits).prop_map(RecordField::List),
        1 => constant.prop_map(|(shape, value)| RecordField::Constant(shape, value)),
    ]
    .boxed()
}

/// The last entry's array: `remaining` elements, whole bytes, a third of
/// the time strings or bytes a terminator ends, whose size varies.
fn remaining_draft(little: bool) -> BoxedStrategy<ArrayDraft> {
    (0_u32..=2, proptest::option::of(0_u32..=3))
        .prop_flat_map(move |(min, extra)| {
            let max = extra.map(|extra| min + extra);
            let bounds = Bounds {
                min: (min > 0).then_some(min),
                max,
            };
            let counts: Vec<i64> = (min..=max.unwrap_or(min + 3)).map(i64::from).collect();
            let terminated = counts.clone();
            prop_oneof![
                2 => draft(CountKind::Remaining, bounds, counts, little, true),
                1 => terminated_encoder().prop_map(move |encoder| ArrayDraft {
                    count: CountKind::Remaining,
                    bounds,
                    counts: terminated.clone(),
                    element: DraftElement::Scalar(Box::new(encoder)),
                }),
            ]
        })
        .boxed()
}

/// A list argument the set leaves out (§2.4), so it has a default: any
/// element, any bounds.
fn left_out_list() -> BoxedStrategy<Encoder> {
    (
        element_encoder(false, false),
        0_u32..=2,
        proptest::option::of(0_u32..=3),
    )
        .prop_flat_map(|(encoder, min, extra)| {
            let max = extra.map(|extra| min + extra);
            let mut encoder = encoder;
            encoder.list = Some(Bounds {
                min: (min > 0).then_some(min),
                max,
            });
            let length = min..=max.unwrap_or(min + 3);
            (Just(encoder), length)
        })
        .prop_flat_map(|(encoder, length)| {
            let values = list_value(&encoder, length as usize);
            (Just(encoder), values)
        })
        .prop_map(|(mut encoder, default)| {
            encoder.default = Some(as_literal(default));
            encoder
        })
        .boxed()
}

fn constant() -> BoxedStrategy<(Shape, Const)> {
    let integer = prop_oneof![
        (1_u32..=64).prop_map(Shape::Uint),
        (1_u32..=64).prop_map(Shape::Int)
    ]
    .prop_flat_map(|shape| {
        let values = shape.integers().unwrap_or(0..=0);
        (Just(shape), values.prop_map(Const::Int))
    });
    let text = (
        select(TEXTS),
        prop_oneof![
            (1_usize..=6).prop_map(Len::Fixed),
            (1_usize..=6, any::<u8>()).prop_map(|(n, byte)| Len::Padded(n, byte)),
        ],
    )
        .prop_flat_map(|(text, len)| {
            let value = text_for(text, &len).prop_map(Const::Text);
            (Just(Shape::Text(text, len)), value)
        });
    prop_oneof![
        2 => integer,
        1 => any::<bool>().prop_map(|b| (Shape::Bool, Const::Bool(b))),
        1 => text,
    ]
    .boxed()
}

fn target_shape() -> BoxedStrategy<Shape> {
    let lengths = prop_oneof![
        (1_usize..=8).prop_map(Len::Fixed),
        (1_usize..=8, any::<u8>()).prop_map(|(n, byte)| Len::Padded(n, byte)),
        any::<u8>().prop_map(Len::Terminator),
        length_field(),
    ];
    prop_oneof![
        (1_u32..=64).prop_map(Shape::Uint),
        (1_u32..=64).prop_map(Shape::Int),
        (select(TEXTS), lengths).prop_map(|(text, len)| Shape::Text(text, len)),
    ]
    .boxed()
}

fn trailer(little: bool) -> BoxedStrategy<Trailer> {
    let text = with_default(
        select(TEXTS).prop_map(|text| encoder_of(ArgType::Str, Shape::Text(text, Len::Remaining))),
    );
    let bytes = with_default(Just(encoder_of(
        ArgType::Bytes,
        Shape::Bytes(Len::Remaining),
    )));
    prop_oneof![
        1 => text.prop_map(|encoder| Trailer::Argument(Box::new(encoder))),
        1 => bytes.prop_map(|encoder| Trailer::Argument(Box::new(encoder))),
        1 => select(TEXTS).prop_map(|text| Trailer::Target(Shape::Text(text, Len::Remaining))),
        2 => remaining_draft(little).prop_map(|draft| Trailer::Array(Box::new(draft))),
    ]
    .boxed()
}

/// Any checksum: a CRC of any width and parameters, a sum, an xor or the
/// ISO checksum.
fn checksum() -> BoxedStrategy<Algorithm> {
    let width = select(&[8_u32, 16, 24, 32, 40, 48, 56, 64][..]);
    let crc = (
        width.clone(),
        any::<(u64, u64, u64)>(),
        any::<(bool, bool)>(),
    )
        .prop_map(
            |(width, (poly, init, xor_out), (reflect_in, reflect_out))| {
                let mask = if width == 64 {
                    u64::MAX
                } else {
                    (1 << width) - 1
                };
                Algorithm::Crc {
                    width,
                    poly: poly & mask,
                    init: init & mask,
                    reflect_in,
                    reflect_out,
                    xor_out: xor_out & mask,
                }
            },
        );
    prop_oneof![
        3 => crc,
        1 => width.prop_map(|width| Algorithm::Sum { width }),
        1 => Just(Algorithm::Xor),
        1 => Just(Algorithm::Iso8473),
    ]
    .boxed()
}

/// The first field of every packet, and each packet's value of it: an
/// integer, or a tag in a fixed-length buffer. With a terminator, every
/// tag has one length, so the tags are values of one key (§7.2).
pub(super) fn key(count: usize) -> BoxedStrategy<(Shape, Vec<Const>)> {
    let sizes = select(&[2_u32, 3, 4, 5, 6, 7, 8, 16][..]);
    let integer = prop_oneof![
        sizes.clone().prop_map(Shape::Uint),
        sizes.prop_map(Shape::Int)
    ]
    .prop_flat_map(move |shape| {
        let values = btree_set(shape.integers().unwrap_or(0..=0), count)
            .prop_map(|values| values.into_iter().map(Const::Int).collect::<Vec<_>>())
            .prop_shuffle();
        (Just(shape), values)
    });
    let tag = (2_usize..=6, proptest::option::of(select(&[0_u8, b' '][..])))
        .prop_flat_map(|(bytes, terminator)| {
            let length = match terminator {
                Some(_) => (1..=bytes).boxed(),
                None => Just(bytes).boxed(),
            };
            (Just(bytes), Just(terminator), length)
        })
        .prop_flat_map(move |(bytes, terminator, length)| {
            let len = match terminator {
                Some(byte) => Len::Padded(bytes, byte),
                None => Len::Fixed(bytes),
            };
            let word = vec(select(&b"abcdefghij"[..]), length)
                .prop_map(|letters| letters.into_iter().map(char::from).collect::<String>());
            let tags = btree_set(word, count)
                .prop_map(|tags| tags.into_iter().map(Const::Text).collect::<Vec<_>>())
                .prop_shuffle();
            (Just(Shape::Text(Text::Ascii, len)), tags)
        });
    prop_oneof![2 => integer, 1 => tag].boxed()
}

/// Places entries one after another, as `decoding.md` §5.2 reads them.
/// `at` is where the next entry starts if every value of varying length is
/// empty; those take whole bytes, so `at` modulo 8 is exact either way.
struct Placer {
    little: bool,
    at: u64,
    /// Whether `at` is the next entry's position in every payload.
    fixed: bool,
    /// Byte-aligned positions of earlier entries, in every payload.
    starts: Vec<u64>,
    entries: Vec<Placed>,
    /// Whether each entry starts where it does in every payload.
    statics: Vec<bool>,
}

impl Placer {
    fn push(&mut self, entry: Placed) {
        self.statics.push(self.fixed);
        self.entries.push(entry);
    }

    fn pad_to_byte(&mut self) {
        let skip = (8 - self.at % 8) % 8;
        if skip > 0 {
            self.push(Placed::Padding(skip));
            self.at += skip;
        }
    }

    fn note_start(&mut self) {
        if self.fixed && self.at.is_multiple_of(8) && !self.starts.contains(&self.at) {
            self.starts.push(self.at);
        }
    }

    /// Whether a value of `bits` at the next position spans bytes.
    fn spans(&self, bits: Option<u32>) -> bool {
        bits.is_some_and(|bits| !inside_one_byte(self.at, bits))
    }

    /// Strings and bytes start on a byte; little-endian numbers that span
    /// bytes are whole bytes on byte boundaries (widened if need be); a
    /// byte order is spelled only where it differs from the family's and
    /// matters (§2.1, §2.2).
    fn field(&mut self, name: String, shape: Shape, flip: bool, source: Source) {
        let mut shape = shape;
        let mut little = self.little;
        let mut explicit = false;
        if flip && shape.ordered() {
            little = !little;
            explicit = true;
        }
        let ordered_little = little && shape.ordered();
        if matches!(shape, Shape::Text(..) | Shape::Bytes(_))
            || (ordered_little && self.spans(shape.bits()))
        {
            self.pad_to_byte();
        }
        if let Some(bits) = shape.bits()
            && ordered_little
            && self.spans(Some(bits))
            && bits % 8 != 0
        {
            shape = shape.widened(bits.div_ceil(8) * 8);
        }
        if explicit && !self.spans(shape.bits()) {
            explicit = false;
            little = self.little;
        }
        self.note_start();
        let bits = shape.bits();
        self.push(Placed::Field {
            name,
            shape,
            little,
            explicit,
            source,
        });
        match bits {
            Some(bits) => self.at += u64::from(bits),
            None => self.fixed = false,
        }
    }

    /// A checksum on a byte boundary, covering from an earlier byte-aligned
    /// position every payload has.
    fn checksum(&mut self, name: String, algorithm: Algorithm, from: &Index) {
        self.pad_to_byte();
        let candidates: Vec<u64> = self
            .starts
            .iter()
            .copied()
            .filter(|start| *start < self.at)
            .collect();
        let from_bits = match candidates.len() {
            0 => 0,
            count => candidates[from.index(count)],
        };
        self.note_start();
        self.at += u64::from(algorithm.bits());
        self.push(Placed::Checksum {
            name,
            algorithm,
            from_bits,
        });
    }

    /// The field that holds an item's length, if a field does.
    fn length_field(&mut self, item: &Item, name: &str, arguments: &[(Encoder, bool)]) {
        if let Some(Len::Field { bits, .. }) =
            item_shape(item, arguments).as_ref().and_then(Shape::len)
        {
            self.field(
                format!("{name}_len"),
                Shape::Uint(*bits),
                false,
                Source::Length(name.to_string()),
            );
        }
    }

    /// An item, after the field that holds its length unless that field
    /// is placed already.
    fn item(
        &mut self,
        item: &Item,
        flip: bool,
        name: String,
        measured: bool,
        arguments: &[(Encoder, bool)],
    ) {
        if !measured {
            self.length_field(item, &name, arguments);
        }
        let (source, shape) = match item {
            Item::Argument(index) => (Source::Argument(*index), arguments[*index].0.shape.clone()),
            Item::Constant(shape, value) => (Source::Constant(value.clone()), shape.clone()),
            Item::Target(shape) => (Source::Target, shape.clone()),
            // Laid out by `array`.
            Item::Array(_) => return,
        };
        self.field(name, shape, flip, source);
    }

    /// An array (§3.6), after its count field unless an array that shares
    /// it placed it already. Each element starts where the last ended:
    /// strings, bytes and little-endian numbers start on a byte, which the
    /// array's start then keeps, every element being whole bytes. Records
    /// are laid out from their own start, byte records padded to whole
    /// bytes. A fixed count of elements of one size keeps every later
    /// position static.
    fn array(
        &mut self,
        name: String,
        spec: &ArraySpec,
        flip: bool,
        arguments: &[(Encoder, bool)],
        counts: &mut Vec<(usize, String)>,
        type_name: String,
    ) {
        let count = match spec.count {
            CountKind::Fixed(count) => Counted::Fixed(count),
            CountKind::Remaining => Counted::Remaining,
            CountKind::Field {
                id,
                bits,
                multiply,
                add,
            } => {
                let field = match counts.iter().find(|(placed, _)| *placed == id) {
                    Some((_, field)) => field.clone(),
                    None => {
                        let field = format!("{name}_n");
                        self.field(field.clone(), Shape::Uint(bits), false, Source::Count);
                        counts.push((id, field.clone()));
                        field
                    }
                };
                Counted::Field {
                    name: field,
                    multiply,
                    add,
                }
            }
        };
        // Reading to the packet's end, which is on a byte, it starts on one
        // (`decoding.md` §5.2).
        if matches!(spec.count, CountKind::Remaining) {
            self.pad_to_byte();
        }
        // A varying count keeps what follows on its bits only with whole
        // bytes; a little-endian integer is whole bytes wherever it starts.
        let whole = !matches!(spec.count, CountKind::Fixed(_));
        let (elements, element_bits) = match &spec.element {
            ElementSpec::Scalar(argument) => {
                let mut shape = arguments[*argument].0.shape.clone();
                let mut little = self.little;
                let mut explicit = false;
                if flip && shape.ordered() && shape.bits().is_some_and(|bits| bits >= 16) {
                    little = !little;
                    explicit = true;
                }
                if let Some(bits) = shape.bits()
                    && shape.ordered()
                    && bits % 8 != 0
                    && (whole || little)
                {
                    shape = shape.widened(bits.div_ceil(8) * 8);
                }
                if matches!(shape, Shape::Text(..) | Shape::Bytes(_))
                    || (shape.ordered() && little)
                    || explicit
                {
                    self.pad_to_byte();
                }
                let bits = shape.bits();
                let elements = Elements::Scalar {
                    argument: *argument,
                    shape,
                    little,
                    explicit,
                };
                (elements, bits)
            }
            ElementSpec::Records { fields, bits } => {
                let mut record = Placer {
                    little: self.little,
                    at: 0,
                    fixed: true,
                    starts: Vec::new(),
                    entries: Vec::new(),
                    statics: Vec::new(),
                };
                for (position, field) in fields.iter().enumerate() {
                    let (shape, source) = match field {
                        RecordField::List(argument) => (
                            arguments[*argument].0.shape.clone(),
                            Source::Argument(*argument),
                        ),
                        RecordField::Constant(shape, value) => {
                            (shape.clone(), Source::Constant(value.clone()))
                        }
                    };
                    record.field(format!("r{position}"), shape, false, source);
                }
                if !bits {
                    record.pad_to_byte();
                    self.pad_to_byte();
                }
                let size = record.fixed.then_some(record.at as u32);
                let elements = Elements::Records {
                    name: type_name,
                    entries: record.entries,
                };
                (elements, size)
            }
        };
        self.note_start();
        self.push(Placed::Array {
            name,
            count,
            elements,
        });
        match (spec.count, element_bits) {
            (CountKind::Fixed(count), Some(bits)) => {
                self.at += u64::from(count) * u64::from(bits);
            }
            _ => self.fixed = false,
        }
    }
}

fn item_shape(item: &Item, arguments: &[(Encoder, bool)]) -> Option<Shape> {
    match item {
        Item::Argument(index) => Some(arguments[*index].0.shape.clone()),
        Item::Constant(shape, _) | Item::Target(shape) => Some(shape.clone()),
        Item::Array(_) => None,
    }
}

pub(super) fn lay_out(
    index: usize,
    spec: CommandSpec,
    key: (Shape, Const),
    little: bool,
    apid: Option<i64>,
) -> Layout {
    let mut placer = Placer {
        little,
        at: 0,
        fixed: true,
        starts: Vec::new(),
        entries: Vec::new(),
        statics: Vec::new(),
    };
    placer.field("op".into(), key.0, false, Source::Constant(key.1));
    let early = |position: usize| spec.early.get(position).copied().unwrap_or(false);
    for (position, item) in spec.items.iter().enumerate() {
        if early(position) {
            placer.length_field(item, &format!("f{position}"), &spec.arguments);
        }
    }
    let count = spec.items.len();
    let checksums: Vec<(usize, Algorithm, Index)> = spec
        .checksums
        .into_iter()
        .map(|(before, algorithm, from)| (before.index(count + 1), algorithm, from))
        .collect();
    let mut sums = 0;
    let mut counts = Vec::new();
    for position in 0..=count {
        for (_, algorithm, from) in checksums.iter().filter(|(at, ..)| *at == position) {
            placer.checksum(format!("c{sums}"), algorithm.clone(), from);
            sums += 1;
        }
        let flip = spec.flips.get(position).copied().unwrap_or(false);
        match spec.items.get(position) {
            Some(Item::Array(array)) => placer.array(
                format!("f{position}"),
                &spec.arrays[*array],
                flip,
                &spec.arguments,
                &mut counts,
                format!("t{index}_{position}"),
            ),
            Some(item) => placer.item(
                item,
                flip,
                format!("f{position}"),
                early(position),
                &spec.arguments,
            ),
            None => {}
        }
    }
    match &spec.trailer {
        Some(Item::Array(array)) => placer.array(
            "rest".into(),
            &spec.arrays[*array],
            false,
            &spec.arguments,
            &mut counts,
            format!("t{index}_rest"),
        ),
        Some(trailer) => placer.item(trailer, false, "rest".into(), false, &spec.arguments),
        None => {}
    }
    Layout {
        name: format!("C{index}"),
        command: format!("cmd.c{index}"),
        little,
        apid,
        arguments: spec.arguments,
        entries: placer.entries,
        statics: placer.statics,
        groups: spec.groups,
    }
}
