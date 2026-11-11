//! Command packets: a set's key, each packet's entries after it, and their
//! places, as `spec/decoding.md` §5.2 reads them.

use proptest::collection::{btree_set, vec};
use proptest::prelude::*;
use proptest::sample::{Index, select};

use super::encoders::{encoder, length_field, with_default};
use super::values::text_for;
use super::{ArgType, Const, Encoder, Layout, Len, Placed, Shape, Source, TEXTS, encoder_of};
use crate::checksum::Algorithm;
use crate::layout::{Text, inside_one_byte};

/// One entry of a packet after its key, before it's laid out.
#[derive(Debug, Clone)]
enum Item {
    /// The argument at this index of the command's.
    Argument(usize),
    Constant(Shape, Const),
    Target(Shape),
}

/// The last entry of a packet, whose length is the rest of the payload.
#[derive(Debug, Clone)]
enum Trailer {
    Argument(Box<Encoder>),
    Target(Shape),
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
}

pub(super) fn command_spec() -> BoxedStrategy<CommandSpec> {
    (
        vec((encoder(), proptest::bool::weighted(0.85)), 0..=5),
        vec(constant(), 0..=2),
        proptest::option::weighted(0.4, target_shape()),
        proptest::option::weighted(0.25, trailer()),
        vec((any::<Index>(), checksum(), any::<Index>()), 0..=2),
        vec(any::<bool>(), 8),
        vec(any::<bool>(), 8),
    )
        .prop_flat_map(
            |(arguments, constants, target, trailer, checksums, flips, early)| {
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
                let trailer = trailer.map(|trailer| match trailer {
                    Trailer::Argument(encoder) => {
                        arguments.push((*encoder, true));
                        Item::Argument(arguments.len() - 1)
                    }
                    Trailer::Target(shape) => Item::Target(shape),
                });
                // One target field at most.
                if let (Some(shape), false) = (target, matches!(trailer, Some(Item::Target(_)))) {
                    items.push(Item::Target(shape));
                }
                (
                    Just(arguments),
                    Just(items).prop_shuffle(),
                    Just(flips),
                    Just(early),
                    Just(checksums),
                    Just(trailer),
                )
            },
        )
        .prop_map(
            |(arguments, items, flips, early, checksums, trailer)| CommandSpec {
                arguments,
                items,
                flips,
                early,
                checksums,
                trailer,
            },
        )
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

fn trailer() -> BoxedStrategy<Trailer> {
    let text = with_default(
        select(TEXTS).prop_map(|text| encoder_of(ArgType::Str, Shape::Text(text, Len::Remaining))),
    );
    let bytes = with_default(Just(encoder_of(
        ArgType::Bytes,
        Shape::Bytes(Len::Remaining),
    )));
    prop_oneof![
        text.prop_map(|encoder| Trailer::Argument(Box::new(encoder))),
        bytes.prop_map(|encoder| Trailer::Argument(Box::new(encoder))),
        select(TEXTS).prop_map(|text| Trailer::Target(Shape::Text(text, Len::Remaining))),
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
        if let Some(Len::Field { bits, .. }) = item_shape(item, arguments).len() {
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
        let source = match item {
            Item::Argument(index) => Source::Argument(*index),
            Item::Constant(_, value) => Source::Constant(value.clone()),
            Item::Target(_) => Source::Target,
        };
        self.field(name, item_shape(item, arguments), flip, source);
    }
}

fn item_shape(item: &Item, arguments: &[(Encoder, bool)]) -> Shape {
    match item {
        Item::Argument(index) => arguments[*index].0.shape.clone(),
        Item::Constant(shape, _) | Item::Target(shape) => shape.clone(),
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
    for position in 0..=count {
        for (_, algorithm, from) in checksums.iter().filter(|(at, ..)| *at == position) {
            placer.checksum(format!("c{sums}"), algorithm.clone(), from);
            sums += 1;
        }
        if let Some(item) = spec.items.get(position) {
            let flip = spec.flips.get(position).copied().unwrap_or(false);
            placer.item(
                item,
                flip,
                format!("f{position}"),
                early(position),
                &spec.arguments,
            );
        }
    }
    if let Some(trailer) = &spec.trailer {
        placer.item(trailer, false, "rest".into(), false, &spec.arguments);
    }
    Layout {
        name: format!("C{index}"),
        command: format!("cmd.c{index}"),
        little,
        apid,
        arguments: spec.arguments,
        entries: placer.entries,
        statics: placer.statics,
    }
}
