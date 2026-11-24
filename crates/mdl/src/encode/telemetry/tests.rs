use serde_json::{Value as Json, json};

use super::*;
use crate::artifact::{Artifacts, Written, build};
use crate::ir::Artifact;
use crate::read::{Loaded, read_artifact, read_bundle};
use crate::validate::validate;

/// A binary family whose alias is the payload's first byte, and a JSON
/// family whose alias is at `/sat`.
fn bundle() -> Json {
    json!({
        "mdl_version": "2.4",
        "catalog": { "params": {
            "eps.volts": { "type": "f64", "unit": "V" },
            "eps.mode": { "type": "enum", "states": ["SAFE", "NOMINAL"] },
            "eps.temp": { "type": "f64", "unit": "Cel" },
            "eps.heater": { "type": "bool" },
            "adcs.mag": { "type": "vec3", "unit": "nT", "frame": "BODY" },
            "probe.level": { "type": "f64" },
            "x.p": { "type": "u64" },
            "probe.vec": { "type": "vec3", "frame": "BODY" }
        } },
        "families": [
            { "name": "sat", "encoding": "binary", "byte_order": "big",
              "alias": { "kind": "bits", "offset_bits": 0, "size_bits": 8 },
              "metadata": ["apid"] },
            { "name": "probe", "encoding": "json",
              "alias": { "kind": "path", "pointer": "/sat" } }
        ],
        "sets": [
            { "name": "sat_v1", "family": "sat",
              "types": {
                "Pad": { "entries": [ { "kind": "padding", "size_bits": 8 } ] },
                "Outer": { "entries": [
                    { "kind": "array", "name": "inner",
                      "count": { "kind": "fixed", "count": 1 },
                      "element": { "kind": "struct", "name": "Pad" } }
                ] },
                "Val": { "entries": [
                    { "kind": "field", "name": "v", "type": "uint", "size_bits": 8 }
                ] },
                "Spare": { "entries": [
                    { "kind": "padding", "size_bits": 8 },
                    { "kind": "array", "name": "vals",
                      "count": { "kind": "fixed", "count": 0 },
                      "element": { "kind": "struct", "name": "Val" } }
                ] }
              },
              "packets": [
                { "name": "HK", "identify": { "msg": 1, "apid": 100 },
                  "entries": [
                    { "kind": "field", "name": "id", "type": "uint", "size_bits": 8 },
                    { "kind": "field", "name": "msg", "type": "uint", "size_bits": 8 },
                    { "kind": "field", "name": "volts", "type": "uint", "size_bits": 16,
                      "conversion": { "kind": "linear", "scale": 0.001, "offset": 0 },
                      "unit": "V", "canonical": "eps.volts", "invalid": [65535] },
                    { "kind": "field", "name": "mode", "type": "uint", "size_bits": 8,
                      "conversion": { "kind": "states", "states": [
                          { "raw": 0, "name": "SAFE" }, { "raw": 1, "name": "NOMINAL" }] },
                      "canonical": "eps.mode" },
                    { "kind": "field", "name": "temp", "type": "int", "size_bits": 16,
                      "conversion": { "kind": "poly", "coefficients": [0, 0.01, 0.000001] },
                      "inverse": { "kind": "linear", "scale": 100, "offset": 0 },
                      "unit": "Cel", "canonical": "eps.temp" },
                    { "kind": "field", "name": "n", "type": "uint", "size_bits": 8 },
                    { "kind": "array", "name": "samples",
                      "count": { "kind": "field", "field": "n" },
                      "element": { "kind": "scalar", "type": "uint", "size_bits": 8 } },
                    { "kind": "checksum", "name": "crc",
                      "algorithm": { "kind": "crc", "width": 16, "poly": 4129,
                                     "init": 65535, "reflect_in": false,
                                     "reflect_out": false, "xor_out": 0 } }
                  ] },
                { "name": "MAG", "identify": { "msg": 2 },
                  "composites": [
                    { "canonical": "adcs.mag", "frame": "BODY", "members": ["mx", "my", "mz"] }
                  ],
                  "entries": [
                    { "kind": "field", "name": "id", "type": "uint", "size_bits": 8 },
                    { "kind": "field", "name": "msg", "type": "uint", "size_bits": 8 },
                    { "kind": "field", "name": "mx", "type": "int", "size_bits": 16, "unit": "nT" },
                    { "kind": "field", "name": "my", "type": "int", "size_bits": 16, "unit": "nT" },
                    { "kind": "field", "name": "mz", "type": "int", "size_bits": 16, "unit": "nT" }
                  ] },
                { "name": "HEATER", "identify": { "msg": 3 },
                  "entries": [
                    { "kind": "field", "name": "id", "type": "uint", "size_bits": 8 },
                    { "kind": "field", "name": "msg", "type": "uint", "size_bits": 8 },
                    { "kind": "field", "name": "on", "type": "uint", "size_bits": 8,
                      "canonical": "eps.heater", "invalid": [1] }
                  ] },
                { "name": "GAP", "identify": { "msg": 5 },
                  "entries": [
                    { "kind": "field", "name": "msg", "type": "uint", "size_bits": 8,
                      "offset_bits": 8 }
                  ] },
                { "name": "WIDE", "identify": { "msg": 6 },
                  "entries": [
                    { "kind": "field", "name": "hdr", "type": "uint", "size_bits": 4,
                      "offset_bits": 4 },
                    { "kind": "field", "name": "msg", "type": "uint", "size_bits": 8 }
                  ] },
                { "name": "COND", "identify": { "msg": 7 },
                  "entries": [
                    { "kind": "field", "name": "id", "type": "uint", "size_bits": 8 },
                    { "kind": "field", "name": "msg", "type": "uint", "size_bits": 8 },
                    { "kind": "field", "name": "n", "type": "uint", "size_bits": 8 },
                    { "kind": "field", "name": "sel", "type": "uint", "size_bits": 8 },
                    { "kind": "array", "name": "arr",
                      "count": { "kind": "field", "field": "n" },
                      "element": { "kind": "scalar", "type": "uint", "size_bits": 8 },
                      "canonical": "x.p",
                      "when": { "kind": "compare", "field": "sel", "op": "eq", "value": 1 } }
                  ] },
                { "name": "PADS", "identify": { "msg": 8 },
                  "entries": [
                    { "kind": "field", "name": "id", "type": "uint", "size_bits": 8 },
                    { "kind": "field", "name": "msg", "type": "uint", "size_bits": 8 },
                    { "kind": "field", "name": "n", "type": "uint", "size_bits": 32 },
                    { "kind": "array", "name": "recs",
                      "count": { "kind": "field", "field": "n" },
                      "element": { "kind": "struct", "name": "Pad" } }
                  ] },
                { "name": "NEST", "identify": { "msg": 9 },
                  "entries": [
                    { "kind": "field", "name": "id", "type": "uint", "size_bits": 8 },
                    { "kind": "field", "name": "msg", "type": "uint", "size_bits": 8 },
                    { "kind": "field", "name": "n", "type": "uint", "size_bits": 32 },
                    { "kind": "array", "name": "outers",
                      "count": { "kind": "field", "field": "n" },
                      "element": { "kind": "struct", "name": "Outer" } }
                  ] },
                { "name": "EMPTY", "identify": { "msg": 10 },
                  "entries": [
                    { "kind": "field", "name": "id", "type": "uint", "size_bits": 8 },
                    { "kind": "field", "name": "msg", "type": "uint", "size_bits": 8 },
                    { "kind": "field", "name": "n", "type": "uint", "size_bits": 32 },
                    { "kind": "array", "name": "nones",
                      "count": { "kind": "field", "field": "n" },
                      "element": { "kind": "struct", "name": "Spare" } }
                  ] },
                { "name": "ANY",
                  "entries": [
                    { "kind": "field", "name": "id", "type": "uint", "size_bits": 8 },
                    { "kind": "field", "name": "tag", "type": "uint", "size_bits": 8 },
                    { "kind": "field", "name": "rest", "type": "bytes",
                      "length": { "kind": "fixed", "bytes": 6 } }
                  ] }
            ] },
            { "name": "probe_v1", "family": "probe",
              "types": {
                "Rec": { "entries": [
                    { "kind": "field", "name": "id", "type": "uint", "pointer": "/id" },
                    { "kind": "struct", "name": "opt", "struct": "Opt", "pointer": "/o",
                      "optional": true }
                ] },
                "Opt": { "entries": [
                    { "kind": "field", "name": "v", "type": "float", "pointer": "/v",
                      "canonical": "probe.level" }
                ] }
              },
              "packets": [
                { "name": "J", "identify": { "/kind": "hk" },
                  "entries": [
                    { "kind": "field", "name": "v", "type": "float", "pointer": "/v",
                      "unit": "V", "canonical": "eps.volts" },
                    { "kind": "field", "name": "note", "type": "string",
                      "pointer": "/note", "optional": true }
                  ] },
                { "name": "REC", "identify": { "/kind": "rec" },
                  "entries": [
                    { "kind": "array", "name": "rs", "pointer": "/r",
                      "element": { "kind": "struct", "name": "Rec" } }
                  ] },
                { "name": "ARR", "identify": { "/kind": "arr" },
                  "composites": [
                    { "canonical": "probe.vec", "frame": "BODY",
                      "members": ["levels[0]", "levels[1]", "levels[2]"] }
                  ],
                  "entries": [
                    { "kind": "array", "name": "levels", "pointer": "/l",
                      "element": { "kind": "scalar", "type": "float" },
                      "canonical": "probe.level" }
                  ] }
            ] }
        ]
    })
}

fn artifacts_of(bundle: &Json) -> Artifacts {
    let bundle = read_bundle(&serde_json::to_vec(bundle).unwrap())
        .unwrap()
        .value;
    build(&validate(bundle).unwrap()).unwrap()
}

fn load(written: &Written) -> Loaded<Artifact> {
    read_artifact(written.canonical.as_bytes()).unwrap()
}

fn sets_of(bundle: &Json, family: &str) -> FamilySets {
    let artifacts = artifacts_of(bundle);
    let sets: Vec<_> = artifacts
        .sets
        .values()
        .filter(|w| matches!(&w.artifact, Artifact::Set { set, .. } if set.family == family))
        .map(load)
        .collect();
    FamilySets::new(
        &load(&artifacts.families[family]),
        &sets,
        &[load(&artifacts.catalog)],
    )
    .unwrap()
}

fn hk() -> TelemetryRequest {
    TelemetryRequest::new("sat_v1", "HK", 7_u64)
        .with_param("eps.volts", SampleValue::F64(28.5))
        .with_param("eps.mode", SampleValue::Enum("NOMINAL".to_string()))
        .with_param("eps.temp", SampleValue::F64(21.5))
        .with_raw(
            "samples",
            RawValue::List(vec![RawValue::Integer(9), RawValue::Integer(8)]),
        )
}

fn code(result: Result<EncodedTelemetry<'_>, TelemetryError>) -> &'static str {
    match result {
        Ok(_) => "ok",
        Err(error) => error.code(),
    }
}

/// The alias, the identify key and its metadata, the count and the
/// checksum come from the definitions; params go through their
/// conversions, the IR 2.4 inverse among them. Worked out by hand: volts
/// 28.5 V is 28500 (0x6f54) at 0.001; temp 21.5 °C is 2150 (0x0866)
/// through the inverse; the CRC-16/CCITT-FALSE of the first 10 bytes,
/// 0x05fd, by a separate implementation checked against its check value
/// (0x29b1 for "123456789").
#[test]
fn a_packet_encodes_from_params_and_definitions() {
    let sets = sets_of(&bundle(), "sat");
    let encoded = sets.encode_telemetry(&hk()).unwrap();
    assert_eq!(
        encoded.payload,
        [
            0x07, 0x01, 0x6f, 0x54, 0x01, 0x08, 0x66, 0x02, 0x09, 0x08, 0x05, 0xfd
        ]
    );
    assert_eq!(
        encoded.metadata,
        [("apid".to_string(), MetadataValue::Int(100))]
    );
    assert_eq!(encoded.record.get("n"), Some(&Value::Uint(2)));
    assert_eq!(encoded.record.get("crc"), Some(&Value::Uint(0x05fd)));
}

#[test]
fn a_composite_fills_its_members() {
    let sets = sets_of(&bundle(), "sat");
    let mag = TelemetryRequest::new("sat_v1", "MAG", 7_u64).with_param(
        "adcs.mag",
        SampleValue::Vec3 {
            frame: Frame::Body,
            x: 100.0,
            y: -200.0,
            z: 300.0,
        },
    );
    let encoded = sets.encode_telemetry(&mag).unwrap();
    assert_eq!(
        encoded.payload,
        [0x07, 0x02, 0x00, 0x64, 0xff, 0x38, 0x01, 0x2c]
    );
    let wrong = TelemetryRequest::new("sat_v1", "MAG", 7_u64).with_param(
        "adcs.mag",
        SampleValue::Vec3 {
            frame: Frame::Ned,
            x: 1.0,
            y: 2.0,
            z: 3.0,
        },
    );
    assert_eq!(code(sets.encode_telemetry(&wrong)), "wrong_type");
    // The members can't be given raw as well.
    assert_eq!(
        code(sets.encode_telemetry(&mag.clone().with_raw("mx", RawValue::Integer(100)))),
        "duplicate_value"
    );
}

#[test]
fn values_the_definitions_fix_must_agree() {
    let sets = sets_of(&bundle(), "sat");
    // Given what they fix, they're accepted; given otherwise, refused.
    assert_eq!(
        code(sets.encode_telemetry(&hk().with_raw("msg", RawValue::Integer(1)))),
        "ok"
    );
    assert_eq!(
        code(sets.encode_telemetry(&hk().with_raw("msg", RawValue::Integer(2)))),
        "wrong_constant"
    );
    assert_eq!(
        code(sets.encode_telemetry(&hk().with_raw("n", RawValue::Integer(3)))),
        "wrong_constant"
    );
    assert_eq!(
        code(sets.encode_telemetry(&hk().with_raw("crc", RawValue::Integer(1)))),
        "wrong_constant"
    );
    assert_eq!(
        code(sets.encode_telemetry(&hk().with_raw("id", RawValue::Integer(8)))),
        "wrong_constant"
    );
}

#[test]
fn the_native_id_is_what_the_alias_can_read() {
    let sets = sets_of(&bundle(), "sat");
    let mut request = hk();
    request.native_id = NativeId::from(256_u64);
    assert_eq!(code(sets.encode_telemetry(&request)), "native_id");
    request.native_id = NativeId::from_text("box");
    assert_eq!(code(sets.encode_telemetry(&request)), "native_id");
    request.native_id = NativeId::from_text("");
    assert_eq!(code(sets.encode_telemetry(&request)), "native_id");
}

#[test]
fn every_value_is_given_once_and_where_the_packet_has_it() {
    let sets = sets_of(&bundle(), "sat");
    let mut missing = hk();
    missing.params.retain(|(name, _)| name != "eps.temp");
    assert_eq!(code(sets.encode_telemetry(&missing)), "missing_value");
    // An entry given a raw value takes nothing from its param, whose value
    // is then left over.
    let error = sets
        .encode_telemetry(&hk().with_raw("volts", RawValue::Integer(1)))
        .unwrap_err();
    assert_eq!(error.code(), "bad_count");
    assert_eq!(
        error.to_string(),
        "param eps.volts: it's given 1 value, and 0 entries take their value from it"
    );
    assert_eq!(
        code(sets.encode_telemetry(&hk().with_raw("nothing", RawValue::Integer(1)))),
        "unknown_entry"
    );
    assert_eq!(
        code(sets.encode_telemetry(&hk().with_param("adcs.mag", SampleValue::F64(1.0)))),
        "unknown_param"
    );
    assert_eq!(
        code(sets.encode_telemetry(&hk().with_param("eps.volts", SampleValue::F64(1.0)))),
        "duplicate_value"
    );
    let mut twice = hk();
    twice.params[0].1.push(SampleValue::F64(1.0));
    assert_eq!(code(sets.encode_telemetry(&twice)), "bad_count");
    assert_eq!(
        code(sets.encode_telemetry(&TelemetryRequest::new("sat_v1", "NONE", 7_u64))),
        "unknown_packet"
    );
    assert_eq!(
        code(sets.encode_telemetry(&TelemetryRequest::new("sat_v9", "HK", 7_u64))),
        "unknown_set"
    );
}

#[test]
fn a_param_value_is_one_its_field_can_hold() {
    let sets = sets_of(&bundle(), "sat");
    let with = |name: &str, value: SampleValue| {
        let mut request = hk();
        request.params.retain(|(param, _)| param != name);
        request.with_param(name, value)
    };
    assert_eq!(
        code(sets.encode_telemetry(&with("eps.volts", SampleValue::Str("x".to_string())))),
        "wrong_type"
    );
    assert_eq!(
        code(sets.encode_telemetry(&with("eps.volts", SampleValue::F64(f64::NAN)))),
        "non_finite"
    );
    assert_eq!(
        code(sets.encode_telemetry(&with("eps.volts", SampleValue::F64(70.0)))),
        "raw_range"
    );
    // 65.535 V is raw 65535, which the field lists as no reading.
    assert_eq!(
        code(sets.encode_telemetry(&with("eps.volts", SampleValue::F64(65.535)))),
        "invalid_raw"
    );
    assert_eq!(
        code(sets.encode_telemetry(&with("eps.mode", SampleValue::Enum("PAYLOAD".to_string())))),
        "unknown_state"
    );
    // A bool's true is raw 1, which the field lists as no reading.
    let heater = TelemetryRequest::new("sat_v1", "HEATER", 7_u64)
        .with_param("eps.heater", SampleValue::Bool(true));
    assert_eq!(code(sets.encode_telemetry(&heater)), "invalid_raw");
    let off = TelemetryRequest::new("sat_v1", "HEATER", 7_u64)
        .with_param("eps.heater", SampleValue::Bool(false));
    assert_eq!(code(sets.encode_telemetry(&off)), "ok");
}

/// Without the IR 2.4 inverse, a polynomial's field takes its raw value.
#[test]
fn a_conversion_without_an_inverse_takes_a_raw_value() {
    let mut bundle = bundle();
    let temp = &mut bundle["sets"][0]["packets"][0]["entries"][4];
    temp.as_object_mut().unwrap().remove("inverse");
    bundle["mdl_version"] = json!("2.0");
    let sets = sets_of(&bundle, "sat");
    assert_eq!(code(sets.encode_telemetry(&hk())), "no_inverse");
    let mut raw = hk();
    raw.params.retain(|(name, _)| name != "eps.temp");
    assert_eq!(
        code(sets.encode_telemetry(&raw.with_raw("temp", RawValue::Integer(2150)))),
        "ok"
    );
}

/// The catch-all packet's values can match another packet's identify
/// keys, which decoding then picks: refused, not sent as something else.
#[test]
fn a_payload_that_decodes_as_another_packet_is_refused() {
    let sets = sets_of(&bundle(), "sat");
    let any = |tag: i128| {
        TelemetryRequest::new("sat_v1", "ANY", 7_u64)
            .with_raw("tag", RawValue::Integer(tag))
            .with_raw("rest", RawValue::Bytes(vec![0; 6]))
    };
    // No packet is identified by msg 42.
    assert_eq!(code(sets.encode_telemetry(&any(42))), "ok");
    // HK is identified by apid too, which ANY's payload isn't framed with.
    assert_eq!(code(sets.encode_telemetry(&any(1))), "ok");
    // MAG is identified by msg 2 alone, and ANY's 8 bytes decode as it.
    let error = sets.encode_telemetry(&any(2)).unwrap_err();
    assert_eq!(error.code(), "decodes_otherwise");
    assert!(
        error.to_string().contains("identifies packet MAG"),
        "{error}"
    );
}

#[test]
fn a_json_packet_writes_its_alias_and_identify_key() {
    let sets = sets_of(&bundle(), "probe");
    let request = TelemetryRequest::new("probe_v1", "J", NativeId::from_text("P1"))
        .with_param("eps.volts", SampleValue::F64(-0.0));
    let encoded = sets.encode_telemetry(&request).unwrap();
    assert_eq!(
        String::from_utf8(encoded.payload).unwrap(),
        r#"{"kind":"hk","sat":"P1","v":0}"#
    );
    // An optional entry given a value is in the document.
    let noted = request.with_raw("note", RawValue::Text("hi".to_string()));
    let encoded = sets.encode_telemetry(&noted).unwrap();
    assert_eq!(
        String::from_utf8(encoded.payload).unwrap(),
        r#"{"kind":"hk","note":"hi","sat":"P1","v":0}"#
    );
}

/// A composite given fills its members, here an array's first elements,
/// and the array's own param fills the rest: four elements.
#[test]
fn a_composite_and_a_param_fill_one_array() {
    let sets = sets_of(&bundle(), "probe");
    let request = TelemetryRequest::new("probe_v1", "ARR", NativeId::from_text("P1"))
        .with_param(
            "probe.vec",
            SampleValue::Vec3 {
                frame: Frame::Body,
                x: 1.0,
                y: 2.0,
                z: 3.0,
            },
        )
        .with_param("probe.level", SampleValue::F64(4.0));
    let encoded = sets.encode_telemetry(&request).unwrap();
    assert_eq!(
        String::from_utf8(encoded.payload).unwrap(),
        r#"{"kind":"arr","l":[1,2,3,4],"sat":"P1"}"#
    );
}

/// Inside an array's records, a param's values don't say which records
/// hold an optional struct, so a raw object, empty here, says the second
/// does; the param's value fills it.
#[test]
fn an_optional_struct_in_a_record_is_there_when_given_raw() {
    let sets = sets_of(&bundle(), "probe");
    let record = |id: i128, opt: bool| {
        let mut entries = vec![("id".to_string(), RawValue::Integer(id))];
        if opt {
            entries.push(("opt".to_string(), RawValue::Record(Vec::new())));
        }
        RawValue::Record(entries)
    };
    let request = TelemetryRequest::new("probe_v1", "REC", NativeId::from_text("P1"))
        .with_raw(
            "rs",
            RawValue::List(vec![record(1, false), record(2, true)]),
        )
        .with_param("probe.level", SampleValue::F64(5.0));
    let encoded = sets.encode_telemetry(&request).unwrap();
    assert_eq!(
        String::from_utf8(encoded.payload).unwrap(),
        r#"{"kind":"rec","r":[{"id":1},{"id":2,"o":{"v":5}}],"sat":"P1"}"#
    );
}

/// The alias's bits are written where no entry is, and over an entry
/// that holds them differently, which decoding then reads otherwise.
#[test]
fn the_alias_bits_are_written_and_checked() {
    let sets = sets_of(&bundle(), "sat");
    let gap = TelemetryRequest::new("sat_v1", "GAP", 7_u64);
    let encoded = sets.encode_telemetry(&gap).unwrap();
    assert_eq!(encoded.payload, [0x07, 0x05]);
    assert_eq!(encoded.metadata, []);
    // hdr is the low 4 bits of the alias's byte.
    let wide = |hdr: i128| {
        TelemetryRequest::new("sat_v1", "WIDE", 7_u64).with_raw("hdr", RawValue::Integer(hdr))
    };
    assert_eq!(
        sets.encode_telemetry(&wide(7)).unwrap().payload,
        [0x07, 0x06]
    );
    let error = sets.encode_telemetry(&wide(1)).unwrap_err();
    assert_eq!(error.code(), "decodes_otherwise");
    assert!(
        error.to_string().contains("gives hdr another value"),
        "{error}"
    );
}

/// A counted array its count field works out ahead gives its params'
/// values back when its condition then doesn't hold: they're left over.
#[test]
fn a_counted_array_that_isnt_there_leaves_its_values_over() {
    let sets = sets_of(&bundle(), "sat");
    let cond = |sel: i128| {
        TelemetryRequest::new("sat_v1", "COND", 7_u64)
            .with_params("x.p", vec![SampleValue::U64(10), SampleValue::U64(20)])
            .with_raw("sel", RawValue::Integer(sel))
    };
    assert_eq!(
        sets.encode_telemetry(&cond(1)).unwrap().payload,
        [0x07, 0x07, 0x02, 0x01, 0x0a, 0x14]
    );
    let error = sets.encode_telemetry(&cond(0)).unwrap_err();
    assert_eq!(error.code(), "bad_count");
    assert_eq!(
        error.to_string(),
        "param x.p: it's given 2 values, and 0 entries take their value from it"
    );
    // A raw list for it is a value given where the message has no entry.
    let raw = TelemetryRequest::new("sat_v1", "COND", 7_u64)
        .with_raw("arr", RawValue::List(vec![RawValue::Integer(1)]))
        .with_raw("sel", RawValue::Integer(0));
    assert_eq!(code(sets.encode_telemetry(&raw)), "absent_entry");
}

/// Records of padding alone take no values, so a count's value alone
/// doesn't make them, however large: a raw list of empty records does.
#[test]
fn records_of_padding_come_from_a_raw_list() {
    let sets = sets_of(&bundle(), "sat");
    let counted = TelemetryRequest::new("sat_v1", "PADS", 7_u64)
        .with_raw("n", RawValue::Integer(4_294_967_295));
    assert_eq!(code(sets.encode_telemetry(&counted)), "missing_value");
    let listed = TelemetryRequest::new("sat_v1", "PADS", 7_u64).with_raw(
        "recs",
        RawValue::List(vec![RawValue::Record(Vec::new()); 3]),
    );
    assert_eq!(
        sets.encode_telemetry(&listed).unwrap().payload,
        [0x07, 0x08, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00]
    );
    // Records holding a fixed count of such records take none either.
    let nested = TelemetryRequest::new("sat_v1", "NEST", 7_u64)
        .with_raw("n", RawValue::Integer(4_294_967_295));
    assert_eq!(code(sets.encode_telemetry(&nested)), "missing_value");
    let listed = TelemetryRequest::new("sat_v1", "NEST", 7_u64).with_raw(
        "outers",
        RawValue::List(vec![RawValue::Record(Vec::new()); 2]),
    );
    assert_eq!(
        sets.encode_telemetry(&listed).unwrap().payload,
        [0x07, 0x09, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00]
    );
    // So do records whose array has no elements, whatever they'd hold.
    let spare = TelemetryRequest::new("sat_v1", "EMPTY", 7_u64)
        .with_raw("n", RawValue::Integer(4_294_967_295));
    assert_eq!(code(sets.encode_telemetry(&spare)), "missing_value");
}

/// A linear conversion of scale 0 gives every raw value the offset, so it
/// has no inverse.
#[test]
fn a_linear_conversion_of_scale_0_has_no_inverse() {
    let mut bundle = bundle();
    bundle["sets"][0]["packets"][0]["entries"][2]["conversion"]["scale"] = json!(0);
    let sets = sets_of(&bundle, "sat");
    assert_eq!(code(sets.encode_telemetry(&hk())), "no_inverse");
}
