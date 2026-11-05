//! The MDL IR: the contract between frontends and the compiler.
//!
//! Frontends (TOML today; XTCE, C headers, a database export, in any
//! language) produce it; `validate` checks it and turns it into a `Plan`.
//! It knows nothing about source formats or envelopes (CCSDS, MAVLink):
//! links unwrap those and hand over metadata plus a payload.
//!
//! Its JSON form is a public contract, so it's designed on purpose: every
//! enum with data is tagged with a `kind` field (`{"kind": "bits", ...}`),
//! names are snake_case, and `mdl_version` says which version of this
//! format a document uses. Evolve it like a protobuf schema (DDIA Ch. 5):
//! add optional fields, never repurpose a name.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The version of the IR format this compiler reads and writes.
pub const MDL_VERSION: u32 = 1;

// ---- The compiled definitions -------------------------------------------------

/// Everything the compiler works on: the catalog and every definition set,
/// keyed by name. `BTreeMap` keeps the keys sorted, so the plan's JSON (and
/// its hash) is the same on every build.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Definitions {
    /// Which version of the IR format this is. `validate` rejects others.
    pub mdl_version: u32,
    pub catalog: BTreeMap<String, CanonicalParam>,
    pub sets: BTreeMap<String, DefinitionSet>,
}

/// One entry in the catalog: a canonical parameter, the name every source
/// maps its own fields to (`[[canonical]]` in TOML). Vendors never see these;
/// whoever writes a definition set picks them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanonicalParam {
    pub name: String,
    pub ty: ParamType,
    pub unit: String,
    /// Enum params: the allowed states. Empty otherwise
    pub states: Vec<String>,
    /// Vec3 and quat params; the frame consumers receive
    pub frame: Option<Frame>,
    pub limits: Option<Limits>,
    pub description: String,
}

/// One vendor's definitions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DefinitionSet {
    pub name: String,
    pub encoding: Encoding,
    /// `Some` for binary payloads, `None` for json.
    pub endian: Option<Endian>,
    pub time: Time,
    pub alias: Alias,
    /// Vendor-native id to canonical asset id.
    pub aliases: BTreeMap<String, String>,
    pub packets: Vec<Packet>,
}

// ---- Vocabulary shared with the frontends ------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    F64,
    I64,
    Bool,
    Str,
    Enum,
    Vec3,
    Quat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Frame {
    Ecef,
    EciJ2000,
    Body,
    Ned,
    /// Latitude, longitude, altitude. Source-side only: consumers convert it.
    Geodetic,
}

/// Default limits, evaluated by consumers. Any of them may be omitted.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub red_low: Option<f64>,
    pub yellow_low: Option<f64>,
    pub yellow_high: Option<f64>,
    pub red_high: Option<f64>,
}

/// How a set's payload is written, once its link has removed the envelope.
/// Envelopes (CCSDS, MAVLink, ground-station wrappers) belong to links, not
/// definitions: the link's stack strips them and passes on named metadata
/// (`apid`, `msgid`, `sysid`...) that definitions may refer to by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Encoding {
    /// Fields at bit offsets from the start of the payload.
    Binary,
    /// One JSON document; fields at JSON Pointers.
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Endian {
    Big,
    Little,
}

/// One identification value: an integer (an APID, a msgid, a frame type)
/// or a string (a JSON channel name). `untagged`: serde tries each variant
/// in order and keeps the first that fits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IdentValue {
    Int(i64),
    Str(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    Uint,
    Int,
    Float,
    Bool,
    String,
}

// ---- Time and Alias ------------------------------------

/// How event time is found. Unlike the TOML's `Option`, "no time" is an
/// explicit variant: packets are stamped with ground time only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Time {
    None,
    Cuc {
        offset_bits: u32,
        coarse_bytes: u8,
        fine_bytes: u8,
        /// The epoch, already parsed: nanoseconds since the Unix epoch.
        epoch_unix_ns: i64,
    },
    Iso8601 {
        path: String,
    },
    BootMs {
        correlate: Correlate,
    },
}

/// The packet that carries both clocks, and its two fields (checked to exist).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Correlate {
    pub packet: String,
    pub unix_us: String,
    pub boot_ms: String,
}

/// Where the vendor-native asset id is. The TOML's loose `Locator` becomes
/// exactly one of these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Alias {
    /// Single-asset source: every packet is this asset.
    Fixed { asset: String },
    /// Bits in the payload (CCSDS spacecraft id).
    Bits(BitRange),
    /// Link metadata (MAVLink `sysid`); checked against the link by the consumer.
    Metadata { name: String },
    /// A JSON Pointer (`/sat`).
    Path { pointer: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BitRange {
    pub offset_bits: u32,
    pub size_bits: u32,
}

// ---- Packet, Fields, Conversions, Composites ------------------------------------

/// One packet layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Packet {
    pub name: String,
    pub identify: BTreeMap<String, IdentValue>,
    /// Payload size in bytes (binary encoding).
    pub length_bytes: Option<u32>,
    /// `boot_ms` time: the field holding milliseconds since boot.
    pub time_field: Option<String>,
    pub fields: Vec<Field>,
    pub composites: Vec<Composite>,
}

/// One field: where it is, what it is, and what it means.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    pub location: Location,
    pub ty: FieldType,
    pub conversion: Option<Conversion>,
    pub unit: Option<String>,
    pub canonical: Option<String>,
    pub invalid: Vec<i64>,
}

/// Bits for binary payloads, a JSON Pointer for json. Never both, never
/// neither: the type makes that impossible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Location {
    Bits(BitRange),
    Path { pointer: String },
}

/// Raw to engineering value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Conversion {
    Linear { scale: f64, offset: f64 },
    Poly { coefficients: Vec<f64> },
    States { states: Vec<State> },
}

/// One enum state: a raw integer and its name. A list of these rather than
/// a map, because JSON object keys can only be strings: as a list, the raw
/// values stay real numbers for every language that reads the IR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub raw: i64,
    pub name: String,
}

/// Several fields that become one vec3 or quat sample.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Composite {
    pub canonical: String,
    pub frame: Frame,
    pub fields: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny() -> Definitions {
        let mode = Field {
            name: "mode".into(),
            location: Location::Bits(BitRange {
                offset_bits: 118,
                size_bits: 2,
            }),
            ty: FieldType::Uint,
            conversion: Some(Conversion::States {
                states: vec![
                    State {
                        raw: 0,
                        name: "SAFE".into(),
                    },
                    State {
                        raw: 1,
                        name: "NOMINAL".into(),
                    },
                ],
            }),
            unit: None,
            canonical: Some("eps.mode".into()),
            invalid: vec![],
        };
        let set = DefinitionSet {
            name: "vendor_a".into(),
            encoding: Encoding::Binary,
            endian: Some(Endian::Big),
            time: Time::None,
            alias: Alias::Fixed {
                asset: "SAT-1".into(),
            },
            aliases: BTreeMap::new(),
            packets: vec![Packet {
                name: "EPS_HK".into(),
                identify: BTreeMap::from([("apid".to_string(), IdentValue::Int(0x064))]),
                length_bytes: Some(15),
                time_field: None,
                fields: vec![mode],
                composites: vec![],
            }],
        };
        Definitions {
            mdl_version: MDL_VERSION,
            catalog: BTreeMap::new(),
            sets: BTreeMap::from([("vendor_a".to_string(), set)]),
        }
    }

    #[test]
    fn definitions_round_trip_through_json() {
        // The plan in Step 4 is exactly this JSON, so it must come back intact.
        let json = serde_json::to_string(&tiny()).unwrap();
        let back: Definitions = serde_json::from_str(&json).unwrap();
        assert_eq!(back, tiny());
    }

    #[test]
    fn enums_serialize_with_a_kind_tag() {
        // The JSON shape other languages read and write.
        let json = serde_json::to_value(tiny()).unwrap();
        let field = &json["sets"]["vendor_a"]["packets"][0]["fields"][0];
        assert_eq!(
            field["location"],
            serde_json::json!({ "kind": "bits", "offset_bits": 118, "size_bits": 2 })
        );
        assert_eq!(field["conversion"]["kind"], "states");
        assert_eq!(
            field["conversion"]["states"][1],
            serde_json::json!({ "raw": 1, "name": "NOMINAL" })
        );
        assert_eq!(
            json["sets"]["vendor_a"]["alias"],
            serde_json::json!({ "kind": "fixed", "asset": "SAT-1" })
        );
        assert_eq!(json["sets"]["vendor_a"]["encoding"], "binary");
        assert_eq!(json["mdl_version"], 1);
    }

    #[test]
    fn the_same_definitions_serialize_to_the_same_bytes() {
        assert_eq!(
            serde_json::to_string(&tiny()).unwrap(),
            serde_json::to_string(&tiny()).unwrap()
        );
    }
}
