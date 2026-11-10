//! Valid layouts and the messages they decode, built from this module's
//! own reading of `spec/decoding.md`, not from `mdl`: the oracle the
//! decoder is checked against. A generated [`Telemetry`] is a binary family
//! with one set of up to three packets, each a run of fields of every
//! scalar type, size, position and byte order validation allows, mapped to
//! catalog params; and messages packed bit by bit (§2.1), with what each
//! decodes to: every raw value, and every sample.
//!
//! Positions and conversions follow the spec's rules as written there:
//! little-endian values are whole bytes on byte boundaries or sit inside
//! one byte, strings and bytes start on a byte, a `byte_order` is spelled
//! only where it differs from the family's and matters, and a linear
//! conversion multiplies, then adds, in binary64 (§7.1).

use std::ops::RangeInclusive;

use proptest::collection::{btree_set, vec};
use proptest::prelude::*;
use proptest::sample::select;
use serde_json::{Map, Value, json};

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
}

/// A sample: its param, value and quality (`spec/decoding.md` §7.3).
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub param: String,
    pub value: Expected,
    /// The quality's name: `good`, or `non_finite`.
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
            _ => false,
        }
    }
}

/// Any telemetry layout validation accepts, of the kinds this module
/// builds, with one to three messages for each packet.
pub fn telemetry() -> BoxedStrategy<Telemetry> {
    (any::<bool>(), vec(vec(field_spec(), 1..=8), 1..=3))
        .prop_flat_map(|(little, packets)| {
            let packets: Vec<PacketLayout> = packets
                .into_iter()
                .enumerate()
                .map(|(index, fields)| lay_out(index, little, fields))
                .collect();
            let messages: Vec<BoxedStrategy<Vec<Message>>> = packets
                .iter()
                .map(|packet| vec(message(packet.clone()), 1..=3).boxed())
                .collect();
            (Just(little), Just(packets), messages)
        })
        .prop_map(|(little, packets, messages)| assemble(little, &packets, messages))
        .boxed()
}

// ---- What a field is -----------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Text {
    Utf8,
    Ascii,
    Latin1,
}

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Uint(u32),
    Int(u32),
    Float(u32),
    Bool,
    Text {
        bytes: u32,
        text: Text,
        terminator: Option<u8>,
    },
    Bytes(u32),
}

impl Kind {
    fn bits(&self) -> u32 {
        match self {
            Kind::Uint(bits) | Kind::Int(bits) | Kind::Float(bits) => *bits,
            Kind::Bool => 1,
            Kind::Text { bytes, .. } | Kind::Bytes(bytes) => bytes * 8,
        }
    }

    /// Whether it has a byte order (§2.1): only integers and floats do.
    fn ordered(&self) -> bool {
        matches!(self, Kind::Uint(_) | Kind::Int(_) | Kind::Float(_))
    }

    /// Whether it starts on a byte whatever its order (§2.2).
    fn on_a_byte(&self) -> bool {
        matches!(self, Kind::Text { .. } | Kind::Bytes(_))
    }

    /// The same kind of integer, `bits` wide.
    fn widened(&self, bits: u32) -> Kind {
        match self {
            Kind::Uint(_) => Kind::Uint(bits),
            Kind::Int(_) => Kind::Int(bits),
            other => other.clone(),
        }
    }
}

/// The param a field maps to, and through what.
#[derive(Debug, Clone, PartialEq)]
enum Meaning {
    U64,
    I64,
    F64,
    Bool,
    Str,
    Bytes,
    /// A string field's text, one of these states.
    TextState(Vec<String>),
    /// An f64 param, through `raw * scale + offset`.
    Linear {
        scale: f64,
        offset: f64,
    },
    /// An enum param, through these (raw, name) states.
    States(Vec<(i128, String)>),
}

#[derive(Debug, Clone)]
struct FieldSpec {
    kind: Kind,
    meaning: Meaning,
    /// Whether it asks for the other byte order than its family's.
    flip: bool,
}

fn field_spec() -> BoxedStrategy<FieldSpec> {
    let kind = prop_oneof![
        3 => (1_u32..=64).prop_map(Kind::Uint),
        3 => (1_u32..=64).prop_map(Kind::Int),
        1 => select(&[32_u32, 64][..]).prop_map(Kind::Float),
        1 => Just(Kind::Bool),
        2 => (
            1_u32..=12,
            select(&[Text::Utf8, Text::Ascii, Text::Latin1][..]),
            proptest::option::of(any::<u8>()),
        )
            .prop_map(|(bytes, text, terminator)| Kind::Text { bytes, text, terminator }),
        1 => (1_u32..=8).prop_map(Kind::Bytes),
    ];
    (kind, any::<bool>())
        .prop_flat_map(|(kind, flip)| {
            (meaning(&kind), Just(kind), Just(flip)).prop_map(|(meaning, kind, flip)| FieldSpec {
                kind,
                meaning,
                flip,
            })
        })
        .boxed()
}

/// A meaning the field can have (validation's mapping rules).
fn meaning(kind: &Kind) -> BoxedStrategy<Meaning> {
    let linear = (scale(), -1e3..1e3_f64)
        .prop_map(|(scale, offset)| Meaning::Linear { scale, offset })
        .boxed();
    match kind {
        Kind::Uint(bits) => {
            let max = (1_i128 << bits) - 1;
            let mut choices = vec![
                Just(Meaning::U64).boxed(),
                Just(Meaning::F64).boxed(),
                Just(Meaning::Bool).boxed(),
                linear,
                states(0..=max),
            ];
            if *bits <= 63 {
                choices.push(Just(Meaning::I64).boxed());
            }
            proptest::strategy::Union::new(choices).boxed()
        }
        Kind::Int(bits) => {
            let half = 1_i128 << (bits - 1);
            let range = -half..=half - 1;
            prop_oneof![
                Just(Meaning::I64),
                Just(Meaning::F64),
                Just(Meaning::Bool),
                linear,
                states(range),
            ]
            .boxed()
        }
        Kind::Float(_) => prop_oneof![Just(Meaning::F64), linear].boxed(),
        Kind::Bool => Just(Meaning::Bool).boxed(),
        // Text states are ASCII names that fit: exactly the length without
        // a terminator, up to it with a NUL one.
        Kind::Text {
            bytes,
            text,
            terminator,
        } if *text != Text::Utf8 && matches!(terminator, None | Some(0)) => {
            let bytes = *bytes as usize;
            let least = if terminator.is_some() { 1 } else { bytes };
            let name = vec(select(&b"abcdefghij"[..]), least..=bytes)
                .prop_map(|letters| letters.into_iter().map(char::from).collect::<String>());
            prop_oneof![
                Just(Meaning::Str),
                btree_set(name, 1..=3)
                    .prop_map(|names| Meaning::TextState(names.into_iter().collect())),
            ]
            .boxed()
        }
        Kind::Text { .. } => Just(Meaning::Str).boxed(),
        Kind::Bytes(_) => Just(Meaning::Bytes).boxed(),
    }
}

/// A linear scale: nonzero, from a thousandth to a thousand either way.
fn scale() -> BoxedStrategy<f64> {
    (1e-3..1e3_f64, any::<bool>())
        .prop_map(|(scale, negative)| if negative { -scale } else { scale })
        .boxed()
}

/// One to four states with distinct raw values in `range`.
fn states(range: RangeInclusive<i128>) -> BoxedStrategy<Meaning> {
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

// ---- Laying a packet out ------------------------------------------------

/// A field at its place.
#[derive(Debug, Clone)]
struct Placed {
    name: String,
    param: String,
    kind: Kind,
    meaning: Meaning,
    /// Its bit position in the packet.
    offset: u64,
    /// Whether it's read little-endian.
    little: bool,
}

#[derive(Debug, Clone)]
struct PacketLayout {
    name: String,
    /// The value of its first byte, which identifies it.
    id: u8,
    fields: Vec<Placed>,
    /// Its entries, padding included, as IR JSON.
    entries: Vec<Value>,
    bits: u64,
}

/// Whether `size` bits at `offset` lie inside one byte.
fn inside_one_byte(offset: u64, size: u32) -> bool {
    offset / 8 == (offset + u64::from(size) - 1) / 8
}

fn lay_out(index: usize, family_little: bool, specs: Vec<FieldSpec>) -> PacketLayout {
    let id = u8::try_from(index + 1).unwrap_or(u8::MAX);
    let mut entries =
        vec![json!({ "kind": "field", "name": "id", "type": "uint", "size_bits": 8 })];
    let mut fields = Vec::new();
    let mut offset: u64 = 8;
    for (position, spec) in specs.into_iter().enumerate() {
        let mut kind = spec.kind;
        let mut meaning = spec.meaning;
        let mut little = family_little;
        let mut explicit = false;
        if spec.flip && kind.ordered() {
            little = !family_little;
            explicit = true;
        }
        let ordered_little = little && kind.ordered();
        if kind.on_a_byte() || (ordered_little && !inside_one_byte(offset, kind.bits())) {
            let skip = (8 - offset % 8) % 8;
            if skip > 0 {
                entries.push(json!({ "kind": "padding", "size_bits": skip }));
                offset += skip;
            }
        }
        if ordered_little && !inside_one_byte(offset, kind.bits()) && kind.bits() % 8 != 0 {
            kind = kind.widened(kind.bits().div_ceil(8) * 8);
            // An i64 param takes a uint of at most 63 bits.
            if kind == Kind::Uint(64) && meaning == Meaning::I64 {
                meaning = Meaning::U64;
            }
        }
        // Inside one byte the order doesn't apply, so it isn't spelled.
        if explicit && inside_one_byte(offset, kind.bits()) {
            explicit = false;
            little = family_little;
        }
        let name = format!("f{position}");
        let param = format!("p{index}.f{position}");
        entries.push(field_json(&name, &param, &kind, &meaning, explicit, little));
        fields.push(Placed {
            name,
            param,
            kind: kind.clone(),
            meaning,
            offset,
            little,
        });
        offset += u64::from(kind.bits());
    }
    PacketLayout {
        name: format!("P{index}"),
        id,
        fields,
        entries,
        bits: offset,
    }
}

fn field_json(
    name: &str,
    param: &str,
    kind: &Kind,
    meaning: &Meaning,
    explicit: bool,
    little: bool,
) -> Value {
    let mut field = Map::new();
    field.insert("kind".into(), json!("field"));
    field.insert("name".into(), json!(name));
    let ty = match kind {
        Kind::Uint(_) => "uint",
        Kind::Int(_) => "int",
        Kind::Float(_) => "float",
        Kind::Bool => "bool",
        Kind::Text { .. } => "string",
        Kind::Bytes(_) => "bytes",
    };
    field.insert("type".into(), json!(ty));
    match kind {
        Kind::Uint(bits) | Kind::Int(bits) | Kind::Float(bits) => {
            field.insert("size_bits".into(), json!(bits));
        }
        Kind::Bool => {
            field.insert("size_bits".into(), json!(1));
        }
        Kind::Text {
            bytes,
            text,
            terminator,
        } => {
            let mut length = json!({ "kind": "fixed", "bytes": bytes });
            if let Some(byte) = terminator {
                length["terminator"] = json!(byte);
            }
            field.insert("length".into(), length);
            match text {
                Text::Utf8 => {}
                Text::Ascii => {
                    field.insert("text".into(), json!("ascii"));
                }
                Text::Latin1 => {
                    field.insert("text".into(), json!("latin1"));
                }
            }
        }
        Kind::Bytes(bytes) => {
            field.insert("length".into(), json!({ "kind": "fixed", "bytes": bytes }));
        }
    }
    if explicit {
        field.insert(
            "byte_order".into(),
            json!(if little { "little" } else { "big" }),
        );
    }
    match meaning {
        Meaning::Linear { scale, offset } => {
            field.insert(
                "conversion".into(),
                json!({ "kind": "linear", "scale": scale, "offset": offset }),
            );
        }
        Meaning::States(states) => {
            let states: Vec<Value> = states
                .iter()
                .map(|(raw, name)| json!({ "raw": integer_json(*raw), "name": name }))
                .collect();
            field.insert(
                "conversion".into(),
                json!({ "kind": "states", "states": states }),
            );
        }
        _ => {}
    }
    field.insert("canonical".into(), json!(param));
    Value::Object(field)
}

fn param_json(meaning: &Meaning) -> Value {
    match meaning {
        Meaning::U64 => json!({ "type": "u64" }),
        Meaning::I64 => json!({ "type": "i64" }),
        Meaning::F64 | Meaning::Linear { .. } => json!({ "type": "f64" }),
        Meaning::Bool => json!({ "type": "bool" }),
        Meaning::Str => json!({ "type": "str" }),
        Meaning::Bytes => json!({ "type": "bytes" }),
        Meaning::TextState(names) => json!({ "type": "enum", "states": names }),
        Meaning::States(states) => {
            let names: Vec<&str> = states.iter().map(|(_, name)| name.as_str()).collect();
            json!({ "type": "enum", "states": names })
        }
    }
}

// ---- Values and packing ---------------------------------------------------

/// A message for `packet`: a raw value for each field, packed over random
/// bytes, so padding and trailing bits are noise the decoder must skip.
fn message(packet: PacketLayout) -> BoxedStrategy<Message> {
    let values: Vec<BoxedStrategy<Raw>> = packet.fields.iter().map(raw_value).collect();
    let length = usize::try_from(packet.bits.div_ceil(8)).unwrap_or(0);
    (values, vec(any::<u8>(), length))
        .prop_map(move |(raws, noise)| pack(&packet, raws, noise))
        .boxed()
}

fn raw_value(field: &Placed) -> BoxedStrategy<Raw> {
    if let Meaning::States(states) = &field.meaning {
        let raws: Vec<i128> = states.iter().map(|(raw, _)| *raw).collect();
        // Each raw value is in the field's range, so it converts.
        return match field.kind {
            Kind::Int(_) => select(raws)
                .prop_map(|raw| Raw::Int(i64::try_from(raw).unwrap_or(0)))
                .boxed(),
            _ => select(raws)
                .prop_map(|raw| Raw::Uint(u64::try_from(raw).unwrap_or(0)))
                .boxed(),
        };
    }
    match &field.kind {
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
        Kind::Text {
            bytes,
            text,
            terminator,
        } => match &field.meaning {
            Meaning::TextState(names) => select(names.clone()).prop_map(Raw::Str).boxed(),
            _ => text_value(*bytes as usize, *text, *terminator),
        },
        Kind::Bytes(bytes) => vec(any::<u8>(), *bytes as usize)
            .prop_map(Raw::Bytes)
            .boxed(),
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

/// Text in `text` that fits a field of `bytes` bytes: exactly that many
/// without a terminator, at most that many, without the terminator byte,
/// with one.
fn text_value(bytes: usize, text: Text, terminator: Option<u8>) -> BoxedStrategy<Raw> {
    let chars = match text {
        Text::Ascii => proptest::char::range('\u{0}', '\u{7f}').boxed(),
        Text::Latin1 => proptest::char::range('\u{0}', '\u{ff}').boxed(),
        Text::Utf8 => any::<char>().boxed(),
    };
    (
        vec(chars, 0..=bytes),
        vec(select(&b"abcdefghij"[..]), bytes),
    )
        .prop_map(move |(chars, filler)| {
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
            // Without a terminator the text fills the field exactly.
            if terminator.is_none() {
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

fn pack(packet: &PacketLayout, raws: Vec<Raw>, mut payload: Vec<u8>) -> Message {
    put_bits(&mut payload, 0, 8, u64::from(packet.id), false);
    let mut record = vec![("id".to_string(), Raw::Uint(u64::from(packet.id)))];
    let mut samples = Vec::new();
    for (field, raw) in packet.fields.iter().zip(raws) {
        let bits = field.kind.bits();
        match (&field.kind, &raw) {
            (_, Raw::Uint(n)) => put_bits(&mut payload, field.offset, bits, *n, field.little),
            (_, Raw::Int(n)) => put_bits(
                &mut payload,
                field.offset,
                bits,
                fit(*n as u64, bits),
                field.little,
            ),
            (Kind::Float(32), Raw::Float(x)) => put_bits(
                &mut payload,
                field.offset,
                32,
                u64::from((*x as f32).to_bits()),
                field.little,
            ),
            (_, Raw::Float(x)) => {
                put_bits(&mut payload, field.offset, 64, x.to_bits(), field.little)
            }
            (_, Raw::Bool(b)) => put_bits(&mut payload, field.offset, 1, u64::from(*b), false),
            (
                Kind::Text {
                    bytes,
                    text,
                    terminator,
                },
                Raw::Str(value),
            ) => {
                let encoded = encode(value, *text);
                let start = (field.offset / 8) as usize;
                payload[start..start + encoded.len()].copy_from_slice(&encoded);
                if let Some(byte) = terminator
                    && encoded.len() < *bytes as usize
                {
                    payload[start + encoded.len()] = *byte;
                }
            }
            (_, Raw::Bytes(value)) => {
                let start = (field.offset / 8) as usize;
                payload[start..start + value.len()].copy_from_slice(value);
            }
            (_, Raw::Str(_)) => {}
        }
        samples.push(sample(field, &raw));
        record.push((field.name.clone(), raw));
    }
    Message {
        packet: packet.name.clone(),
        payload,
        record,
        samples,
    }
}

/// An integer as the IR writes it: a number within ±(2^53 - 1), else a
/// decimal string.
fn integer_json(n: i128) -> Value {
    const SAFE: i128 = (1 << 53) - 1;
    match i64::try_from(n) {
        Ok(small) if n.abs() <= SAFE => json!(small),
        _ => json!(n.to_string()),
    }
}

/// What a field's raw value gives its param (§7.1, §7.3).
fn sample(field: &Placed, raw: &Raw) -> Sample {
    let number = match raw {
        Raw::Uint(n) => *n as f64,
        Raw::Int(n) => *n as f64,
        Raw::Float(x) => *x,
        _ => f64::NAN,
    };
    let integer = match raw {
        Raw::Uint(n) => i128::from(*n),
        Raw::Int(n) => i128::from(*n),
        _ => 0,
    };
    let good = |value| Sample {
        param: field.param.clone(),
        value,
        quality: "good",
    };
    match (&field.meaning, raw) {
        (Meaning::U64, Raw::Uint(n)) => good(Expected::U64(*n)),
        (Meaning::I64, _) => good(Expected::I64(i64::try_from(integer).unwrap_or(0))),
        // NaN or an infinity read is the raw value, `non_finite` (§7.3).
        (Meaning::F64, _) if !number.is_finite() => Sample {
            param: field.param.clone(),
            value: Expected::F64(number),
            quality: "non_finite",
        },
        (Meaning::F64, _) => good(Expected::F64(number)),
        (Meaning::Bool, Raw::Bool(b)) => good(Expected::Bool(*b)),
        (Meaning::Bool, _) => good(Expected::Bool(integer != 0)),
        (Meaning::Str, Raw::Str(text)) => good(Expected::Str(text.clone())),
        (Meaning::TextState(_), Raw::Str(text)) => good(Expected::Enum(text.clone())),
        (Meaning::Bytes, Raw::Bytes(bytes)) => good(Expected::Bytes(bytes.clone())),
        (Meaning::Linear { scale, offset }, _) => {
            let value = number * scale + offset;
            if value.is_finite() {
                good(Expected::F64(value))
            } else {
                Sample {
                    param: field.param.clone(),
                    value: Expected::F64(number),
                    quality: "non_finite",
                }
            }
        }
        (Meaning::States(states), _) => {
            let name = states
                .iter()
                .find(|(state, _)| *state == integer)
                .map_or_else(String::new, |(_, name)| name.clone());
            good(Expected::Enum(name))
        }
        // Not generated: every meaning comes with the raw values it takes.
        (_, _) => good(Expected::Str(String::new())),
    }
}

// ---- The bundle -----------------------------------------------------------

fn assemble(little: bool, packets: &[PacketLayout], messages: Vec<Vec<Message>>) -> Telemetry {
    let mut params = Map::new();
    for field in packets.iter().flat_map(|packet| &packet.fields) {
        params.insert(field.param.clone(), param_json(&field.meaning));
    }
    let packet_json: Vec<Value> = packets
        .iter()
        .map(|packet| {
            json!({ "name": packet.name, "identify": { "id": packet.id },
                    "entries": packet.entries })
        })
        .collect();
    let bundle = json!({
        "mdl_version": "2.0",
        "catalog": { "params": params },
        "families": [{
            "name": "fam",
            "encoding": "binary",
            "byte_order": if little { "little" } else { "big" },
            "alias": { "kind": "fixed", "value": "A1" }
        }],
        "sets": [{ "name": "fam_v1", "family": "fam", "packets": packet_json }]
    });
    Telemetry {
        bundle,
        family: "fam".into(),
        set: "fam_v1".into(),
        native_id: "A1".into(),
        messages: messages.into_iter().flatten().collect(),
    }
}

#[cfg(test)]
mod tests;
