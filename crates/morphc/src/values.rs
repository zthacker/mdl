//! Values in reports: decoded and encoded values as JSON, and bytes as
//! hex both ways.

use mdl::v2::decode::{Record, SampleValue, Value};
use mdl::v2::encode::ArgumentValue;
use serde::Serialize;
use serde_json::Value as Json;

use crate::describe::ir;

/// Why a message didn't decode or a command didn't encode: the library's
/// stable code and its message.
#[derive(Debug, Clone, Serialize)]
pub struct Failure {
    pub code: String,
    pub message: String,
}

/// Bytes from hex digits. Spaces, `_` and `:` may separate groups of
/// digits, and each group may start with `0x` (`0a1b`, `0a 1b`,
/// `0x0a1b`, `0x0a 0x1b`, `0a:1b`).
pub fn parse_hex(text: &str) -> Result<Vec<u8>, String> {
    let mut digits = String::new();
    for group in text.split(|c: char| c.is_whitespace() || c == '_' || c == ':') {
        let group = group
            .strip_prefix("0x")
            .or_else(|| group.strip_prefix("0X"))
            .unwrap_or(group);
        digits.push_str(group);
    }
    if !digits.len().is_multiple_of(2) {
        return Err(format!(
            "{text:?} has an odd number of hex digits; a byte is two"
        ));
    }
    digits
        .as_bytes()
        .chunks(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).map_err(|_| format!("{text:?} isn't hex"))?;
            u8::from_str_radix(pair, 16).map_err(|_| format!("{pair:?} isn't a hex byte"))
        })
        .collect()
}

/// The largest integer every JSON reader holds exactly; larger ones are
/// written as text, as MDL writes them.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

pub fn integer_json(value: i128) -> Json {
    if value.unsigned_abs() <= u128::from(MAX_SAFE_INTEGER) {
        match i64::try_from(value) {
            Ok(n) => Json::from(n),
            Err(_) => Json::String(value.to_string()),
        }
    } else {
        Json::String(value.to_string())
    }
}

/// A float as a JSON number, or, when JSON has no number for it, an
/// object that can't be taken for text: `{"non_finite": "NaN"}`, `"inf"`
/// or `"-inf"`.
pub fn float_json(value: f64) -> Json {
    match serde_json::Number::from_f64(value) {
        Some(n) => Json::Number(n),
        None => {
            let name = if value.is_nan() {
                "NaN"
            } else if value > 0.0 {
                "inf"
            } else {
                "-inf"
            };
            serde_json::json!({ "non_finite": name })
        }
    }
}

/// A value of a kind this morphc doesn't know (one a newer library adds),
/// as an object that can't be taken for a value: `{"unsupported": ...}`
/// with the library's description of it.
pub fn unsupported<T: std::fmt::Debug>(value: &T) -> Json {
    serde_json::json!({ "unsupported": format!("{value:?}") })
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A sample or engineering value: numbers as JSON numbers (integers past
/// ±(2^53 - 1) as text, non-finite floats tagged), bytes as hex, a state
/// as its name, vectors and quaternions as objects with their frame.
pub fn sample_json(value: &SampleValue) -> Json {
    match value {
        SampleValue::F64(f) => float_json(*f),
        SampleValue::I64(n) => integer_json(i128::from(*n)),
        SampleValue::U64(n) => integer_json(i128::from(*n)),
        SampleValue::Bool(b) => Json::Bool(*b),
        SampleValue::Str(text) => Json::String(text.clone()),
        SampleValue::Bytes(bytes) => Json::String(hex(bytes)),
        SampleValue::Enum(state) => Json::String(state.clone()),
        SampleValue::Vec3 { frame, x, y, z, .. } => serde_json::json!({
            "frame": ir(frame), "x": float_json(*x), "y": float_json(*y), "z": float_json(*z)
        }),
        SampleValue::Quat {
            frame, w, x, y, z, ..
        } => serde_json::json!({
            "frame": ir(frame), "w": float_json(*w), "x": float_json(*x),
            "y": float_json(*y), "z": float_json(*z)
        }),
        other => unsupported(other),
    }
}

pub fn argument_json(value: &ArgumentValue) -> Json {
    match value {
        ArgumentValue::Bool(b) => Json::Bool(*b),
        ArgumentValue::Int(n) => integer_json(i128::from(*n)),
        ArgumentValue::Uint(n) => integer_json(i128::from(*n)),
        ArgumentValue::Float(f) => float_json(*f),
        ArgumentValue::Text(text) => Json::String(text.clone()),
        ArgumentValue::Bytes(bytes) => Json::String(hex(bytes)),
        ArgumentValue::List(values) => Json::Array(values.iter().map(argument_json).collect()),
        other => unsupported(other),
    }
}

pub fn record_json(record: &Record<'_>) -> Json {
    Json::Object(
        record
            .iter()
            .map(|(name, value)| (name.to_string(), value_json(value)))
            .collect(),
    )
}

pub fn value_json(value: &Value<'_>) -> Json {
    match value {
        Value::Uint(n) => integer_json(i128::from(*n)),
        Value::Int(n) => integer_json(i128::from(*n)),
        Value::Float(f) => float_json(*f),
        Value::Bool(b) => Json::Bool(*b),
        Value::Str(text) => Json::String(text.clone()),
        Value::Bytes(bytes) => Json::String(hex(bytes)),
        Value::Array(values) => Json::Array(values.iter().map(value_json).collect()),
        Value::Record(record) => record_json(record),
        Value::Null => Json::Null,
        other => unsupported(other),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn hex_takes_separators_and_prefixes() {
        for text in [
            "0a1b",
            "0a 1b",
            "0x0a1b",
            "0x0a 0x1b",
            "0a:1b",
            "0A_1B",
            " 0a1b ",
        ] {
            assert_eq!(parse_hex(text), Ok(vec![0x0a, 0x1b]), "{text}");
        }
        assert!(parse_hex("0a1").unwrap_err().contains("odd number"));
        assert!(parse_hex("zz").unwrap_err().contains("isn't a hex byte"));
        assert_eq!(hex(&[0x0a, 0xff]), "0aff");
    }

    #[test]
    fn json_values_never_pass_for_other_values() {
        assert_eq!(float_json(1.5), json!(1.5));
        assert_eq!(float_json(f64::NAN), json!({ "non_finite": "NaN" }));
        assert_eq!(float_json(f64::INFINITY), json!({ "non_finite": "inf" }));
        assert_eq!(
            float_json(f64::NEG_INFINITY),
            json!({ "non_finite": "-inf" })
        );
        let safe = (1_i128 << 53) - 1;
        assert_eq!(integer_json(safe), json!(9_007_199_254_740_991_i64));
        assert_eq!(integer_json(-safe), json!(-9_007_199_254_740_991_i64));
        assert_eq!(integer_json(safe + 1), json!("9007199254740992"));
        assert_eq!(
            integer_json(i128::from(u64::MAX)),
            json!("18446744073709551615")
        );
        assert_eq!(
            sample_json(&SampleValue::Bytes(vec![0xc0, 0xff])),
            json!("c0ff")
        );
        assert_eq!(unsupported(&Some(1)), json!({ "unsupported": "Some(1)" }));
    }
}
