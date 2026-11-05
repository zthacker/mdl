//! The interpreter: what a decoder runs, with a `Plan`, on every message.
//!
//! - `extract`: payload + packet definition → each field's raw value,
//!   under its vendor name. Nothing is converted yet, so a consumer can keep
//!   exactly what the asset sent.
//! - `convert`: one raw value → its engineering value, or a
//!   sentinel / unknown-state outcome that becomes a quality flag.
//!
//! Both assume validated definitions (the plan guarantees it), but neither
//! trusts the payload: short binary data, a missing JSON path or a value of
//! the wrong type is an error for that message, never a panic.

use thiserror::Error;

use crate::bits::{BitsError, extract_bits, sign_extend};
use crate::ir::{BitRange, Conversion, DefinitionSet, Endian, Field, FieldType, Location, Packet};

/// A payload as the link's stack hands it over (after the envelope).
#[derive(Debug, Clone, Copy)]
pub enum Payload<'a> {
    Binary(&'a [u8]),
    Json(&'a serde_json::Value),
}

/// One value, raw or converted.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    U64(u64),
    I64(i64),
    F64(f64),
    Bool(bool),
    Str(String),
}

impl Value {
    /// As a number, for linear and poly conversions. Integers above 2^53
    /// lose precision, as they would anywhere they become an f64.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::U64(v) => Some(*v as f64),
            Value::I64(v) => Some(*v as f64),
            Value::F64(v) => Some(*v),
            Value::Bool(_) | Value::Str(_) => None,
        }
    }

    /// As an integer, for `invalid` sentinels and states.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::U64(v) => i64::try_from(*v).ok(),
            Value::I64(v) => Some(*v),
            Value::F64(_) | Value::Bool(_) | Value::Str(_) => None,
        }
    }
}

/// A field's raw value under its vendor name.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldValue {
    pub name: String,
    pub value: Value,
}

/// What `convert` makes of a raw value.
#[derive(Debug, Clone, PartialEq)]
pub enum Converted {
    Value(Value),
    /// The raw value is one the definition lists in `invalid`: no reading.
    Sentinel,
    /// A states conversion has no name for this raw value.
    UnknownState(i64),
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum InterpretError {
    #[error("packet {packet} needs {need} bytes, got {got}")]
    ShortPayload {
        packet: String,
        need: usize,
        got: usize,
    },
    #[error("packet {packet} is defined for a {expected} payload")]
    WrongEncoding {
        packet: String,
        expected: &'static str,
    },
    #[error("field {field}: {source}")]
    Bits { field: String, source: BitsError },
    #[error("field {field}: no value at {pointer}")]
    MissingPath { field: String, pointer: String },
    #[error("field {field}: expected {expected:?}, got {got}")]
    WrongType {
        field: String,
        expected: FieldType,
        got: String,
    },
    #[error("field {field}: a {conversion} conversion needs a number, got {value:?}")]
    NotANumber {
        field: String,
        conversion: &'static str,
        value: Value,
    },
}

/// Read every field of `packet` from `payload`, in definition order.
pub fn extract(
    set: &DefinitionSet,
    packet: &Packet,
    payload: Payload,
) -> Result<Vec<FieldValue>, InterpretError> {
    if let Payload::Binary(bytes) = payload {
        // Longer is fine: a vendor that appends fields mustn't break us
        // (DDIA Ch. 5, forward compatibility). Shorter means truncation.
        let need = packet.length_bytes.unwrap_or(0) as usize;
        if bytes.len() < need {
            return Err(InterpretError::ShortPayload {
                packet: packet.name.clone(),
                need,
                got: bytes.len(),
            });
        }
    }
    packet
        .fields
        .iter()
        .map(|field| {
            let value = match (&field.location, payload) {
                (Location::Bits(range), Payload::Binary(bytes)) => {
                    let endian = set.endian.unwrap_or(Endian::Big);
                    read_bits(bytes, field, range, endian)?
                }
                (Location::Path { pointer }, Payload::Json(doc)) => read_json(doc, field, pointer)?,
                (Location::Bits(_), Payload::Json(_)) => {
                    return Err(wrong_encoding(packet, "binary"));
                }
                (Location::Path { .. }, Payload::Binary(_)) => {
                    return Err(wrong_encoding(packet, "json"));
                }
            };
            Ok(FieldValue {
                name: field.name.clone(),
                value,
            })
        })
        .collect()
}

fn wrong_encoding(packet: &Packet, expected: &'static str) -> InterpretError {
    InterpretError::WrongEncoding {
        packet: packet.name.clone(),
        expected,
    }
}

fn read_bits(
    bytes: &[u8],
    field: &Field,
    range: &BitRange,
    endian: Endian,
) -> Result<Value, InterpretError> {
    let raw =
        extract_bits(bytes, range.offset_bits, range.size_bits, endian).map_err(|source| {
            InterpretError::Bits {
                field: field.name.clone(),
                source,
            }
        })?;
    Ok(match field.ty {
        FieldType::Uint => Value::U64(raw),
        FieldType::Int => Value::I64(sign_extend(raw, range.size_bits)),
        // extract_bits returned only 32 bits, so `as u32` drops nothing.
        FieldType::Float if range.size_bits == 32 => {
            Value::F64(f64::from(f32::from_bits(raw as u32)))
        }
        FieldType::Float => Value::F64(f64::from_bits(raw)),
        FieldType::Bool => Value::Bool(raw != 0),
        FieldType::String => {
            return Err(InterpretError::WrongType {
                field: field.name.clone(),
                expected: FieldType::String,
                got: "bits".into(),
            });
        }
    })
}

fn read_json(
    doc: &serde_json::Value,
    field: &Field,
    pointer: &str,
) -> Result<Value, InterpretError> {
    let found = doc
        .pointer(pointer)
        .ok_or_else(|| InterpretError::MissingPath {
            field: field.name.clone(),
            pointer: pointer.to_string(),
        })?;
    let value = match field.ty {
        FieldType::Uint => found.as_u64().map(Value::U64),
        FieldType::Int => found.as_i64().map(Value::I64),
        FieldType::Float => found.as_f64().map(Value::F64),
        FieldType::Bool => found.as_bool().map(Value::Bool),
        FieldType::String => found.as_str().map(|s| Value::Str(s.to_string())),
    };
    value.ok_or_else(|| InterpretError::WrongType {
        field: field.name.clone(),
        expected: field.ty,
        got: found.to_string(),
    })
}

/// Turn one raw value into its engineering value.
pub fn convert(field: &Field, raw: &Value) -> Result<Converted, InterpretError> {
    if let Some(int) = raw.as_i64()
        && field.invalid.contains(&int)
    {
        return Ok(Converted::Sentinel);
    }
    let not_a_number = |conversion| InterpretError::NotANumber {
        field: field.name.clone(),
        conversion,
        value: raw.clone(),
    };
    match &field.conversion {
        None => Ok(Converted::Value(raw.clone())),
        Some(Conversion::Linear { scale, offset }) => {
            let x = raw.as_f64().ok_or_else(|| not_a_number("linear"))?;
            Ok(Converted::Value(Value::F64(x * scale + offset)))
        }
        Some(Conversion::Poly { coefficients }) => {
            let x = raw.as_f64().ok_or_else(|| not_a_number("poly"))?;
            // Horner's rule: c0 + x(c1 + x(c2 + ...)), highest power first.
            let y = coefficients.iter().rev().fold(0.0, |acc, c| acc * x + c);
            Ok(Converted::Value(Value::F64(y)))
        }
        Some(Conversion::States { states }) => {
            let n = raw.as_i64().ok_or_else(|| not_a_number("states"))?;
            Ok(match states.iter().find(|state| state.raw == n) {
                Some(state) => Converted::Value(Value::Str(state.name.clone())),
                None => Converted::UnknownState(n),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::toml::load_dir;
    use crate::plan::Plan;
    use crate::validate::validate;
    use std::path::Path;

    /// The committed definitions, compiled the way morphc does.
    fn plan() -> Plan {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/definitions");
        validate(load_dir(&dir).unwrap()).unwrap()
    }

    fn lookup<'p>(plan: &'p Plan, set: &str, packet: &str) -> (&'p DefinitionSet, &'p Packet) {
        let set = &plan.definitions().sets[set];
        let packet = set.packets.iter().find(|p| p.name == packet).unwrap();
        (set, packet)
    }

    fn field<'p>(packet: &'p Packet, name: &str) -> &'p Field {
        packet.fields.iter().find(|f| f.name == name).unwrap()
    }

    fn value_of(values: &[FieldValue], name: &str) -> Value {
        values
            .iter()
            .find(|v| v.name == name)
            .unwrap()
            .value
            .clone()
    }

    // ---- extract ----

    #[test]
    fn extracts_sat1_eps_hk_big_endian() {
        let plan = plan();
        let (set, packet) = lookup(&plan, "vendor_a", "EPS_HK");
        let mut payload = [0u8; 15];
        payload[8..10].copy_from_slice(&742u16.to_be_bytes()); // bus voltage counts
        payload[10..12].copy_from_slice(&(-1500i16).to_be_bytes()); // -1.5 A
        payload[14] = 0b0000_0001; // mode NOMINAL

        let values = extract(set, packet, Payload::Binary(&payload)).unwrap();
        assert_eq!(value_of(&values, "bus_voltage_counts"), Value::U64(742));
        assert_eq!(value_of(&values, "batt_current_raw"), Value::I64(-1500));
        assert_eq!(value_of(&values, "mode"), Value::U64(1));
    }

    #[test]
    fn extracts_uav1_little_endian_ints_and_floats() {
        let plan = plan();
        let (set, packet) = lookup(&plan, "vendor_c", "ATTITUDE");
        let mut payload = [0u8; 28];
        payload[0..4].copy_from_slice(&123_456u32.to_le_bytes());
        payload[4..8].copy_from_slice(&0.5f32.to_le_bytes()); // roll, rad

        let values = extract(set, packet, Payload::Binary(&payload)).unwrap();
        assert_eq!(value_of(&values, "time_boot_ms"), Value::U64(123_456));
        assert_eq!(value_of(&values, "roll"), Value::F64(0.5));
    }

    #[test]
    fn extracts_sat2_json_by_pointer() {
        let plan = plan();
        let (set, packet) = lookup(&plan, "vendor_b", "NAV_POS_ECEF");
        let doc = serde_json::json!({"sat": "PX-7", "ch": "NAV.POS.ECEF", "v": [1.5, -2, 3e6]});

        let values = extract(set, packet, Payload::Json(&doc)).unwrap();
        assert_eq!(value_of(&values, "x"), Value::F64(1.5));
        assert_eq!(value_of(&values, "y"), Value::F64(-2.0)); // a JSON integer read as float
        assert_eq!(value_of(&values, "z"), Value::F64(3e6));
    }

    #[test]
    fn longer_payloads_are_fine_shorter_ones_are_errors() {
        let plan = plan();
        let (set, packet) = lookup(&plan, "vendor_a", "EPS_HK");
        assert!(extract(set, packet, Payload::Binary(&[0u8; 20])).is_ok());
        assert_eq!(
            extract(set, packet, Payload::Binary(&[0u8; 9])),
            Err(InterpretError::ShortPayload {
                packet: "EPS_HK".into(),
                need: 15,
                got: 9
            })
        );
    }

    #[test]
    fn bad_json_is_an_error_naming_the_field() {
        let plan = plan();
        let (set, packet) = lookup(&plan, "vendor_b", "PWR_BUS_V");
        let missing = serde_json::json!({"sat": "PX-7", "ch": "PWR.BUS.V"});
        let error = extract(set, packet, Payload::Json(&missing)).unwrap_err();
        assert_eq!(error.to_string(), "field v: no value at /v");

        let wrong = serde_json::json!({"sat": "PX-7", "ch": "PWR.BUS.V", "v": "high"});
        let error = extract(set, packet, Payload::Json(&wrong)).unwrap_err();
        assert_eq!(error.to_string(), "field v: expected Int, got \"high\"");

        let error = extract(set, packet, Payload::Binary(&[0u8; 4])).unwrap_err();
        assert_eq!(
            error.to_string(),
            "packet PWR_BUS_V is defined for a json payload"
        );
    }

    // ---- convert: one test per conversion kind ----

    #[test]
    fn linear() {
        let plan = plan();
        let (_, packet) = lookup(&plan, "vendor_a", "EPS_HK");
        let temp = field(packet, "batt_temp_counts"); // ×0.1 − 273.15
        assert_eq!(
            convert(temp, &Value::U64(2982)).unwrap(),
            Converted::Value(Value::F64(2982.0 * 0.1 - 273.15))
        );
    }

    #[test]
    fn poly() {
        let mut f = field(
            lookup(&plan(), "vendor_a", "EPS_HK").1,
            "bus_voltage_counts",
        )
        .clone();
        f.conversion = Some(Conversion::Poly {
            coefficients: vec![1.0, 2.0, 3.0], // 1 + 2x + 3x²
        });
        assert_eq!(
            convert(&f, &Value::I64(-2)).unwrap(),
            Converted::Value(Value::F64(9.0))
        );
    }

    #[test]
    fn states_known_and_unknown() {
        let plan = plan();
        let (_, packet) = lookup(&plan, "vendor_a", "EPS_HK");
        let mode = field(packet, "mode");
        assert_eq!(
            convert(mode, &Value::U64(2)).unwrap(),
            Converted::Value(Value::Str("PAYLOAD".into()))
        );
        assert_eq!(
            convert(mode, &Value::U64(3)).unwrap(),
            Converted::UnknownState(3)
        );
    }

    #[test]
    fn no_conversion_passes_the_raw_value_through() {
        let plan = plan();
        let (_, packet) = lookup(&plan, "vendor_b", "SYS_MODE");
        let mode = field(packet, "v");
        let raw = Value::Str("SAFE".into());
        assert_eq!(convert(mode, &raw).unwrap(), Converted::Value(raw));
    }

    #[test]
    fn sentinels_win_over_conversion() {
        let plan = plan();
        let (_, packet) = lookup(&plan, "vendor_c", "SYS_STATUS");
        let volts = field(packet, "voltage_battery"); // invalid = [65535]
        let amps = field(packet, "current_battery"); // invalid = [-1]
        assert_eq!(
            convert(volts, &Value::U64(65535)).unwrap(),
            Converted::Sentinel
        );
        assert_eq!(convert(amps, &Value::I64(-1)).unwrap(), Converted::Sentinel);
        assert_eq!(
            convert(volts, &Value::U64(12600)).unwrap(),
            Converted::Value(Value::F64(12600.0 * 0.001))
        );
    }
}
