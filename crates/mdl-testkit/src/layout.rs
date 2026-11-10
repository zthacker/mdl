//! Valid layouts and the messages they decode, built from this module's
//! own reading of `spec/decoding.md`, not from `mdl`: the oracle the
//! decoder is checked against. A generated [`Telemetry`] is a binary family
//! with one set of up to three packets, each a run of fields of every
//! scalar type, size, position and byte order validation allows, mapped to
//! catalog params; and messages packed bit by bit (§2.1), with what each
//! decodes to: every raw value, and every sample.
//!
//! Positions and meaning follow the spec's rules as written there:
//! little-endian values are whole bytes on byte boundaries or sit inside
//! one byte, strings and bytes start on a byte, a `byte_order` is spelled
//! only where it differs from the family's and matters; conversions
//! (linear, poly, tables, splines, states) are evaluated in binary64 as
//! §7.1 writes them, then from the field's unit into the param's
//! (`units.md` §4); sentinels and qualities follow §7.2 and §7.3.

use std::collections::BTreeSet;
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
            Kind::Text { text, terminator, .. }
                if *text != Text::Utf8 && matches!(terminator, None | Some(0))
        )
    }
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

#[derive(Debug, Clone)]
struct FieldSpec {
    kind: Kind,
    meaning: Meaning,
    invalid: Vec<Sentinel>,
    /// Whether it asks for the other byte order than its family's.
    flip: bool,
}

fn field_spec() -> BoxedStrategy<FieldSpec> {
    let kind = prop_oneof![
        3 => (1_u32..=64).prop_map(Kind::Uint),
        3 => (1_u32..=64).prop_map(Kind::Int),
        2 => select(&[32_u32, 64][..]).prop_map(Kind::Float),
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
            (meaning(&kind), sentinels(&kind), Just(kind), Just(flip)).prop_map(
                |(meaning, invalid, kind, flip)| FieldSpec {
                    kind,
                    meaning,
                    invalid,
                    flip,
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

/// ASCII letters that fit a field that [`Kind::takes_letters`]: exactly its
/// length without a terminator, up to it with one.
fn letters(kind: &Kind) -> BoxedStrategy<String> {
    let (bytes, least) = match kind {
        Kind::Text {
            bytes, terminator, ..
        } => {
            let bytes = *bytes as usize;
            (bytes, if terminator.is_some() { 1 } else { bytes })
        }
        _ => (1, 1),
    };
    vec(select(&b"abcdefghij"[..]), least..=bytes)
        .prop_map(|letters| letters.into_iter().map(char::from).collect())
        .boxed()
}

/// An f64 meaning: through any numeric conversion, in any units.
fn number(kind: &Kind) -> BoxedStrategy<Meaning> {
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
    (conversion, proptest::option::of(units()))
        .prop_map(|(conversion, units)| Meaning::Number { conversion, units })
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

// ---- Units ------------------------------------------------------------------

/// Codes of one dimension each, which convert into each other.
const UNIT_GROUPS: &[&[&str]] = &[
    &["mV", "V", "kV"],
    &["mA", "A"],
    &["K", "Cel", "[degF]"],
    &["ms", "s", "min", "h"],
    &["m", "km", "[ft_i]"],
    &["rad", "mrad", "deg"],
];

/// A field's unit and its param's, of one dimension (maybe the same code).
fn units() -> BoxedStrategy<(&'static str, &'static str)> {
    select(UNIT_GROUPS)
        .prop_flat_map(|group| (select(group), select(group)))
        .boxed()
}

/// A unit's factor (`units.md` §2): its prefix's factor times its atom's,
/// in binary64, for the codes of [`UNIT_GROUPS`].
fn factor(code: &str) -> f64 {
    match code {
        "mV" => 1e-3 * 1000.0,
        "V" => 1000.0,
        "kV" => 1e3 * 1000.0,
        "mA" | "ms" | "mrad" => 1e-3 * 1.0,
        "km" => 1e3 * 1.0,
        "min" => 60.0,
        "h" => 3600.0,
        "[ft_i]" => 0.3048,
        "deg" => 0.017453292519943295,
        _ => 1.0,
    }
}

/// `v` from unit `from` into `to` (`units.md` §4).
fn convert_unit(v: f64, from: &str, to: &str) -> f64 {
    if from == to {
        return v;
    }
    let temperature = |code: &str| matches!(code, "Cel" | "[degF]");
    if !temperature(from) && !temperature(to) {
        return (v * factor(from)) / factor(to);
    }
    let kelvin = match from {
        "Cel" => v + 273.15,
        "[degF]" => ((v + 459.67) * 5.0) / 9.0,
        other => v * factor(other),
    };
    match to {
        "Cel" => kelvin - 273.15,
        "[degF]" => ((kelvin * 9.0) / 5.0) - 459.67,
        other => kelvin / factor(other),
    }
}

// ---- Conversions --------------------------------------------------------

/// `x` through a numeric conversion (§7.1); `None` outside a table's or
/// spline's points when it doesn't extrapolate.
fn convert(conversion: &Numeric, x: f64) -> Option<f64> {
    match conversion {
        Numeric::Plain => Some(x),
        Numeric::Linear { scale, offset } => Some(x * scale + offset),
        Numeric::Poly(coefficients) => {
            let mut highest_first = coefficients.iter().rev();
            let mut y = highest_first.next().copied().unwrap_or(0.0);
            for c in highest_first {
                y = y * x + c;
            }
            Some(y)
        }
        Numeric::Table {
            points,
            step,
            extrapolate,
        } => table(points, *step, *extrapolate, x),
        Numeric::Spline {
            points,
            extrapolate,
        } => spline(points, *extrapolate, x),
    }
}

/// The segment `x` falls in: `Ok(i)` between points `i` and `i + 1`,
/// `Err(true)` below the first point, `Err(false)` above the last.
fn segment(points: &[(f64, f64)], x: f64) -> Result<usize, bool> {
    let last = points.len() - 1;
    if x < points[0].0 {
        return Err(true);
    }
    if x > points[last].0 {
        return Err(false);
    }
    Ok((0..last)
        .find(|&i| points[i].0 < x && x < points[i + 1].0)
        .unwrap_or(0))
}

fn table(points: &[(f64, f64)], step: bool, extrapolate: bool, x: f64) -> Option<f64> {
    if let Some(&(_, value)) = points.iter().find(|(raw, _)| *raw == x) {
        return Some(value);
    }
    let last = points.len() - 1;
    let i = match segment(points, x) {
        Ok(i) => i,
        Err(_) if !extrapolate => return None,
        // Extrapolated, a step table gives the nearest end point's value.
        Err(below) if step => return Some(if below { points[0].1 } else { points[last].1 }),
        Err(below) => {
            if below {
                0
            } else {
                last - 1
            }
        }
    };
    let ((ri, vi), (rj, vj)) = (points[i], points[i + 1]);
    if step {
        return Some(vi);
    }
    Some(vi + ((x - ri) * (vj - vi)) / (rj - ri))
}

/// The natural cubic spline through the points, by §7.1's sweep.
fn spline(points: &[(f64, f64)], extrapolate: bool, x: f64) -> Option<f64> {
    if let Some(&(_, value)) = points.iter().find(|(raw, _)| *raw == x) {
        return Some(value);
    }
    let n = points.len() - 1;
    let r: Vec<f64> = points.iter().map(|point| point.0).collect();
    let v: Vec<f64> = points.iter().map(|point| point.1).collect();
    let h: Vec<f64> = (0..n).map(|i| r[i + 1] - r[i]).collect();
    let s: Vec<f64> = (0..n).map(|i| (v[i + 1] - v[i]) / h[i]).collect();
    let mut c = vec![0.0; n];
    let mut d = vec![0.0; n];
    for i in 1..n {
        let p = 2.0 * (h[i - 1] + h[i]) - h[i - 1] * c[i - 1];
        c[i] = h[i] / p;
        d[i] = (6.0 * (s[i] - s[i - 1]) - h[i - 1] * d[i - 1]) / p;
    }
    let mut m = vec![0.0; n + 1];
    if n >= 2 {
        m[n - 1] = d[n - 1];
        for i in (1..n - 1).rev() {
            m[i] = d[i] - c[i] * m[i + 1];
        }
    }
    let i = match segment(points, x) {
        Ok(i) => i,
        Err(_) if !extrapolate => return None,
        Err(true) => return Some(v[0] + (x - r[0]) * (s[0] - h[0] * m[1] / 6.0)),
        Err(false) => {
            return Some(v[n] + (x - r[n]) * (s[n - 1] + h[n - 1] * m[n - 1] / 6.0));
        }
    };
    let left = r[i + 1] - x;
    let right = x - r[i];
    Some(
        (m[i] * left * left * left + m[i + 1] * right * right * right) / (6.0 * h[i])
            + ((v[i] - m[i] * h[i] * h[i] / 6.0) * left) / h[i]
            + ((v[i + 1] - m[i + 1] * h[i] * h[i] / 6.0) * right) / h[i],
    )
}

// ---- Laying a packet out ------------------------------------------------

/// A field at its place.
#[derive(Debug, Clone)]
struct Placed {
    name: String,
    param: String,
    kind: Kind,
    meaning: Meaning,
    invalid: Vec<Sentinel>,
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
        // Widening keeps every value, state, point and sentinel it held.
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
        let field = Placed {
            name: format!("f{position}"),
            param: format!("p{index}.f{position}"),
            kind,
            meaning,
            invalid: spec.invalid,
            offset,
            little,
        };
        entries.push(field_json(&field, explicit));
        offset += u64::from(field.kind.bits());
        fields.push(field);
    }
    PacketLayout {
        name: format!("P{index}"),
        id,
        fields,
        entries,
        bits: offset,
    }
}

fn field_json(field: &Placed, explicit: bool) -> Value {
    let mut json = Map::new();
    json.insert("kind".into(), json!("field"));
    json.insert("name".into(), json!(field.name));
    let ty = match field.kind {
        Kind::Uint(_) => "uint",
        Kind::Int(_) => "int",
        Kind::Float(_) => "float",
        Kind::Bool => "bool",
        Kind::Text { .. } => "string",
        Kind::Bytes(_) => "bytes",
    };
    json.insert("type".into(), json!(ty));
    match &field.kind {
        Kind::Uint(bits) | Kind::Int(bits) | Kind::Float(bits) => {
            json.insert("size_bits".into(), json!(bits));
        }
        Kind::Bool => {
            json.insert("size_bits".into(), json!(1));
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
            json.insert("length".into(), length);
            match text {
                Text::Utf8 => {}
                Text::Ascii => {
                    json.insert("text".into(), json!("ascii"));
                }
                Text::Latin1 => {
                    json.insert("text".into(), json!("latin1"));
                }
            }
        }
        Kind::Bytes(bytes) => {
            json.insert("length".into(), json!({ "kind": "fixed", "bytes": bytes }));
        }
    }
    if explicit {
        json.insert(
            "byte_order".into(),
            json!(if field.little { "little" } else { "big" }),
        );
    }
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
    json.insert("canonical".into(), json!(field.param));
    Value::Object(json)
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

/// An integer as the IR writes it: a number within ±(2^53 - 1), else a
/// decimal string.
fn integer_json(n: i128) -> Value {
    const SAFE: i128 = (1 << 53) - 1;
    match i64::try_from(n) {
        Ok(small) if n.abs() <= SAFE => json!(small),
        _ => json!(n.to_string()),
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
        Kind::Text {
            bytes,
            text,
            terminator,
        } => text_value(*bytes as usize, *text, *terminator),
        Kind::Bytes(bytes) => vec(any::<u8>(), *bytes as usize)
            .prop_map(Raw::Bytes)
            .boxed(),
    }
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

// ---- What a value means -----------------------------------------------

/// The raw value as a sample states it when its quality isn't `good`.
fn as_raw(raw: &Raw) -> Expected {
    match raw {
        Raw::Uint(n) => Expected::U64(*n),
        Raw::Int(n) => Expected::I64(*n),
        Raw::Float(x) => Expected::F64(*x),
        Raw::Bool(b) => Expected::Bool(*b),
        Raw::Str(text) => Expected::Str(text.clone()),
        Raw::Bytes(bytes) => Expected::Bytes(bytes.clone()),
    }
}

/// Whether the raw value is one of the field's sentinels (§7.2): text
/// exactly, an integer or a float numerically.
fn is_sentinel(field: &Placed, raw: &Raw) -> bool {
    field.invalid.iter().any(|sentinel| match (sentinel, raw) {
        (Sentinel::Int(n), Raw::Uint(m)) => i128::from(*m) == *n,
        (Sentinel::Int(n), Raw::Int(m)) => i128::from(*m) == *n,
        // Sentinels for floats are small, so the double holds them.
        (Sentinel::Int(n), Raw::Float(x)) => *x == *n as f64,
        (Sentinel::Text(text), Raw::Str(value)) => text == value,
        _ => false,
    })
}

/// What a field's raw value gives its param (§7.1 to §7.3).
fn sample(field: &Placed, raw: &Raw) -> Sample {
    let (value, quality) = meaning_of(field, raw);
    Sample {
        param: field.param.clone(),
        value,
        quality,
    }
}

fn meaning_of(field: &Placed, raw: &Raw) -> (Expected, &'static str) {
    if is_sentinel(field, raw) {
        return (as_raw(raw), "invalid");
    }
    let integer = match raw {
        Raw::Uint(n) => Some(i128::from(*n)),
        Raw::Int(n) => Some(i128::from(*n)),
        _ => None,
    };
    let good = |value| (value, "good");
    match (&field.meaning, raw) {
        (Meaning::U64, Raw::Uint(n)) => good(Expected::U64(*n)),
        (Meaning::I64, _) => good(Expected::I64(
            integer.and_then(|n| i64::try_from(n).ok()).unwrap_or(0),
        )),
        (Meaning::Bool, Raw::Bool(b)) => good(Expected::Bool(*b)),
        (Meaning::Bool, _) => good(Expected::Bool(integer.unwrap_or(0) != 0)),
        (Meaning::Str, Raw::Str(text)) => good(Expected::Str(text.clone())),
        (Meaning::Bytes, Raw::Bytes(bytes)) => good(Expected::Bytes(bytes.clone())),
        (Meaning::TextState(names), Raw::Str(text)) if names.contains(text) => {
            good(Expected::Enum(text.clone()))
        }
        (Meaning::States(states), _) => {
            match states.iter().find(|(state, _)| Some(*state) == integer) {
                Some((_, name)) => good(Expected::Enum(name.clone())),
                None => (as_raw(raw), "unknown_state"),
            }
        }
        (Meaning::Number { conversion, units }, _) => {
            let x = match raw {
                Raw::Uint(n) => *n as f64,
                Raw::Int(n) => *n as f64,
                Raw::Float(x) => *x,
                _ => f64::NAN,
            };
            // NaN or an infinity read is the raw value, whatever converts
            // it; NaN is never outside a table.
            if !x.is_finite() {
                return (as_raw(raw), "non_finite");
            }
            let Some(converted) = convert(conversion, x) else {
                return (as_raw(raw), "out_of_calibration");
            };
            let value = match units {
                Some((from, to)) => convert_unit(converted, from, to),
                None => converted,
            };
            if value.is_finite() {
                good(Expected::F64(value))
            } else {
                (as_raw(raw), "non_finite")
            }
        }
        // A text state the param doesn't list.
        (_, _) => (as_raw(raw), "unknown_state"),
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
mod reference;
#[cfg(test)]
mod tests;
