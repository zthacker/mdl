//! IR values as short text for people: `uint16`, `n == 0`,
//! `linear scale 0.001 offset 0`. Anything without a short form is shown
//! as the IR writes it.

use mdl::v2::ir::{
    ByteLength, Condition, Conversion, Count, Element, Entry, FromField, Length, Limits, Locator,
    Scalar, ScalarElement, Time,
};
use mdl::v2::layout::Placement;
use serde::Serialize;
use serde_json::Value;

/// A value as the IR writes it: text as it is (`uint`, `big`, `ECEF`),
/// anything else as compact JSON.
pub fn ir<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(text)) => text,
        Ok(other) => other.to_string(),
        Err(err) => format!("(unprintable: {err})"),
    }
}

pub fn scalar(value: &Scalar) -> String {
    match value {
        Scalar::Int(n) => n.to_string(),
        Scalar::Str(text) => Value::String(text.clone()).to_string(),
    }
}

/// `bits 16..24`, `metadata apid`, `path /id`, `fixed SAT1`.
pub fn locator(locator: &Locator) -> String {
    match locator {
        Locator::Bits {
            offset_bits,
            size_bits,
            ..
        } => {
            let end = u64::from(*offset_bits) + u64::from(*size_bits);
            format!("bits {offset_bits}..{end}")
        }
        Locator::Metadata { name, .. } => format!("metadata {name}"),
        Locator::Path { pointer, .. } => format!("path {pointer}"),
        Locator::Fixed { value, .. } => format!("fixed {value}"),
        other => ir(other),
    }
}

/// How a set finds event time: `CUC 4+2 bytes at bit 0 from
/// 2000-01-01T00:00:00Z (tai)`, `seconds since boot`.
pub fn time(time: &Time) -> String {
    match time {
        Time::Cuc {
            offset_bits,
            coarse_bytes,
            fine_bytes,
            epoch,
            scale,
            ..
        } => format!(
            "CUC {coarse_bytes}+{fine_bytes} bytes at bit {offset_bits} from {epoch} ({})",
            ir(scale)
        ),
        Time::Iso8601 { pointer, .. } => format!("ISO 8601 text at {pointer}"),
        Time::Boot { unit, .. } => format!("{} since boot", ir(unit)),
        Time::Elapsed {
            unit, epoch, scale, ..
        } => format!("{} since {epoch} ({})", ir(unit), ir(scale)),
        Time::Gps {
            unit, week_field, ..
        } => format!("GPS week in {week_field}, {} of week", ir(unit)),
        other => ir(other),
    }
}

/// `exact 12 bytes`, `at least 4 bytes`.
pub fn length(length: &Length) -> String {
    match length {
        Length::Exact { bytes, .. } => format!("exact {bytes} bytes"),
        Length::AtLeast { bytes, .. } => format!("at least {bytes} bytes"),
        other => ir(other),
    }
}

/// `n == 0`, `mode in ["A", "B"]`, `a == 1 and not (b == 2 or c == 3)`.
pub fn condition(condition: &Condition) -> String {
    let joined = |conditions: &[Condition], with: &str| {
        let parts: Vec<String> = conditions.iter().map(nested).collect();
        parts.join(with)
    };
    match condition {
        Condition::Compare {
            field, op, value, ..
        } => {
            let op = match ir(op).as_str() {
                "eq" => "==".to_string(),
                "ne" => "!=".to_string(),
                "lt" => "<".to_string(),
                "le" => "<=".to_string(),
                "gt" => ">".to_string(),
                "ge" => ">=".to_string(),
                "mask_any" => "has any bits of".to_string(),
                "mask_all" => "has all bits of".to_string(),
                other => other.to_string(),
            };
            format!("{field} {op} {}", scalar(value))
        }
        Condition::In { field, values, .. } => {
            let values: Vec<String> = values.iter().map(scalar).collect();
            format!("{field} in [{}]", values.join(", "))
        }
        Condition::All { conditions, .. } => joined(conditions, " and "),
        Condition::Any { conditions, .. } => joined(conditions, " or "),
        Condition::Not {
            condition: inner, ..
        } => format!("not ({})", self::condition(inner)),
        other => ir(other),
    }
}

/// A condition inside another, in parentheses when it combines several.
fn nested(inner: &Condition) -> String {
    match inner {
        Condition::All { .. } | Condition::Any { .. } => format!("({})", condition(inner)),
        _ => condition(inner),
    }
}

/// `linear scale 0.001 offset 0`, `polynomial 1 + 0.5*x + 2*x^2`,
/// `states OFF=0, ON=1`, `table of 5 points`.
pub fn conversion(conversion: &Conversion) -> String {
    match conversion {
        Conversion::Linear { scale, offset, .. } => format!("linear scale {scale} offset {offset}"),
        Conversion::Poly { coefficients, .. } => {
            let terms: Vec<String> = coefficients
                .iter()
                .enumerate()
                .map(|(power, c)| match power {
                    0 => c.to_string(),
                    1 => format!("{c}*x"),
                    _ => format!("{c}*x^{power}"),
                })
                .collect();
            format!("polynomial {}", terms.join(" + "))
        }
        Conversion::States { states, .. } => {
            let states: Vec<String> = states
                .iter()
                .map(|state| format!("{}={}", state.name, state.raw))
                .collect();
            format!("states {}", states.join(", "))
        }
        Conversion::Table {
            points,
            interpolation,
            ..
        } => {
            let step = interpolation.as_ref().is_some_and(|i| ir(i) == "step");
            let kind = if step { "step table" } else { "table" };
            format!("{kind} of {} points", points.len())
        }
        Conversion::Spline { points, .. } => format!("spline through {} points", points.len()),
        other => ir(other),
    }
}

/// `n`, or `n * 2 + 1` with its adjustment.
fn from_field(from: &FromField) -> String {
    let mut text = from.field.clone();
    if let Some(multiply) = from.multiply {
        text.push_str(&format!(" * {multiply}"));
    }
    match from.add {
        Some(add) if add < 0 => text.push_str(&format!(" - {}", add.unsigned_abs())),
        Some(add) => text.push_str(&format!(" + {add}")),
        None => {}
    }
    text
}

/// A scalar's type: `uint16`, `float32`, `bool`, `string(8)`,
/// `string(8, ..0x00)` (ending at the first NUL), `bytes(n)`,
/// `string(..0x00)`, `bytes(to the end)`, with ` le` or ` be` when it names
/// its own byte order.
fn scalar_type(
    ty: &mdl::v2::ir::ScalarType,
    size_bits: Option<u32>,
    length: Option<&ByteLength>,
    byte_order: Option<&mdl::v2::ir::ByteOrder>,
) -> String {
    let name = ir(ty);
    let mut text = match (name.as_str(), length) {
        ("string" | "bytes", Some(length)) => {
            let length = match length {
                ByteLength::Fixed {
                    bytes, terminator, ..
                } => match terminator {
                    Some(byte) => format!("{bytes}, ..0x{byte:02x}"),
                    None => bytes.to_string(),
                },
                ByteLength::Field(from) => from_field(from),
                ByteLength::Terminator { byte, .. } => format!("..0x{byte:02x}"),
                ByteLength::Remaining { .. } => TO_THE_END.to_string(),
                other => ir(other),
            };
            format!("{name}({length})")
        }
        ("bool", _) => name,
        (_, _) => match size_bits {
            Some(bits) => format!("{name}{bits}"),
            None => name,
        },
    };
    match byte_order.map(ir).as_deref() {
        Some("little") => text.push_str(" le"),
        Some("big") => text.push_str(" be"),
        _ => {}
    }
    text
}

fn element_type(element: &Element) -> String {
    match element {
        Element::Scalar(scalar) => scalar_element(scalar),
        Element::Struct { name, .. } => format!("struct {name}"),
        other => ir(other),
    }
}

fn scalar_element(element: &ScalarElement) -> String {
    scalar_type(
        &element.ty,
        element.size_bits,
        element.length.as_ref(),
        element.byte_order.as_ref(),
    )
}

/// An entry's type: `uint16`, `struct xy`, `uint8 x n`, `struct cell[]`,
/// `crc16`, `iso8473`, `padding`.
pub fn entry_type(entry: &Entry) -> String {
    match entry {
        Entry::Field(field) => scalar_type(
            &field.ty,
            field.size_bits,
            field.length.as_ref(),
            field.byte_order.as_ref(),
        ),
        Entry::Array(array) => {
            let element = element_type(&array.element);
            let count = match &array.count {
                Some(Count::Fixed { count, .. }) => count.to_string(),
                Some(Count::Field(from)) => from_field(from),
                Some(Count::Remaining { .. }) => format!("({TO_THE_END})"),
                Some(other) => ir(other),
                // A self-describing array: every element the payload has.
                None => return format!("{element}[]"),
            };
            format!("{element} x {count}")
        }
        Entry::Struct(entry) => format!("struct {}", entry.struct_type),
        Entry::Padding(_) => "padding".to_string(),
        Entry::Checksum(checksum) => {
            // The algorithm's kind, with its width when it has a choice of
            // widths (`crc16`, `sum8`); `xor` and `iso8473` have one.
            let algorithm = serde_json::to_value(&checksum.algorithm).unwrap_or(Value::Null);
            let kind = algorithm
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("checksum");
            match algorithm.get("width") {
                Some(width) => format!("{kind}{width}"),
                None => kind.to_string(),
            }
        }
        other => ir(other),
    }
}

/// An entry's name; padding has none.
pub fn entry_name(entry: &Entry) -> Option<&str> {
    match entry {
        Entry::Field(field) => Some(&field.name),
        Entry::Array(array) => Some(&array.name),
        Entry::Struct(entry) => Some(&entry.name),
        Entry::Checksum(checksum) => Some(&checksum.name),
        _ => None,
    }
}

/// Bounds written as `min` and `max` (an argument's range, a list's
/// length): `0..10`, `..255`, `1..`.
pub fn bounds<T: Serialize>(value: &T) -> String {
    let value = serde_json::to_value(value).unwrap_or(Value::Null);
    let end = |key: &str| match value.get(key) {
        Some(Value::Number(n)) => n.as_f64().map_or_else(|| n.to_string(), |f| f.to_string()),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    format!("{}..{}", end("min"), end("max"))
}

/// `red low 24, yellow high 30, red high 34`.
pub fn limits(limits: &Limits) -> String {
    let ends = [
        ("red low", limits.red_low),
        ("yellow low", limits.yellow_low),
        ("yellow high", limits.yellow_high),
        ("red high", limits.red_high),
    ];
    let given: Vec<String> = ends
        .iter()
        .filter_map(|(name, value)| value.map(|v| format!("{name} {v}")))
        .collect();
    given.join(", ")
}

/// `16`, `0..2040`, `8..`: bits, or a range of them.
pub fn bits(min: u64, max: Option<u64>) -> String {
    match max {
        Some(max) if max == min => min.to_string(),
        Some(max) => format!("{min}..{max}"),
        None => format!("{min}.."),
    }
}

/// Where an entry starts: its static position; the one bit it always
/// starts at without having a static position, marked [`NOT_STATIC`]; or
/// the range it can start in.
pub fn start(placement: &Placement) -> String {
    match placement.static_offset_bits {
        Some(offset) => offset.to_string(),
        None if placement.start_max_bits == Some(placement.start_min_bits) => {
            format!("{}{NOT_STATIC}", placement.start_min_bits)
        }
        None => bits(placement.start_min_bits, placement.start_max_bits),
    }
}

/// Marks a start that never varies but isn't a static position.
pub const NOT_STATIC: char = '*';

const TO_THE_END: &str = "to the end";

/// The size an entry has when present.
pub fn size(placement: &Placement) -> String {
    bits(placement.size_min_bits, placement.size_max_bits)
}
