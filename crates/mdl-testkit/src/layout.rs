//! Valid layouts and the messages they decode, built from this module's
//! own reading of `spec/decoding.md`, not from `mdl`: the oracle the
//! decoder is checked against. A generated [`Telemetry`] is a binary family
//! with one set of up to three packets, each a run of fields, arrays and
//! structs of every scalar type, size, length, position and byte order
//! validation allows, mapped to catalog params; and messages packed bit by
//! bit (§2.1), with what each decodes to: every raw value, and every
//! sample. [`damaged`] plants faults in such messages, with the error each
//! then gives (§5, §9).
//!
//! Positions and meaning follow the spec's rules as written there:
//! little-endian values are whole bytes on byte boundaries or sit inside
//! one byte, strings and bytes start on a byte, a `byte_order` is spelled
//! only where it differs from the family's and matters; conversions
//! (linear, poly, tables, splines, states) are evaluated in binary64 as
//! §7.1 writes them, then from the field's unit into the param's
//! (`units.md` §4); sentinels and qualities follow §7.2 and §7.3.

use std::collections::BTreeSet;
use std::ops::{Range, RangeInclusive};

use body::faults::{Short, Step};
use body::{Body, BodySpec, PacketLength, Writer, body_spec, fixed_checks};
pub use faults::{
    Family, FaultKind, Garbling, Gone, Held, JsonKind, Place, Route, Tamper, Unfit, Wanted,
};
pub use fleet::{Fleet, Meta, Sent, fleet};
pub use json::{json_damaged, json_telemetry};
use meaning::{UNIT_GROUPS, convert, convert_unit, table};
use proptest::collection::{btree_set, vec};
use proptest::prelude::*;
use proptest::sample::select;
use serde_json::{Map, Value, json};
pub use time::EventTime;
use time::{
    Count, SECOND, TimeFields, TimeSource, count_value, epoch, event_time, near_leap, no_reading,
    received, time_fields, time_json, time_scale, time_source, to_utc, unit, week_value,
};
pub(crate) use time::{instant_text, label};

/// A family, its one set, and messages for it.
#[derive(Debug, Clone)]
pub struct Telemetry {
    /// The bundle: a catalog, the family and the set.
    pub bundle: Value,
    pub family: String,
    pub set: String,
    /// The text the family's fixed alias reads, which a binding names.
    pub native_id: String,
    pub messages: Vec<Message>,
}

/// One packed message and what it decodes to.
#[derive(Debug, Clone)]
pub struct Message {
    pub packet: String,
    pub payload: Vec<u8>,
    /// Each field's raw value, in packet order.
    pub record: Vec<(String, Raw)>,
    /// Each sample, in record order.
    pub samples: Vec<Sample>,
    /// Its ground time, in nanoseconds since 1970-01-01T00:00:00Z.
    pub received: i128,
    pub event_time: EventTime,
    /// For a damaged message, what was done to it and the error decoding
    /// it gives. Its record, samples and event time are then those of the
    /// message before it was damaged.
    pub damage: Option<Damage>,
}

/// Faults planted in a message, and the error they give.
#[derive(Debug, Clone)]
pub struct Damage {
    /// Each fault's route, one or two of them.
    pub routes: Vec<Route>,
    /// What was planted, for a failing case's report.
    pub planted: String,
    pub failure: Failure,
}

/// Why a message can't be decoded (`spec/decoding.md` §5, §9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// The path of the entry the error names, every index included
    /// (`cells[1].volts`). Padding has no path, so for padding it's its
    /// container's, empty at a packet's top level; empty too for a packet
    /// too short for its length, which names no entry.
    pub path: String,
    pub problem: Problem,
}

/// What went wrong at an entry, with what the error reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// `past_end`: reading needs the packet's bits up to `needed_bits`,
    /// but it has `available_bits`. `padding` when what runs past is
    /// padding.
    PastEnd {
        needed_bits: u64,
        available_bits: u64,
        padding: bool,
    },
    /// `no_terminator`: no `byte` ends the value before the packet ends.
    NoTerminator { byte: u8 },
    /// `invalid_text`: the bytes aren't text in `encoding`, which is
    /// spelled as the IR spells encodings (`utf8`, `ascii`).
    InvalidText { encoding: &'static str },
    /// `bad_length`: a count or length from a field is `value`, below 0.
    BadLength { value: i128 },
    /// `too_short`: the payload has `given_bytes`, fewer than the packet's
    /// `length` of `needed_bytes`, `exact` or at least (§5.1).
    TooShort {
        needed_bytes: u32,
        given_bytes: usize,
        exact: bool,
    },
    /// `bad_checksum`: the checksum holds `stored`, which its algorithm
    /// doesn't accept for the bytes it covers, whose value is `computed`
    /// (§5.6).
    BadChecksum { stored: u64, computed: u64 },
    /// `missing_value`: nothing, or `null`, at a required JSON entry's
    /// `pointer` (§5.3).
    Missing { pointer: String },
    /// `wrong_type`: a JSON entry that takes `wanted` found a value of
    /// another kind (§5.3).
    WrongType { wanted: Wanted, found: JsonKind },
    /// `out_of_range`: a number that isn't an integer (§2.3) in the
    /// range of a `uint` field, or of an `int` one when `signed`.
    OutOfRange { signed: bool },
}

impl Problem {
    /// Its code (§9).
    pub fn code(&self) -> &'static str {
        match self {
            Problem::PastEnd { .. } => "past_end",
            Problem::NoTerminator { .. } => "no_terminator",
            Problem::InvalidText { .. } => "invalid_text",
            Problem::BadLength { .. } => "bad_length",
            Problem::TooShort { .. } => "too_short",
            Problem::BadChecksum { .. } => "bad_checksum",
            Problem::Missing { .. } => "missing_value",
            Problem::WrongType { .. } => "wrong_type",
            Problem::OutOfRange { .. } => "out_of_range",
        }
    }
}

/// A raw value, as decoding reads it before any conversion. Floats
/// compare by their bits, so -0.0 isn't 0.0 (every NaN is a NaN: Rust
/// doesn't promise a NaN's payload through a conversion).
#[derive(Debug, Clone)]
pub enum Raw {
    Uint(u64),
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Bytes(Vec<u8>),
    /// An array's elements, in order.
    Array(Vec<Raw>),
    /// A record: each of its fields' raw values, by name, in order.
    Record(Vec<(String, Raw)>),
    /// No reading: a `null` element of a JSON array, which keeps its
    /// position (§5.3).
    Null,
}

/// A sample (`spec/decoding.md` §7.3).
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub param: String,
    pub value: Expected,
    /// The record path of its value, every index included (`f2[1].f0`);
    /// for a composite, its members' paths joined by `,`.
    pub path: String,
    /// The value's position in its innermost array, for a value inside
    /// one.
    pub index: Option<u64>,
    /// For an element of an array with `sample_interval_ms`: how long after
    /// the packet's event time it was taken, in nanoseconds.
    pub time_offset_ns: Option<i64>,
    /// The quality's name, as `spec/decoding.md` §7.3 spells it.
    pub quality: &'static str,
}

/// A sample's value, by its param's type. Floats compare as [`Raw`]'s do.
#[derive(Debug, Clone)]
pub enum Expected {
    F64(f64),
    I64(i64),
    U64(u64),
    Bool(bool),
    Str(String),
    Bytes(Vec<u8>),
    Enum(String),
    /// A composite's members in order (x, y, z, or w, x, y, z), in its
    /// source's frame, spelled as the IR spells frames (§7.4).
    Vector {
        frame: String,
        members: Vec<f64>,
    },
}

/// Two floats are the same value: the same bits, or both NaN.
fn same_float(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

impl PartialEq for Raw {
    fn eq(&self, other: &Raw) -> bool {
        match (self, other) {
            (Raw::Uint(a), Raw::Uint(b)) => a == b,
            (Raw::Int(a), Raw::Int(b)) => a == b,
            (Raw::Float(a), Raw::Float(b)) => same_float(*a, *b),
            (Raw::Bool(a), Raw::Bool(b)) => a == b,
            (Raw::Str(a), Raw::Str(b)) => a == b,
            (Raw::Bytes(a), Raw::Bytes(b)) => a == b,
            (Raw::Array(a), Raw::Array(b)) => a == b,
            (Raw::Record(a), Raw::Record(b)) => a == b,
            (Raw::Null, Raw::Null) => true,
            _ => false,
        }
    }
}

impl PartialEq for Expected {
    fn eq(&self, other: &Expected) -> bool {
        match (self, other) {
            (Expected::F64(a), Expected::F64(b)) => same_float(*a, *b),
            (Expected::I64(a), Expected::I64(b)) => a == b,
            (Expected::U64(a), Expected::U64(b)) => a == b,
            (Expected::Bool(a), Expected::Bool(b)) => a == b,
            (Expected::Str(a), Expected::Str(b)) => a == b,
            (Expected::Bytes(a), Expected::Bytes(b)) => a == b,
            (Expected::Enum(a), Expected::Enum(b)) => a == b,
            (
                Expected::Vector { frame, members },
                Expected::Vector {
                    frame: other_frame,
                    members: other_members,
                },
            ) => {
                frame == other_frame
                    && members.len() == other_members.len()
                    && members
                        .iter()
                        .zip(other_members)
                        .all(|(a, b)| same_float(*a, *b))
            }
            _ => false,
        }
    }
}

/// Any telemetry layout validation accepts, of the kinds this module
/// builds, with one to three messages for each packet.
pub fn telemetry() -> BoxedStrategy<Telemetry> {
    case(message)
}

/// Layouts as [`telemetry`] makes them, with messages damaged as
/// `layout/body/faults.rs` plants faults in them: each fails with its
/// [`Damage::failure`], or decodes when nothing could be planted in it.
pub fn damaged() -> BoxedStrategy<Telemetry> {
    case(body::faults::damaged_message)
}

/// A family and its set, with one to three messages from `message` for
/// each packet.
fn case(
    message: fn(PacketLayout, TimeSource) -> BoxedStrategy<Message>,
) -> BoxedStrategy<Telemetry> {
    (any::<bool>(), time_source())
        .prop_flat_map(|(little, time)| {
            let packet = (body_spec(), time_fields(&time));
            (Just(little), Just(time), vec(packet, 1..=3))
        })
        .prop_flat_map(move |(little, time, packets)| {
            let packets: Vec<PacketLayout> = packets
                .into_iter()
                .enumerate()
                .map(|(index, (body, count))| {
                    lay_out(PacketShape::single(index), little, &time, count, None, body)
                })
                .collect();
            let messages: Vec<BoxedStrategy<Vec<Message>>> = packets
                .iter()
                .map(|packet| vec(message(packet.clone(), time.clone()), 1..=3).boxed())
                .collect();
            (Just(little), Just(time), Just(packets), messages)
        })
        .prop_map(|(little, time, packets, messages)| assemble(little, &time, &packets, messages))
        .boxed()
}

// ---- What a field is -----------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Text {
    Utf8,
    Ascii,
    Latin1,
}

impl Text {
    /// Its name, as the IR spells it.
    fn name(self) -> &'static str {
        match self {
            Text::Utf8 => "utf8",
            Text::Ascii => "ascii",
            Text::Latin1 => "latin1",
        }
    }
}

/// The most bytes a string or bytes value of a length that varies has in a
/// message.
const MOST_BYTES: u32 = 8;

/// How many bytes a string or bytes value takes (§5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Length {
    /// `bytes` bytes. With a terminator the value ends at the first one
    /// among them, and the rest is padding (a C `char[16]`).
    Fixed { bytes: u32, terminator: Option<u8> },
    /// Up to a terminator byte, which ends the value but isn't part of it.
    Terminated(u8),
    /// `raw * multiply + add` bytes, `raw` read from a field of `bits` right
    /// before it (§5.4).
    Field { bits: u32, multiply: u32, add: i64 },
    /// The rest of the packet.
    Remaining,
}

impl Length {
    /// The byte that ends its value, if one does.
    fn terminator(self) -> Option<u8> {
        match self {
            Length::Fixed { terminator, .. } => terminator,
            Length::Terminated(byte) => Some(byte),
            Length::Field { .. } | Length::Remaining => None,
        }
    }

    /// How many bytes its value can have in a message: exactly a fixed
    /// length's without a terminator, up to it with one, those a length
    /// field can say (up to [`MOST_BYTES`]), or up to [`MOST_BYTES`]. Some
    /// count is always above 0: a length field of 2 bits or more holds 2
    /// and 3, which say `2m + a` and `3m + a` bytes, and for the scales
    /// [`varying_length`] makes one of those is from 1 to 8.
    fn value_bytes(self) -> Vec<usize> {
        match self {
            Length::Fixed {
                bytes,
                terminator: None,
            } => vec![bytes as usize],
            Length::Fixed { bytes, .. } => (0..=bytes as usize).collect(),
            Length::Field { .. } => (0..=MOST_BYTES as usize)
                .filter(|&bytes| self.field_raw(bytes).is_some())
                .collect(),
            Length::Terminated(_) | Length::Remaining => (0..=MOST_BYTES as usize).collect(),
        }
    }

    /// The raw value a length field holds for a value of `bytes` bytes:
    /// `(bytes - add) / multiply`, if that's a whole number its field holds.
    fn field_raw(self, bytes: usize) -> Option<u64> {
        let Length::Field {
            bits,
            multiply,
            add,
        } = self
        else {
            return None;
        };
        let rest = i128::try_from(bytes).ok()? - i128::from(add);
        let multiply = i128::from(multiply);
        if rest < 0 || rest % multiply != 0 {
            return None;
        }
        let raw = u64::try_from(rest / multiply).ok()?;
        raw.checked_shr(bits)
            .is_none_or(|high| high == 0)
            .then_some(raw)
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Uint(u32),
    Int(u32),
    Float(u32),
    Bool,
    Text { length: Length, text: Text },
    Bytes(Length),
}

impl Kind {
    /// Its size in bits; for a string or bytes whose length varies, the
    /// fewest it takes (its terminator alone, or nothing), since what
    /// varies is whole bytes.
    fn bits(&self) -> u32 {
        match self {
            Kind::Uint(bits) | Kind::Int(bits) | Kind::Float(bits) => *bits,
            Kind::Bool => 1,
            Kind::Text { length, .. } | Kind::Bytes(length) => match length {
                Length::Fixed { bytes, .. } => bytes * 8,
                Length::Terminated(_) => 8,
                Length::Field { .. } | Length::Remaining => 0,
            },
        }
    }

    /// The most bits a value of it takes in a message.
    fn most_bits(&self) -> u32 {
        match self {
            Kind::Text { length, .. } | Kind::Bytes(length) => match length {
                Length::Fixed { bytes, .. } => bytes * 8,
                Length::Terminated(_) => (MOST_BYTES + 1) * 8,
                Length::Field { .. } | Length::Remaining => MOST_BYTES * 8,
            },
            _ => self.bits(),
        }
    }

    /// A string's or bytes' length.
    fn length(&self) -> Option<Length> {
        match self {
            Kind::Text { length, .. } | Kind::Bytes(length) => Some(*length),
            _ => None,
        }
    }

    /// Whether it has a byte order (§2.1): only integers and floats do.
    fn ordered(&self) -> bool {
        matches!(self, Kind::Uint(_) | Kind::Int(_) | Kind::Float(_))
    }

    /// Whether it starts on a byte whatever its order (§2.2).
    fn on_a_byte(&self) -> bool {
        self.length().is_some()
    }

    /// The same kind of integer, `bits` wide.
    fn widened(&self, bits: u32) -> Kind {
        match self {
            Kind::Uint(_) => Kind::Uint(bits),
            Kind::Int(_) => Kind::Int(bits),
            other => other.clone(),
        }
    }

    /// The integers an integer field holds.
    fn integers(&self) -> Option<RangeInclusive<i128>> {
        match self {
            Kind::Uint(bits) => Some(0..=(1_i128 << bits) - 1),
            Kind::Int(bits) => {
                let half = 1_i128 << (bits - 1);
                Some(-half..=half - 1)
            }
            _ => None,
        }
    }

    /// Text of ASCII letters fits it: an ASCII or Latin-1 string without a
    /// terminator, or with a NUL one. State names and text sentinels are
    /// such text.
    fn takes_letters(&self) -> bool {
        matches!(
            self,
            Kind::Text { text, length }
                if *text != Text::Utf8 && matches!(length.terminator(), None | Some(0))
        )
    }
}

/// Where a field or element sits, which decides the kinds it can have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// An entry of a packet or struct type: any kind, its length fixed, up
    /// to a terminator or from a field.
    Entry,
    /// An array's element. It has no fields of its own to take a length
    /// from, and where the count varies it's whole bytes, so not a bool.
    Element { varying: bool },
}

/// The param a field maps to, and through what.
#[derive(Debug, Clone, PartialEq)]
enum Meaning {
    U64,
    I64,
    Bool,
    Str,
    Bytes,
    /// A string field's text, one of these states.
    TextState(Vec<String>),
    /// An enum param, through these (raw, name) states.
    States(Vec<(i128, String)>),
    /// An f64 param, through a conversion, from the field's unit into the
    /// param's.
    Number {
        conversion: Numeric,
        units: Option<(&'static str, &'static str)>,
    },
}

/// A numeric conversion (§7.1).
#[derive(Debug, Clone, PartialEq)]
enum Numeric {
    Plain,
    Linear {
        scale: f64,
        offset: f64,
    },
    Poly(Vec<f64>),
    Table {
        points: Vec<(f64, f64)>,
        step: bool,
        extrapolate: bool,
    },
    Spline {
        points: Vec<(f64, f64)>,
        extrapolate: bool,
    },
}

/// A sentinel (§7.2): an integer, or text.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Sentinel {
    Int(i128),
    Text(String),
}

/// A field or scalar array element, before it's laid out.
#[derive(Debug, Clone)]
struct FieldSpec {
    kind: Kind,
    meaning: Meaning,
    invalid: Vec<Sentinel>,
    /// Whether it asks for the other byte order than its family's.
    flip: bool,
    /// Whether it maps to a param: a field to its own, an element to its
    /// array's.
    mapped: bool,
}

fn field_spec() -> BoxedStrategy<FieldSpec> {
    spec_of(kind(Slot::Entry))
}

/// Any kind a field or element can have where it sits.
fn kind(slot: Slot) -> BoxedStrategy<Kind> {
    let texts = select(&[Text::Utf8, Text::Ascii, Text::Latin1][..]);
    let text = (texts, text_length(slot))
        .prop_map(|(text, length)| Kind::Text { length, text })
        .boxed();
    let bytes = prop_oneof![
        3 => (1_u32..=8).prop_map(|bytes| Length::Fixed { bytes, terminator: None }),
        1 => varying_length(slot),
    ]
    .prop_map(Kind::Bytes)
    .boxed();
    let mut kinds = vec![
        (3, (1_u32..=64).prop_map(Kind::Uint).boxed()),
        (3, (1_u32..=64).prop_map(Kind::Int).boxed()),
        (2, select(&[32_u32, 64][..]).prop_map(Kind::Float).boxed()),
        (2, text),
        (1, bytes),
    ];
    if slot != (Slot::Element { varying: true }) {
        kinds.push((1, Just(Kind::Bool).boxed()));
    }
    proptest::strategy::Union::new_weighted(kinds).boxed()
}

/// A string's length: fixed (1 to 12 bytes, maybe ending at a terminator)
/// most of the time, else one that varies.
fn text_length(slot: Slot) -> BoxedStrategy<Length> {
    let fixed = (1_u32..=12, proptest::option::of(any::<u8>()))
        .prop_map(|(bytes, terminator)| Length::Fixed { bytes, terminator });
    prop_oneof![3 => fixed, 1 => varying_length(slot)].boxed()
}

/// A length that varies: up to a terminator, or, for an entry, from a
/// field of 2 to 16 bits, scaled now and then (§5.4), with a negative
/// `add` a quarter of the time, so a raw 0 says a length below 0.
fn varying_length(slot: Slot) -> BoxedStrategy<Length> {
    let terminated = any::<u8>().prop_map(Length::Terminated);
    if slot != Slot::Entry {
        return terminated.boxed();
    }
    let field = (
        2_u32..=16,
        prop_oneof![3 => Just(1_u32), 1 => 2_u32..=3],
        prop_oneof![2 => Just(0_i64), 1 => -2_i64..=-1, 1 => 1_i64..=2],
    )
        .prop_map(|(bits, multiply, add)| Length::Field {
            bits,
            multiply,
            add,
        });
    prop_oneof![terminated, field].boxed()
}

/// A string or bytes that runs to the packet's end.
fn remaining_kind() -> BoxedStrategy<Kind> {
    prop_oneof![
        select(&[Text::Utf8, Text::Ascii, Text::Latin1][..]).prop_map(|text| Kind::Text {
            length: Length::Remaining,
            text,
        }),
        Just(Kind::Bytes(Length::Remaining)),
    ]
    .boxed()
}

/// A field of one of `kinds`, with any meaning and sentinels it can have,
/// mapped most of the time.
fn spec_of(kinds: BoxedStrategy<Kind>) -> BoxedStrategy<FieldSpec> {
    (kinds, any::<bool>(), proptest::bool::weighted(0.85))
        .prop_flat_map(|(kind, flip, mapped)| {
            (meaning(&kind), sentinels(&kind), Just(kind)).prop_map(
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

/// A meaning the field can have (validation's mapping rules).
fn meaning(kind: &Kind) -> BoxedStrategy<Meaning> {
    match kind {
        Kind::Uint(bits) => {
            let mut choices = vec![
                Just(Meaning::U64).boxed(),
                Just(Meaning::Bool).boxed(),
                number(kind),
                number(kind),
                states(kind),
            ];
            if *bits <= 63 {
                choices.push(Just(Meaning::I64).boxed());
            }
            proptest::strategy::Union::new(choices).boxed()
        }
        Kind::Int(_) => prop_oneof![
            1 => Just(Meaning::I64),
            1 => Just(Meaning::Bool),
            2 => number(kind),
            1 => states(kind),
        ]
        .boxed(),
        Kind::Float(_) => number(kind),
        Kind::Bool => Just(Meaning::Bool).boxed(),
        Kind::Text { .. } if kind.takes_letters() => prop_oneof![
            Just(Meaning::Str),
            btree_set(letters(kind), 1..=3)
                .prop_map(|names| Meaning::TextState(names.into_iter().collect())),
        ]
        .boxed(),
        Kind::Text { .. } => Just(Meaning::Str).boxed(),
        Kind::Bytes(_) => Just(Meaning::Bytes).boxed(),
    }
}

/// ASCII letters that fit a field that [`Kind::takes_letters`]: at least
/// one, and as many as its value can have.
fn letters(kind: &Kind) -> BoxedStrategy<String> {
    let mut counts = kind.length().map(Length::value_bytes).unwrap_or_default();
    counts.retain(|&count| count > 0);
    select(counts)
        .prop_flat_map(|count| vec(select(&b"abcdefghij"[..]), count))
        .prop_map(|letters| letters.into_iter().map(char::from).collect())
        .boxed()
}

/// An f64 meaning: through any numeric conversion, in any units.
fn number(kind: &Kind) -> BoxedStrategy<Meaning> {
    number_in(kind, proptest::option::of(units()).boxed())
}

/// An f64 meaning: through any numeric conversion, in `units` (the
/// field's and its param's).
fn number_in(
    kind: &Kind,
    units: BoxedStrategy<Option<(&'static str, &'static str)>>,
) -> BoxedStrategy<Meaning> {
    let linear = (scale(), -1e3..1e3_f64)
        .prop_map(|(scale, offset)| Numeric::Linear { scale, offset })
        .boxed();
    let poly = vec(-10.0..10.0_f64, 1..=4).prop_map(Numeric::Poly).boxed();
    let table = (points(kind), any::<bool>(), any::<bool>())
        .prop_map(|(points, step, extrapolate)| Numeric::Table {
            points,
            step,
            extrapolate,
        })
        .boxed();
    let spline = (points(kind), any::<bool>())
        .prop_map(|(points, extrapolate)| Numeric::Spline {
            points,
            extrapolate,
        })
        .boxed();
    let conversion = prop_oneof![
        2 => Just(Numeric::Plain),
        2 => linear,
        1 => poly,
        2 => table,
        1 => spline,
    ];
    (conversion, units)
        .prop_map(|(conversion, units)| Meaning::Number { conversion, units })
        .boxed()
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

/// A field's unit and its param's, of one dimension (maybe the same code).
fn units() -> BoxedStrategy<(&'static str, &'static str)> {
    select(UNIT_GROUPS)
        .prop_flat_map(|group| (select(group), select(group)))
        .boxed()
}

/// A linear scale: nonzero, from a thousandth to a thousand either way.
fn scale() -> BoxedStrategy<f64> {
    (1e-3..1e3_f64, any::<bool>())
        .prop_map(|(scale, negative)| if negative { -scale } else { scale })
        .boxed()
}

/// Two to five calibration points in strictly increasing raw order. An
/// integer field's are integers it holds (within ±1000), so its values can
/// land on them; a float field's are quarters.
fn points(kind: &Kind) -> BoxedStrategy<Vec<(f64, f64)>> {
    let raws: BoxedStrategy<Vec<f64>> = match kind.integers() {
        Some(range) => {
            let low = (*range.start()).max(-1000);
            let high = (*range.end()).min(1000);
            let most = usize::try_from(high - low + 1).unwrap_or(5).min(5);
            btree_set(low..=high, 2..=most)
                .prop_map(|raws| raws.into_iter().map(|raw| raw as f64).collect())
                .boxed()
        }
        None => btree_set(-4000_i32..=4000, 2..=5)
            .prop_map(|raws| raws.into_iter().map(|raw| f64::from(raw) / 4.0).collect())
            .boxed(),
    };
    raws.prop_flat_map(|raws| {
        let count = raws.len();
        (Just(raws), vec(-1e6..1e6_f64, count))
    })
    .prop_map(|(raws, values)| raws.into_iter().zip(values).collect())
    .boxed()
}

/// One to four states with distinct raw values the field holds.
fn states(kind: &Kind) -> BoxedStrategy<Meaning> {
    let range = kind.integers().unwrap_or(0..=0);
    btree_set(range, 1..=4)
        .prop_map(|raws| {
            Meaning::States(
                raws.into_iter()
                    .enumerate()
                    .map(|(index, raw)| (raw, format!("S{index}")))
                    .collect(),
            )
        })
        .boxed()
}

/// Up to two sentinels the field can hold: integers in an integer field's
/// range, small integers for a float, letters for text that takes them.
fn sentinels(kind: &Kind) -> BoxedStrategy<Vec<Sentinel>> {
    let set = |values: BoxedStrategy<Sentinel>| {
        btree_set(values, 0..=2)
            .prop_map(|set: BTreeSet<Sentinel>| set.into_iter().collect())
            .boxed()
    };
    match kind {
        Kind::Uint(_) | Kind::Int(_) => set(kind
            .integers()
            .unwrap_or(0..=0)
            .prop_map(Sentinel::Int)
            .boxed()),
        Kind::Float(_) => set((-1000_i128..=1000).prop_map(Sentinel::Int).boxed()),
        Kind::Text { .. } if kind.takes_letters() => {
            set(letters(kind).prop_map(Sentinel::Text).boxed())
        }
        _ => Just(Vec::new()).boxed(),
    }
}

// ---- Laying a packet out ------------------------------------------------

/// A field or scalar array element, laid out: how it's read, and what it
/// means.
#[derive(Debug, Clone)]
struct Placed {
    name: String,
    /// The param it maps to; an element maps to its array's.
    param: Option<String>,
    kind: Kind,
    meaning: Meaning,
    invalid: Vec<Sentinel>,
    /// Whether it's read little-endian.
    little: bool,
    /// Whether it spells its byte order: it isn't the family's, and it
    /// matters where the value sits.
    explicit: bool,
}

#[derive(Debug, Clone)]
struct PacketLayout {
    name: String,
    /// The value of its first byte, which identifies it.
    id: u8,
    /// The keys that identify it, as IR JSON.
    identify: Value,
    /// Uint fields of whole bytes right after the id (a native id, a
    /// version, a second identify key), by name and width, each message
    /// giving their values.
    header: Vec<(&'static str, u32)>,
    /// Whether its family is little-endian.
    little: bool,
    /// Its count field (an elapsed count or a time of week), after a GPS
    /// week field, right after the header.
    count: Option<Count>,
    /// The count's and the week's sentinels, which mean ground time.
    count_invalid: Vec<i128>,
    week_invalid: Vec<u64>,
    /// For a boot set's correlation packet: its clock and boot fields,
    /// right after the header.
    correlation: Option<Correlation>,
    /// Its entries after the header and time fields.
    body: Body,
    /// Its entries, padding included, as IR JSON.
    entries: Vec<Value>,
}

impl PacketLayout {
    /// The packet as IR JSON.
    fn json(&self) -> Value {
        let mut json = json!({ "name": self.name, "identify": self.identify,
                               "entries": self.entries });
        if let Some(length) = self.body.length {
            let kind = if length.exact { "exact" } else { "at_least" };
            json["length"] = json!({ "kind": kind, "bytes": length.bytes });
        }
        if self.count.is_some() {
            json["time_field"] = json!("t");
        }
        let composites = self.body.composites_json();
        if !composites.is_empty() {
            json["composites"] = json!(composites);
        }
        json
    }
}

/// What a packet is called and how it's told apart.
#[derive(Debug, Clone)]
struct PacketShape {
    name: String,
    /// What its params' names start with.
    params: String,
    id: u8,
    identify: Value,
    header: Vec<(&'static str, u32)>,
}

impl PacketShape {
    /// Packet `index` of a set of packets told apart by their id alone.
    fn single(index: usize) -> PacketShape {
        let id = u8::try_from(index + 1).unwrap_or(u8::MAX);
        PacketShape {
            name: format!("P{index}"),
            params: format!("p{index}"),
            id,
            identify: json!({ "id": id }),
            header: Vec::new(),
        }
    }
}

/// A boot set's correlation fields (§8.2): the absolute clock, in whole
/// bytes, and the boot clock (32 bits), with any sentinels.
#[derive(Debug, Clone, PartialEq)]
struct Correlation {
    clock_bits: u32,
    clock_invalid: Vec<u64>,
    boot_invalid: Vec<u64>,
    /// Whether the clocks restate the units they count in.
    units: bool,
}

/// The bit at which a packet's time fields start: after the id and its
/// header.
fn time_start(header: &[(&str, u32)]) -> u64 {
    8 + header.iter().map(|(_, bits)| u64::from(*bits)).sum::<u64>()
}

/// Whether `size` bits at `offset` lie inside one byte.
fn inside_one_byte(offset: u64, size: u32) -> bool {
    offset / 8 == (offset + u64::from(size) - 1) / 8
}

fn lay_out(
    shape: PacketShape,
    family_little: bool,
    time: &TimeSource,
    timed: TimeFields,
    correlation: Option<Correlation>,
    spec: BodySpec,
) -> PacketLayout {
    let TimeFields {
        count,
        count_invalid,
        week_invalid,
        units,
    } = timed;
    let source_unit = match time {
        TimeSource::Elapsed { unit, .. }
        | TimeSource::Gps { unit, .. }
        | TimeSource::Boot { unit, .. } => Some(unit.name()),
        _ => None,
    };
    let mut entries =
        vec![json!({ "kind": "field", "name": "id", "type": "uint", "size_bits": 8 })];
    for (name, bits) in &shape.header {
        entries.push(json!({ "kind": "field", "name": name, "type": "uint", "size_bits": bits }));
    }
    let mut offset = time_start(&shape.header);
    if let Some(correlation) = &correlation {
        let mut clock = json!({ "kind": "field", "name": "clock", "type": "uint",
                                "size_bits": correlation.clock_bits });
        if !correlation.clock_invalid.is_empty() {
            clock["invalid"] = json!(correlation.clock_invalid);
        }
        let mut boot = json!({ "kind": "field", "name": "boot", "type": "uint", "size_bits": 32 });
        if !correlation.boot_invalid.is_empty() {
            boot["invalid"] = json!(correlation.boot_invalid);
        }
        if let (
            true,
            TimeSource::Boot {
                unit, clock_unit, ..
            },
        ) = (correlation.units, time)
        {
            clock["unit"] = json!(clock_unit.name());
            boot["unit"] = json!(unit.name());
        }
        entries.push(clock);
        entries.push(boot);
        offset += u64::from(correlation.clock_bits) + 32;
    }
    // The CUC time code sits right after the header, in every packet.
    if let TimeSource::Cuc { coarse, fine, .. } = time {
        let bits = u64::from(8 * (coarse + fine));
        entries.push(json!({ "kind": "padding", "size_bits": bits }));
        offset += bits;
    }
    if let Some(count) = count {
        if matches!(time, TimeSource::Gps { .. }) {
            let mut week =
                json!({ "kind": "field", "name": "week", "type": "uint", "size_bits": 16 });
            if !week_invalid.is_empty() {
                week["invalid"] = json!(week_invalid);
            }
            if units {
                week["unit"] = json!("wk");
            }
            entries.push(week);
            offset += 16;
        }
        let ty = match count {
            Count::Uint(_) => "uint",
            Count::Int(_) => "int",
            Count::Float(_) => "float",
        };
        let mut t = json!({ "kind": "field", "name": "t", "type": ty, "size_bits": count.bits() });
        if !count_invalid.is_empty() {
            let invalid: Vec<Value> = count_invalid.iter().map(|n| integer_json(*n)).collect();
            t["invalid"] = json!(invalid);
        }
        if let (true, Some(unit)) = (units, source_unit) {
            t["unit"] = json!(unit);
        }
        entries.push(t);
        offset += u64::from(count.bits());
    }
    let body = body::lay_out(spec, family_little, offset, &shape.name, &shape.params);
    entries.extend(body.entries_json());
    PacketLayout {
        name: shape.name,
        id: shape.id,
        identify: shape.identify,
        header: shape.header,
        little: family_little,
        count,
        count_invalid,
        week_invalid,
        correlation,
        body,
        entries,
    }
}

fn field_json(field: &Placed) -> Value {
    let mut json = scalar_json(field);
    json.insert("kind".into(), json!("field"));
    json.insert("name".into(), json!(field.name));
    if let Some(param) = &field.param {
        json.insert("canonical".into(), json!(param));
    }
    Value::Object(json)
}

/// What a field and a scalar array element both spell: type, size or
/// length, byte order, text, conversion, unit and sentinels.
fn scalar_json(field: &Placed) -> Map<String, Value> {
    let mut json = Map::new();
    json.insert("type".into(), json!(type_name(&field.kind)));
    match &field.kind {
        Kind::Uint(bits) | Kind::Int(bits) | Kind::Float(bits) => {
            json.insert("size_bits".into(), json!(bits));
        }
        Kind::Bool => {
            json.insert("size_bits".into(), json!(1));
        }
        Kind::Text { length, text } => {
            json.insert("length".into(), length_json(*length, &field.name));
            // UTF-8 is the default, which the IR doesn't spell.
            if *text != Text::Utf8 {
                json.insert("text".into(), json!(text.name()));
            }
        }
        Kind::Bytes(length) => {
            json.insert("length".into(), length_json(*length, &field.name));
        }
    }
    if field.explicit {
        json.insert(
            "byte_order".into(),
            json!(if field.little { "little" } else { "big" }),
        );
    }
    meaning_json(field, &mut json);
    json
}

/// A field type's name, as the IR spells it.
fn type_name(kind: &Kind) -> &'static str {
    match kind {
        Kind::Uint(_) => "uint",
        Kind::Int(_) => "int",
        Kind::Float(_) => "float",
        Kind::Bool => "bool",
        Kind::Text { .. } => "string",
        Kind::Bytes(_) => "bytes",
    }
}

/// What a field means, as IR JSON keys: its conversion, unit and
/// sentinels, whatever encoding it's read from.
fn meaning_json(field: &Placed, json: &mut Map<String, Value>) {
    match &field.meaning {
        Meaning::States(states) => {
            let states: Vec<Value> = states
                .iter()
                .map(|(raw, name)| json!({ "raw": integer_json(*raw), "name": name }))
                .collect();
            json.insert(
                "conversion".into(),
                json!({ "kind": "states", "states": states }),
            );
        }
        Meaning::Number { conversion, units } => {
            if let Some(conversion) = conversion_json(conversion) {
                json.insert("conversion".into(), conversion);
            }
            if let Some((unit, _)) = units {
                json.insert("unit".into(), json!(unit));
            }
        }
        _ => {}
    }
    if !field.invalid.is_empty() {
        let invalid: Vec<Value> = field
            .invalid
            .iter()
            .map(|sentinel| match sentinel {
                Sentinel::Int(n) => integer_json(*n),
                Sentinel::Text(text) => json!(text),
            })
            .collect();
        json.insert("invalid".into(), json!(invalid));
    }
}

/// A length as IR JSON. A length field is named after the entry it serves
/// ([`length_field`]).
fn length_json(length: Length, name: &str) -> Value {
    match length {
        Length::Fixed { bytes, terminator } => {
            let mut json = json!({ "kind": "fixed", "bytes": bytes });
            if let Some(byte) = terminator {
                json["terminator"] = json!(byte);
            }
            json
        }
        Length::Terminated(byte) => json!({ "kind": "terminator", "byte": byte }),
        Length::Field { multiply, add, .. } => {
            let mut json = json!({ "kind": "field", "field": length_field(name) });
            if multiply != 1 {
                json["multiply"] = json!(multiply);
            }
            if add != 0 {
                json["add"] = json!(add);
            }
            json
        }
        Length::Remaining => json!({ "kind": "remaining" }),
    }
}

/// The name of the field that holds an entry's length.
fn length_field(name: &str) -> String {
    format!("{name}_len")
}

/// A numeric conversion as IR JSON; `None` for none. Defaults (linear
/// interpolation, no extrapolation) aren't spelled.
fn conversion_json(conversion: &Numeric) -> Option<Value> {
    let points = |points: &[(f64, f64)]| -> Value {
        points
            .iter()
            .map(|(raw, value)| json!({ "raw": raw, "value": value }))
            .collect()
    };
    let mut json = match conversion {
        Numeric::Plain => return None,
        Numeric::Linear { scale, offset } => {
            json!({ "kind": "linear", "scale": scale, "offset": offset })
        }
        Numeric::Poly(coefficients) => json!({ "kind": "poly", "coefficients": coefficients }),
        Numeric::Table {
            points: p, step, ..
        } => {
            let mut table = json!({ "kind": "table", "points": points(p) });
            if *step {
                table["interpolation"] = json!("step");
            }
            table
        }
        Numeric::Spline { points: p, .. } => json!({ "kind": "spline", "points": points(p) }),
    };
    if let Numeric::Table {
        extrapolate: true, ..
    }
    | Numeric::Spline {
        extrapolate: true, ..
    } = conversion
    {
        json["extrapolate"] = json!(true);
    }
    Some(json)
}

fn param_json(meaning: &Meaning) -> Value {
    match meaning {
        Meaning::U64 => json!({ "type": "u64" }),
        Meaning::I64 => json!({ "type": "i64" }),
        Meaning::Bool => json!({ "type": "bool" }),
        Meaning::Str => json!({ "type": "str" }),
        Meaning::Bytes => json!({ "type": "bytes" }),
        Meaning::TextState(names) => json!({ "type": "enum", "states": names }),
        Meaning::States(states) => {
            let names: Vec<&str> = states.iter().map(|(_, name)| name.as_str()).collect();
            json!({ "type": "enum", "states": names })
        }
        Meaning::Number { units, .. } => match units {
            Some((_, unit)) => json!({ "type": "f64", "unit": unit }),
            None => json!({ "type": "f64" }),
        },
    }
}

/// How long after the packet's event time element `position` of a block
/// was taken, in whole nanoseconds: `round(i × interval_ms × 1000000)` in
/// binary64, left to right, ties away from zero (§7.3).
fn offset_ns(position: u64, interval_ms: f64) -> i64 {
    (position as f64 * interval_ms * 1_000_000.0).round() as i64
}

/// An integer as the IR writes it: a number within ±(2^53 - 1), else a
/// decimal string.
pub(crate) fn integer_json(n: i128) -> Value {
    const SAFE: i128 = (1 << 53) - 1;
    match i64::try_from(n) {
        Ok(small) if n.abs() <= SAFE => json!(small),
        _ => json!(n.to_string()),
    }
}

// ---- Values and packing ---------------------------------------------------

/// A message for `packet`: a raw value for each field, packed over random
/// bytes, so padding and trailing bits are noise the decoder must skip.
fn message(packet: PacketLayout, time: TimeSource) -> BoxedStrategy<Message> {
    received()
        .prop_flat_map(move |ground| message_at(packet.clone(), time.clone(), ground, Vec::new()))
        .boxed()
}

/// A message of `packet` received at `ground`, its header fields holding
/// `header`.
fn message_at(
    packet: PacketLayout,
    time: TimeSource,
    ground: i128,
    header: Vec<u64>,
) -> BoxedStrategy<Message> {
    draft(&packet, &time, ground, header)
        .prop_map(move |draft| pack(&packet, &time, draft))
        .boxed()
}

/// What a message holds before it's packed: its body's values, the noise
/// they're written over, its header and time fields, and how many bytes
/// past an exact length it has.
#[derive(Debug, Clone)]
struct Draft {
    values: Vec<(String, Raw)>,
    noise: Vec<u8>,
    timing: Timing,
    extra: usize,
}

/// The most bytes a message has past its packet's exact length.
const MOST_EXTRA: usize = 4;

/// A draft of a message of `packet` received at `ground`, its header
/// fields holding `header`: a raw value for each field, over random bytes,
/// so padding and trailing bits are noise the decoder must skip.
fn draft(
    packet: &PacketLayout,
    time: &TimeSource,
    ground: i128,
    header: Vec<u64>,
) -> BoxedStrategy<Draft> {
    let time = time.clone();
    let values = packet.body.values();
    // Room for the most a message reaches, its packet's length, and bytes
    // past an exact one, which decoding ignores (§5.1): now and then.
    let floor = packet.body.length.map_or(0, |length| length.bytes as usize);
    let most = usize::try_from(packet.body.most_bits.div_ceil(8)).unwrap_or(0);
    let length = most.max(floor) + MOST_EXTRA;
    let extra = match packet.body.length {
        Some(PacketLength { exact: true, .. }) => prop_oneof![Just(0), 1..=MOST_EXTRA].boxed(),
        _ => Just(0).boxed(),
    };
    // Now and then a sentinel, which means ground time.
    let count = match packet.count {
        Some(count) if !packet.count_invalid.is_empty() => {
            let sentinels: Vec<Raw> = packet
                .count_invalid
                .iter()
                .map(|n| match count {
                    Count::Uint(_) => Raw::Uint(u64::try_from(*n).unwrap_or(0)),
                    Count::Int(_) => Raw::Int(i64::try_from(*n).unwrap_or(0)),
                    Count::Float(_) => Raw::Float(*n as f64),
                })
                .collect();
            prop_oneof![4 => count_value(&time, count), 1 => select(sentinels)]
                .prop_map(Some)
                .boxed()
        }
        Some(count) => count_value(&time, count).prop_map(Some).boxed(),
        None => Just(None).boxed(),
    };
    let week = if packet.week_invalid.is_empty() {
        week_value(&time, ground)
    } else {
        let sentinels: Vec<u16> = packet
            .week_invalid
            .iter()
            .map(|n| u16::try_from(*n).unwrap_or(0))
            .collect();
        prop_oneof![4 => week_value(&time, ground), 1 => select(sentinels)].boxed()
    };
    // A CUC code lands near a leap second often: its seconds from the
    // epoch to one, written over the noise where the code holds them.
    let cuc = match &time {
        TimeSource::Cuc {
            coarse,
            epoch,
            scale,
            ..
        } => {
            let (start, bytes) = (epoch.label, *coarse);
            prop_oneof![
                2 => Just(None),
                1 => near_leap(*scale).prop_map(move |label| {
                    let seconds = (label - start).div_euclid(SECOND);
                    (0..1_i128 << (8 * bytes)).contains(&seconds).then_some(seconds)
                }),
            ]
            .boxed()
        }
        _ => Just(None).boxed(),
    };
    let correlation = match &packet.correlation {
        Some(correlation) => (
            clock_value(correlation.clock_bits, &correlation.clock_invalid),
            clock_value(32, &correlation.boot_invalid),
        )
            .prop_map(Some)
            .boxed(),
        None => Just(None).boxed(),
    };
    let timing = (count, week, cuc, correlation);
    let start = time_start(&packet.header) as usize / 8;
    (values, vec(any::<u8>(), length), timing, extra)
        .prop_map(
            move |(values, mut noise, (count, week, cuc, correlation), extra)| {
                if let (Some(seconds), TimeSource::Cuc { coarse, .. }) = (cuc, &time) {
                    for k in 0..*coarse as usize {
                        let shift = 8 * (*coarse as usize - 1 - k);
                        noise[start + k] = (seconds >> shift) as u8;
                    }
                }
                let timing = Timing {
                    count,
                    week,
                    received: ground,
                    header: header.clone(),
                    correlation,
                };
                Draft {
                    values,
                    noise,
                    timing,
                    extra,
                }
            },
        )
        .boxed()
}

/// A correlation clock's value: any its field holds, one of its sentinels
/// now and then.
fn clock_value(bits: u32, invalid: &[u64]) -> BoxedStrategy<u64> {
    let any_value = any::<u64>().prop_map(move |n| fit(n, bits)).boxed();
    if invalid.is_empty() {
        return any_value;
    }
    prop_oneof![4 => any_value, 1 => select(invalid.to_vec())].boxed()
}

/// What a message holds besides its fields' values.
#[derive(Debug, Clone)]
struct Timing {
    count: Option<Raw>,
    week: u16,
    received: i128,
    header: Vec<u64>,
    /// A correlation packet's clock and boot values.
    correlation: Option<(u64, u64)>,
}

/// A raw value for the field: any it holds, with its states, calibration
/// points and sentinels more often than chance would give them.
fn raw_value(field: &Placed) -> BoxedStrategy<Raw> {
    let any_value = any_raw(&field.kind);
    let mut choices: Vec<(u32, BoxedStrategy<Raw>)> = vec![(3, any_value)];
    match &field.meaning {
        Meaning::States(states) => {
            let raws: Vec<i128> = states.iter().map(|(raw, _)| *raw).collect();
            choices.push((6, integer_raw(&field.kind, raws)));
        }
        Meaning::TextState(names) => {
            choices.push((6, select(names.clone()).prop_map(Raw::Str).boxed()));
        }
        Meaning::Number {
            conversion: Numeric::Table { points, .. } | Numeric::Spline { points, .. },
            ..
        } => {
            choices.push((2, on_points(&field.kind, points)));
            choices.push((2, between_points(&field.kind, points)));
        }
        _ => {}
    }
    if !field.invalid.is_empty() {
        let sentinels = field.invalid.clone();
        let kind = field.kind.clone();
        let sentinel = select(sentinels)
            .prop_map(move |sentinel| match (sentinel, &kind) {
                (Sentinel::Int(n), Kind::Float(_)) => Raw::Float(n as f64),
                (Sentinel::Int(n), Kind::Int(_)) => Raw::Int(i64::try_from(n).unwrap_or(0)),
                (Sentinel::Int(n), _) => Raw::Uint(u64::try_from(n).unwrap_or(0)),
                (Sentinel::Text(text), _) => Raw::Str(text),
            })
            .boxed();
        choices.push((1, sentinel));
    }
    proptest::strategy::Union::new_weighted(choices).boxed()
}

/// Any value of the field's kind.
fn any_raw(kind: &Kind) -> BoxedStrategy<Raw> {
    match kind {
        Kind::Uint(bits) => {
            let bits = *bits;
            any::<u64>()
                .prop_map(move |n| Raw::Uint(fit(n, bits)))
                .boxed()
        }
        Kind::Int(bits) => {
            let bits = *bits;
            any::<u64>()
                .prop_map(move |n| Raw::Int(sign_extend(fit(n, bits), bits)))
                .boxed()
        }
        // Any bits, with NaN, the infinities and both zeros more often
        // than random bits give them.
        Kind::Float(32) => prop_oneof![
            8 => any::<u32>(),
            1 => select(&[0x7fc0_0000_u32, 0xff80_0000, 0x7f80_0000, 0x8000_0000, 0][..]),
        ]
        .prop_map(|bits| Raw::Float(f64::from(f32::from_bits(bits))))
        .boxed(),
        Kind::Float(_) => prop_oneof![
            8 => any::<u64>(),
            1 => select(
                &[
                    0x7ff8_0000_0000_0000_u64,
                    0xfff0_0000_0000_0000,
                    0x7ff0_0000_0000_0000,
                    0x8000_0000_0000_0000,
                    0,
                ][..]
            ),
        ]
        .prop_map(|bits| Raw::Float(f64::from_bits(bits)))
        .boxed(),
        Kind::Bool => any::<bool>().prop_map(Raw::Bool).boxed(),
        Kind::Text { length, text } => text_value(*length, *text),
        Kind::Bytes(length) => bytes_value(*length),
    }
}

/// Bytes that fit `length`: as many as its value can have, without its
/// terminator byte.
fn bytes_value(length: Length) -> BoxedStrategy<Raw> {
    let terminator = length.terminator();
    let byte = any::<u8>().prop_map(move |byte| match terminator {
        Some(t) if byte == t => t.wrapping_add(1),
        _ => byte,
    });
    select(length.value_bytes())
        .prop_flat_map(move |count| vec(byte.clone(), count))
        .prop_map(Raw::Bytes)
        .boxed()
}

/// One of `values`, as an integer field's raw value.
fn integer_raw(kind: &Kind, values: Vec<i128>) -> BoxedStrategy<Raw> {
    let signed = matches!(kind, Kind::Int(_));
    select(values)
        .prop_map(move |n| {
            if signed {
                Raw::Int(i64::try_from(n).unwrap_or(0))
            } else {
                Raw::Uint(u64::try_from(n).unwrap_or(0))
            }
        })
        .boxed()
}

/// A calibration point's raw value, exactly.
fn on_points(kind: &Kind, points: &[(f64, f64)]) -> BoxedStrategy<Raw> {
    let raws: Vec<f64> = points.iter().map(|point| point.0).collect();
    match kind {
        Kind::Float(32) => select(raws)
            .prop_map(|raw| Raw::Float(f64::from(raw as f32)))
            .boxed(),
        Kind::Float(_) => select(raws).prop_map(Raw::Float).boxed(),
        _ => integer_raw(kind, raws.into_iter().map(|raw| raw as i128).collect()),
    }
}

/// A value between the first and last calibration points.
fn between_points(kind: &Kind, points: &[(f64, f64)]) -> BoxedStrategy<Raw> {
    let first = points[0].0;
    let last = points[points.len() - 1].0;
    match kind {
        Kind::Float(32) => (first..=last)
            .prop_map(|x| Raw::Float(f64::from(x as f32)))
            .boxed(),
        Kind::Float(_) => (first..=last).prop_map(Raw::Float).boxed(),
        _ => integer_raw(kind, (first as i128..=last as i128).collect()),
    }
}

/// The low `bits` bits of `n`.
fn fit(n: u64, bits: u32) -> u64 {
    if bits == 64 { n } else { n & ((1 << bits) - 1) }
}

/// `n`, `bits` wide, as two's complement.
fn sign_extend(n: u64, bits: u32) -> i64 {
    let shift = 64 - bits;
    ((n << shift) as i64) >> shift
}

/// Text in `text` that fits `length`, in as many bytes as its value can
/// have: one count of them, picked first, filled exactly with letters
/// where the length allows no other (a fixed one without a terminator, or
/// one from a field); else any count up to the most, without the
/// terminator byte.
fn text_value(length: Length, text: Text) -> BoxedStrategy<Raw> {
    let chars = match text {
        Text::Ascii => proptest::char::range('\u{0}', '\u{7f}').boxed(),
        Text::Latin1 => proptest::char::range('\u{0}', '\u{ff}').boxed(),
        Text::Utf8 => any::<char>().boxed(),
    };
    let terminator = length.terminator();
    let exact = matches!(
        length,
        Length::Fixed {
            terminator: None,
            ..
        } | Length::Field { .. }
    );
    let counts = length.value_bytes();
    let most = counts.iter().copied().max().unwrap_or(0);
    let limit = if exact {
        select(counts).boxed()
    } else {
        Just(most).boxed()
    };
    (
        limit,
        vec(chars, 0..=most),
        vec(select(&b"abcdefghij"[..]), most),
    )
        .prop_map(move |(bytes, chars, filler)| {
            let mut value = String::new();
            let mut used = 0;
            for c in chars {
                let encoded = encode(&c.to_string(), text);
                if used + encoded.len() > bytes || terminator.is_some_and(|t| encoded.contains(&t))
                {
                    continue;
                }
                used += encoded.len();
                value.push(c);
            }
            if exact {
                for letter in filler.into_iter().take(bytes - used) {
                    value.push(char::from(letter));
                }
            }
            Raw::Str(value)
        })
        .boxed()
}

/// Text in its encoding (§2.2).
fn encode(value: &str, text: Text) -> Vec<u8> {
    match text {
        Text::Utf8 => value.as_bytes().to_vec(),
        // Callers only give chars the encoding holds.
        Text::Ascii | Text::Latin1 => value.chars().map(|c| c as u32 as u8).collect(),
    }
}

/// Writes `size` bits of `value` at `offset` (§2.1): big-endian most
/// significant bit first; little-endian whole bytes least significant
/// first, or inside one byte as big-endian.
fn put_bits(payload: &mut [u8], offset: u64, size: u32, value: u64, little: bool) {
    if little && !inside_one_byte(offset, size) {
        let first = (offset / 8) as usize;
        for k in 0..(size / 8) as usize {
            payload[first + k] = (value >> (8 * k)) as u8;
        }
        return;
    }
    for i in 0..size {
        let bit = ((value >> (size - 1 - i)) & 1) as u8;
        let at = offset + u64::from(i);
        let shift = 7 - (at % 8) as u32;
        let byte = &mut payload[(at / 8) as usize];
        *byte = (*byte & !(1 << shift)) | (bit << shift);
    }
}

fn pack(packet: &PacketLayout, time: &TimeSource, draft: Draft) -> Message {
    let Draft {
        values,
        mut noise,
        timing,
        extra,
    } = draft;
    let written = write_message(packet, time, &values, &mut noise, &timing, None);
    // A valid message's values never stop decoding inside it, and reach
    // an `at_least` length. One of an exact length has its bytes, and now
    // and then more.
    let end = written.end.unwrap_or(8 * noise.len() as u64);
    let reached = usize::try_from(end.div_ceil(8)).unwrap_or(noise.len());
    let floor = packet
        .body
        .length
        .map_or(0, |length| length.bytes as usize + extra);
    let mut payload = noise;
    payload.truncate(reached.max(floor));
    let samples = packet.body.samples(&values);
    let record = written.record;
    let Timing {
        count,
        week,
        received,
        ..
    } = timing;
    let start = time_start(&packet.header);
    let sentinel = no_reading(
        time,
        count.as_ref(),
        week,
        &packet.count_invalid,
        &packet.week_invalid,
    );
    let event_time = if sentinel {
        EventTime::Ground
    } else {
        event_time(
            time,
            &payload[start as usize / 8..],
            count.as_ref(),
            u64::from(week),
            received,
        )
    };
    Message {
        packet: packet.name.clone(),
        payload,
        record,
        samples,
        received,
        event_time,
        damage: None,
    }
}

/// A message as [`write_message`] writes it.
struct Written {
    /// Its record: its header and time fields, then its body's values,
    /// each checksum holding the value written for it.
    record: Vec<(String, Raw)>,
    /// Every step decoding takes ([`Step`]).
    steps: Vec<Step>,
    /// Where the body ends: `None` when decoding stops inside it.
    end: Option<u64>,
    /// The bits nothing decoding does depends on but checksums over them:
    /// the writer's, and a CUC time code's, padding to decoding.
    inert: Vec<Range<u64>>,
}

/// Writes a message over the noise in `payload`: its id, header and time
/// fields, then its body, with `garbled` written for a string's value if
/// given. Decoding checks the packet's length first, then the checksums at
/// static positions, before it reads any entry (§5.1, §5.6).
fn write_message(
    packet: &PacketLayout,
    time: &TimeSource,
    values: &[(String, Raw)],
    payload: &mut [u8],
    timing: &Timing,
    garbled: Option<(String, Vec<u8>)>,
) -> Written {
    let mut steps = Vec::new();
    if let Some(length) = packet.body.length {
        steps.push(Step::Packet {
            bytes: length.bytes,
            exact: length.exact,
        });
    }
    steps.extend(
        fixed_checks(&packet.body.entries)
            .into_iter()
            .map(Step::Verify),
    );
    let header_step = |name: &str, needs: u64| Step::Reach {
        path: name.to_string(),
        needs,
        short: Short::PastEnd,
        place: Place::Header,
    };
    put_bits(payload, 0, 8, u64::from(packet.id), false);
    let mut record = vec![("id".to_string(), Raw::Uint(u64::from(packet.id)))];
    steps.push(header_step("id", 8));
    // In the family's byte order, whole bytes on byte boundaries.
    let mut at = 8;
    for ((name, bits), value) in packet.header.iter().zip(&timing.header) {
        put_bits(payload, at, *bits, *value, packet.little);
        record.push((name.to_string(), Raw::Uint(*value)));
        at += u64::from(*bits);
        steps.push(header_step(name, at));
    }
    let start = time_start(&packet.header);
    if let (Some(fields), Some((clock, boot))) = (&packet.correlation, timing.correlation) {
        let boot_at = start + u64::from(fields.clock_bits);
        put_bits(payload, start, fields.clock_bits, clock, packet.little);
        put_bits(payload, boot_at, 32, boot, packet.little);
        record.push(("clock".to_string(), Raw::Uint(clock)));
        record.push(("boot".to_string(), Raw::Uint(boot)));
        steps.push(header_step("clock", boot_at));
        steps.push(header_step("boot", boot_at + 32));
    }
    // The CUC code is padding to decoding, at the packet's top level.
    let mut inert = Vec::new();
    if let TimeSource::Cuc { coarse, fine, .. } = time {
        let end = start + u64::from(8 * (coarse + fine));
        steps.push(Step::Reach {
            path: String::new(),
            needs: end,
            short: Short::Padding,
            place: Place::TimeCode,
        });
        inert.push(start..end);
    }
    // The count field comes after the GPS week, in the family's byte order.
    if let (Some(field), Some(raw)) = (packet.count, &timing.count) {
        let mut at = start;
        if matches!(time, TimeSource::Gps { .. }) {
            put_bits(payload, start, 16, u64::from(timing.week), packet.little);
            record.push(("week".to_string(), Raw::Uint(u64::from(timing.week))));
            at = start + 16;
            steps.push(header_step("week", at));
        }
        let bits = field.bits();
        let value = match raw {
            Raw::Uint(n) => *n,
            Raw::Int(n) => fit(*n as u64, bits),
            Raw::Float(x) if bits == 32 => u64::from((*x as f32).to_bits()),
            Raw::Float(x) => x.to_bits(),
            _ => 0,
        };
        put_bits(payload, at, bits, value, packet.little);
        record.push(("t".to_string(), raw.clone()));
        steps.push(header_step("t", at + u64::from(bits)));
    }
    let mut writer = Writer::new(payload, garbled, steps);
    // The body's entries are the packet's: their offsets count from bit 0.
    let end = writer.container(&packet.body.entries, values, 0, packet.body.start, "");
    writer.unreached_checksums(&packet.body.entries);
    record.extend(writer.record(values));
    let (steps, body_inert) = writer.finish();
    inert.extend(body_inert);
    Written {
        record,
        steps,
        end,
        inert,
    }
}

// ---- The bundle -----------------------------------------------------------

fn assemble(
    little: bool,
    time: &TimeSource,
    packets: &[PacketLayout],
    messages: Vec<Vec<Message>>,
) -> Telemetry {
    let mut params = Map::new();
    let mut types = Map::new();
    for packet in packets {
        params.extend(packet.body.params());
        types.extend(packet.body.types_json());
    }
    let packet_json: Vec<Value> = packets.iter().map(PacketLayout::json).collect();
    let mut set = json!({ "name": "fam_v1", "family": "fam", "packets": packet_json });
    if let Some(time) = time_json(time, 8) {
        set["time"] = time;
    }
    if !types.is_empty() {
        set["types"] = Value::Object(types);
    }
    let bundle = json!({
        "mdl_version": "2.0",
        "catalog": { "params": params },
        "families": [{
            "name": "fam",
            "encoding": "binary",
            "byte_order": if little { "little" } else { "big" },
            "alias": { "kind": "fixed", "value": "A1" }
        }],
        "sets": [set]
    });
    Telemetry {
        bundle,
        family: "fam".into(),
        set: "fam_v1".into(),
        native_id: "A1".into(),
        messages: messages.into_iter().flatten().collect(),
    }
}

mod body;
pub mod command;
mod condition;
mod faults;
mod fleet;
mod json;
mod meaning;
#[cfg(test)]
mod reference;
#[cfg(test)]
mod tests;
mod time;
