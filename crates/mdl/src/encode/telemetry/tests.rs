use serde_json::{Value as Json, json};

use super::*;
use crate::artifact::{Artifacts, Written, build};
use crate::ir::{Artifact, TimeUnit};
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

// ---- Event time -----------------------------------------------------------------

/// A family for each time source, the alias the payload's first byte (or
/// `/sat`), its set's packets told apart by `tag` (or `/kind`). The CUC
/// family is little-endian, so a field holds the code's coarse bytes only
/// when it's read big-endian, as the code is, and its one fine byte reads
/// alike either way.
fn clocks_bundle() -> Json {
    let binary = |name: &str| {
        json!({ "name": name, "encoding": "binary", "byte_order": "big",
                "alias": { "kind": "bits", "offset_bits": 0, "size_bits": 8 } })
    };
    let mut little = binary("cucsat");
    little["byte_order"] = json!("little");
    let field = |name: &str, ty: &str, size_bits: u32| json!({ "kind": "field", "name": name, "type": ty, "size_bits": size_bits });
    let id = field("id", "uint", 8);
    let tag = field("tag", "uint", 8);
    let with_invalid = |mut field: Json, invalid: Json| {
        field["invalid"] = invalid;
        field
    };
    let mut big_coarse = field("coarse", "uint", 32);
    big_coarse["byte_order"] = json!("big");
    let mut uptime = field("up", "uint", 32);
    uptime["unit"] = json!("ms");
    uptime["canonical"] = json!("sys.uptime");
    json!({
        "mdl_version": "2.4",
        "catalog": { "params": { "sys.uptime": { "type": "f64", "unit": "ms" } } },
        "families": [
            little, binary("elsat"), binary("gpssat"), binary("bootsat"), binary("secsat"),
            { "name": "isoprobe", "encoding": "json",
              "alias": { "kind": "path", "pointer": "/sat" } }
        ],
        "sets": [
            { "name": "cuc_v1", "family": "cucsat",
              "time": { "kind": "cuc", "offset_bits": 8, "coarse_bytes": 4, "fine_bytes": 1,
                        "epoch": "1958-01-01T00:00:00Z", "scale": "tai" },
              "packets": [
                { "name": "T", "identify": { "tag": 1 },
                  "entries": [id, big_coarse, field("fine", "uint", 8), tag] },
                { "name": "LITTLE", "identify": { "tag": 3 },
                  "entries": [id, field("coarse", "uint", 32), field("fine", "uint", 8), tag] },
                { "name": "BARE", "identify": { "tag": 2 },
                  "entries": [id, { "kind": "padding", "size_bits": 40 }, tag] }
              ] },
            { "name": "el_v1", "family": "elsat",
              "time": { "kind": "elapsed", "unit": "ns",
                        "epoch": "1958-01-01T00:00:00Z", "scale": "tai" },
              "packets": [
                { "name": "EI", "identify": { "tag": 1 }, "time_field": "t",
                  "entries": [id, tag, field("t", "uint", 64)] },
                { "name": "EF", "identify": { "tag": 2 }, "time_field": "t",
                  "entries": [id, tag, field("t", "float", 64)] },
                { "name": "ES", "identify": { "tag": 3 }, "time_field": "t",
                  "entries": [id, tag, field("t", "float", 32)] },
                { "name": "ESENT", "identify": { "tag": 4 }, "time_field": "t",
                  "entries": [id, tag, with_invalid(field("t", "uint", 64),
                                                    json!([86_410_000_000_000_u64]))] },
                { "name": "NONE", "identify": { "tag": 5 }, "entries": [id, tag] }
              ] },
            { "name": "sec_v1", "family": "secsat",
              "time": { "kind": "elapsed", "unit": "s",
                        "epoch": "2000-01-01T00:00:00Z", "scale": "utc" },
              "packets": [
                { "name": "S", "time_field": "t", "entries": [id, field("t", "int", 32)] }
              ] },
            { "name": "gps_v1", "family": "gpssat",
              "time": { "kind": "gps", "unit": "ms", "week_field": "wk", "week_bits": 10 },
              "packets": [
                { "name": "G", "time_field": "tow",
                  "entries": [id, field("wk", "uint", 16), field("tow", "uint", 32)] }
              ] },
            { "name": "boot_v1", "family": "bootsat",
              "time": { "kind": "boot", "unit": "ms",
                        "correlate": { "packet": "CORR", "clock_field": "clk", "clock_unit": "s",
                                       "boot_field": "up", "epoch": "1970-01-01T00:00:00Z",
                                       "scale": "utc" } },
              "packets": [
                { "name": "HK", "identify": { "tag": 1 }, "time_field": "up",
                  "entries": [id, tag, uptime] },
                { "name": "CORR", "identify": { "tag": 2 }, "time_field": "up",
                  "entries": [id, tag, field("clk", "uint", 32), field("up", "uint", 32)] }
              ] },
            { "name": "iso_v1", "family": "isoprobe",
              "time": { "kind": "iso8601", "pointer": "/t" },
              "packets": [
                { "name": "WITH", "identify": { "/kind": "a" },
                  "entries": [
                    { "kind": "field", "name": "t", "type": "string", "pointer": "/t" },
                    { "kind": "field", "name": "v", "type": "float", "pointer": "/v" }
                  ] },
                { "name": "BARE", "identify": { "/kind": "b" },
                  "entries": [
                    { "kind": "field", "name": "v", "type": "float", "pointer": "/v" }
                  ] }
              ] }
        ]
    })
}

fn instant(text: &str) -> Timestamp {
    text.parse().unwrap()
}

/// 2026-06-01T12:00:00.123456789Z: Unix second 1,780,315,200, when TAI
/// is 37 s ahead of UTC and GPS 18 s.
fn noon() -> Timestamp {
    instant("2026-06-01T12:00:00.123456789Z")
}

fn at(time: Timestamp) -> EventTime {
    EventTime::At(time)
}

/// The expected readings below are worked out apart from MDL, from the
/// calendar and the leap-second table. On TAI from 1958, noon is
/// 2,159,006,437 s (0x80afd2e5) and 123,456,789 ns, 1958 being 4,383 days
/// before 1970. An 8-bit fraction can be at most 31 (0x1f), since
/// `(123456789 + 1) * 256 - 1` over 10^9 rounds down to it, and it reads
/// back as 121,093,750 ns. Its ticks are 3,906,250 ns apart.
#[test]
fn a_cuc_time_code_is_written_from_the_event_time() {
    let sets = sets_of(&clocks_bundle(), "cucsat");
    let request = TelemetryRequest::new("cuc_v1", "T", 7_u64).with_at(noon());
    let encoded = sets.encode_telemetry(&request).unwrap();
    assert_eq!(encoded.payload, [0x07, 0x80, 0xaf, 0xd2, 0xe5, 0x1f, 0x01]);
    assert_eq!(
        encoded.record.get("coarse"),
        Some(&Value::Uint(0x80af_d2e5))
    );
    assert_eq!(encoded.record.get("fine"), Some(&Value::Uint(0x1f)));
    assert_eq!(
        encoded.event_time,
        at(instant("2026-06-01T12:00:00.12109375Z"))
    );
    // Where no field holds it, it's written over the padding, big-endian
    // in a little-endian family.
    let bare = TelemetryRequest::new("cuc_v1", "BARE", 7_u64).with_at(noon());
    assert_eq!(
        sets.encode_telemetry(&bare).unwrap().payload,
        [0x07, 0x80, 0xaf, 0xd2, 0xe5, 0x1f, 0x02]
    );
    // A field read little-endian doesn't hold the coarse bytes as the code
    // reads them, so it takes a value like any field.
    let little = TelemetryRequest::new("cuc_v1", "LITTLE", 7_u64).with_at(noon());
    assert_eq!(code(sets.encode_telemetry(&little)), "missing_value");
    // A nanosecond before a tick reads as the tick before; on it, it reads.
    for (time, fine, back) in [
        ("2026-06-01T12:00:00.003906249Z", 0, "2026-06-01T12:00:00Z"),
        (
            "2026-06-01T12:00:00.00390625Z",
            1,
            "2026-06-01T12:00:00.00390625Z",
        ),
    ] {
        let request = TelemetryRequest::new("cuc_v1", "T", 7_u64).with_at(instant(time));
        let encoded = sets.encode_telemetry(&request).unwrap();
        assert_eq!(
            encoded.record.get("fine"),
            Some(&Value::Uint(fine)),
            "{time}"
        );
        assert_eq!(encoded.event_time, at(instant(back)));
    }
    // Without the event time nothing can give it.
    let timeless = TelemetryRequest::new("cuc_v1", "BARE", 7_u64);
    let error = sets.encode_telemetry(&timeless).unwrap_err();
    assert_eq!(error.code(), "missing_value");
    assert!(error.to_string().contains("packet BARE, time:"), "{error}");
    // A field that holds part of it may be given that part, and nothing else.
    let agreeing = request
        .clone()
        .with_raw("coarse", RawValue::Integer(0x80af_d2e5));
    assert_eq!(code(sets.encode_telemetry(&agreeing)), "ok");
    let disagreeing = request.with_raw("fine", RawValue::Integer(0));
    assert_eq!(code(sets.encode_telemetry(&disagreeing)), "wrong_constant");
    // Before the epoch, or 2^32 s after it, the code can't hold it.
    let early =
        TelemetryRequest::new("cuc_v1", "BARE", 7_u64).with_at(instant("1957-12-31T23:59:00Z"));
    assert_eq!(code(sets.encode_telemetry(&early)), "raw_range");
    let late =
        TelemetryRequest::new("cuc_v1", "BARE", 7_u64).with_at(instant("2094-02-07T06:28:00Z"));
    assert_eq!(code(sets.encode_telemetry(&late)), "raw_range");
}

/// In nanoseconds on TAI from 1958, noon is 2,159,006,437,123,456,789: a
/// 64-bit integer holds it exactly. Binary64 values there are 256 apart,
/// so the latest at or before it is 21 ns earlier; binary32 values are
/// 2^37 apart, so the latest is 44,703,317,781 ns earlier.
#[test]
fn an_elapsed_count_is_its_latest_reading_at_or_before_the_event_time() {
    let sets = sets_of(&clocks_bundle(), "elsat");
    let encode = |packet: &str| {
        sets.encode_telemetry(&TelemetryRequest::new("el_v1", packet, 7_u64).with_at(noon()))
    };
    let exact = encode("EI").unwrap();
    assert_eq!(
        exact.record.get("t"),
        Some(&Value::Uint(2_159_006_437_123_456_789))
    );
    assert_eq!(exact.event_time, at(noon()));
    let double = encode("EF").unwrap();
    assert_eq!(
        double.record.get("t"),
        Some(&Value::Float(2_159_006_437_123_456_768.0))
    );
    assert_eq!(
        double.event_time,
        at(instant("2026-06-01T12:00:00.123456768Z"))
    );
    let single = encode("ES").unwrap();
    assert_eq!(
        single.record.get("t"),
        Some(&Value::Float(2_159_006_392_420_139_008.0))
    );
    assert_eq!(
        single.event_time,
        at(instant("2026-06-01T11:59:15.420139008Z"))
    );
    // 30 s later the count is past half way to the next binary32, which
    // is after the event time, so it still reads the one before.
    let later = TelemetryRequest::new("el_v1", "ES", 7_u64)
        .with_at(instant("2026-06-01T12:00:30.123456789Z"));
    let later = sets.encode_telemetry(&later).unwrap();
    assert_eq!(later.payload, single.payload);
    assert_eq!(later.event_time, single.event_time);
    // A reading the field lists as no reading would leave ground time only:
    // a day after the epoch is 86,410 s on TAI, as 10 s applies before 1972.
    let sentinel =
        TelemetryRequest::new("el_v1", "ESENT", 7_u64).with_at(instant("1958-01-02T00:00:00Z"));
    assert_eq!(code(sets.encode_telemetry(&sentinel)), "invalid_raw");
    assert_eq!(code(encode("ESENT")), "ok");
    // A value given for the clock must be its reading.
    let given = TelemetryRequest::new("el_v1", "EI", 7_u64)
        .with_at(noon())
        .with_raw("t", RawValue::Integer(2_159_006_437_123_456_788));
    assert_eq!(code(sets.encode_telemetry(&given)), "wrong_constant");
    // Without the event time, the clock takes a value like any field.
    let raw = TelemetryRequest::new("el_v1", "EI", 7_u64)
        .with_raw("t", RawValue::Integer(2_159_006_437_123_456_789));
    assert_eq!(sets.encode_telemetry(&raw).unwrap().payload, exact.payload);
    let missing = TelemetryRequest::new("el_v1", "EI", 7_u64);
    let error = sets.encode_telemetry(&missing).unwrap_err();
    assert_eq!(error.code(), "missing_value");
    assert!(error.to_string().contains("give the event time"), "{error}");
}

/// A whole-second count rounds down before its epoch too: half a second
/// before 2000 is -1, and half a second after is 0.
#[test]
fn a_count_rounds_down_on_either_side_of_its_epoch() {
    let sets = sets_of(&clocks_bundle(), "secsat");
    for (time, count, back) in [
        ("1999-12-31T23:59:59.5Z", -1, "1999-12-31T23:59:59Z"),
        ("2000-01-01T00:00:00.5Z", 0, "2000-01-01T00:00:00Z"),
    ] {
        let request = TelemetryRequest::new("sec_v1", "S", 7_u64).with_at(instant(time));
        let encoded = sets.encode_telemetry(&request).unwrap();
        assert_eq!(encoded.record.get("t"), Some(&Value::Int(count)), "{time}");
        assert_eq!(encoded.event_time, at(instant(back)));
    }
}

/// TAI counts the leap second at the end of 2016, so the last nanosecond
/// of 2016 and the first of 2017 are 1,000,000,001 ns apart on it:
/// 1,861,920,035,999,999,999 and 1,861,920,037,000,000,000 from 1958.
#[test]
fn a_tai_clock_counts_the_leap_second() {
    let sets = sets_of(&clocks_bundle(), "elsat");
    for (time, count) in [
        (
            "2016-12-31T23:59:59.999999999Z",
            1_861_920_035_999_999_999_u64,
        ),
        ("2017-01-01T00:00:00Z", 1_861_920_037_000_000_000),
    ] {
        let request = TelemetryRequest::new("el_v1", "EI", 7_u64).with_at(instant(time));
        let encoded = sets.encode_telemetry(&request).unwrap();
        assert_eq!(encoded.record.get("t"), Some(&Value::Uint(count)), "{time}");
        assert_eq!(encoded.event_time, at(instant(time)));
    }
}

/// On GPS time, 18 s ahead of UTC from 1980-01-06, noon is week 2,421 and
/// 129,618,123 ms into it. Ten bits hold week 373, which decoding takes
/// back to week 2,421, the one nearest the time it's received.
#[test]
fn a_gps_week_of_fewer_bits_is_the_event_times_week_in_them() {
    let sets = sets_of(&clocks_bundle(), "gpssat");
    let request = TelemetryRequest::new("gps_v1", "G", 7_u64).with_at(noon());
    let encoded = sets.encode_telemetry(&request).unwrap();
    assert_eq!(encoded.record.get("wk"), Some(&Value::Uint(373)));
    assert_eq!(encoded.record.get("tow"), Some(&Value::Uint(129_618_123)));
    assert_eq!(encoded.event_time, at(instant("2026-06-01T12:00:00.123Z")));
    // Before the first week, no week counts it.
    let early =
        TelemetryRequest::new("gps_v1", "G", 7_u64).with_at(instant("1980-01-05T00:00:00Z"));
    assert_eq!(code(sets.encode_telemetry(&early)), "raw_range");
    let missing = TelemetryRequest::new("gps_v1", "G", 7_u64);
    assert_eq!(code(sets.encode_telemetry(&missing)), "missing_value");
}

/// An hour, 123 ms and 456,789 ns after booting at 11:00:00, the uptime
/// clock reads 3,600,123 ms. The correlation packet's seconds clock reads
/// 1,780,315,200, so it teaches a boot instant 3,600.123 s before that:
/// 10:59:59.877.
#[test]
fn boot_clocks_count_from_the_boot_instant() {
    let sets = sets_of(&clocks_bundle(), "bootsat");
    let booted = instant("2026-06-01T11:00:00Z");
    let hk = TelemetryRequest::new("boot_v1", "HK", 7_u64)
        .with_at(noon())
        .with_booted(booted);
    let encoded = sets.encode_telemetry(&hk).unwrap();
    assert_eq!(encoded.record.get("up"), Some(&Value::Uint(3_600_123)));
    assert_eq!(
        encoded.event_time,
        EventTime::SinceBoot {
            value: 3_600_123,
            unit: TimeUnit::Ms
        }
    );
    assert_eq!(encoded.booted, None);
    let corr = TelemetryRequest::new("boot_v1", "CORR", 7_u64)
        .with_at(noon())
        .with_booted(booted);
    let encoded = sets.encode_telemetry(&corr).unwrap();
    assert_eq!(encoded.record.get("clk"), Some(&Value::Uint(1_780_315_200)));
    assert_eq!(encoded.record.get("up"), Some(&Value::Uint(3_600_123)));
    assert_eq!(encoded.booted, Some(instant("2026-06-01T10:59:59.877Z")));
    // The uptime needs both instants.
    let unbooted = TelemetryRequest::new("boot_v1", "HK", 7_u64).with_at(noon());
    let error = sets.encode_telemetry(&unbooted).unwrap_err();
    assert_eq!(error.code(), "missing_value");
    assert!(
        error.to_string().contains("give the boot instant"),
        "{error}"
    );
    let before = TelemetryRequest::new("boot_v1", "HK", 7_u64)
        .with_at(booted)
        .with_booted(noon());
    assert_eq!(code(sets.encode_telemetry(&before)), "raw_range");
    // A param the clock maps must be what decoding its reading gives.
    let agreeing = hk
        .clone()
        .with_param("sys.uptime", SampleValue::F64(3_600_123.0));
    assert_eq!(code(sets.encode_telemetry(&agreeing)), "ok");
    let disagreeing = hk.with_param("sys.uptime", SampleValue::F64(3_600_000.0));
    assert_eq!(code(sets.encode_telemetry(&disagreeing)), "wrong_constant");
}

#[test]
fn an_iso_8601_time_is_written_in_mdls_spelling() {
    let sets = sets_of(&clocks_bundle(), "isoprobe");
    let with = TelemetryRequest::new("iso_v1", "WITH", 7_u64)
        .with_at(noon())
        .with_raw("v", RawValue::Float(1.5));
    let encoded = sets.encode_telemetry(&with).unwrap();
    assert_eq!(
        String::from_utf8(encoded.payload).unwrap(),
        r#"{"kind":"a","sat":7,"t":"2026-06-01T12:00:00.123456789Z","v":1.5}"#
    );
    assert_eq!(encoded.event_time, at(noon()));
    let bare = TelemetryRequest::new("iso_v1", "BARE", 7_u64)
        .with_at(noon())
        .with_raw("v", RawValue::Float(1.5));
    assert_eq!(
        String::from_utf8(sets.encode_telemetry(&bare).unwrap().payload).unwrap(),
        r#"{"kind":"b","sat":7,"t":"2026-06-01T12:00:00.123456789Z","v":1.5}"#
    );
    let timeless =
        TelemetryRequest::new("iso_v1", "BARE", 7_u64).with_raw("v", RawValue::Float(1.5));
    let error = sets.encode_telemetry(&timeless).unwrap_err();
    assert_eq!(error.code(), "missing_value");
    assert!(error.to_string().contains("packet BARE, /t:"), "{error}");
    // Text that isn't an RFC 3339 instant gives no event time.
    let unreadable = TelemetryRequest::new("iso_v1", "WITH", 7_u64)
        .with_raw("t", RawValue::Text("noon".to_string()))
        .with_raw("v", RawValue::Float(1.5));
    let error = sets.encode_telemetry(&unreadable).unwrap_err();
    assert_eq!(error.code(), "invalid_time");
    assert!(error.to_string().contains("packet WITH, t:"), "{error}");
    // Any RFC 3339 spelling reads, but the event time is MDL's.
    let spelled = with.with_raw(
        "t",
        RawValue::Text("2026-06-01T14:00:00.123456789+02:00".to_string()),
    );
    assert_eq!(code(sets.encode_telemetry(&spelled)), "wrong_constant");
}

/// A packet with no clock takes the event time and has ground time only.
#[test]
fn a_packet_without_a_clock_has_ground_time_whatever_the_request_gives() {
    let sets = sets_of(&bundle(), "sat");
    let plain = sets.encode_telemetry(&hk()).unwrap();
    let stamped = sets
        .encode_telemetry(&hk().with_at(noon()).with_booted(noon()))
        .unwrap();
    assert_eq!(stamped.payload, plain.payload);
    assert_eq!(stamped.event_time, EventTime::Ground);
    let sets = sets_of(&clocks_bundle(), "elsat");
    let none = TelemetryRequest::new("el_v1", "NONE", 7_u64).with_at(noon());
    assert_eq!(
        sets.encode_telemetry(&none).unwrap().event_time,
        EventTime::Ground
    );
}

#[test]
fn float_keys_bound_the_finite_values() {
    assert_eq!(clock::SINGLE_TOP, i64::from(f32::MAX.to_bits()));
    assert_eq!(
        clock::DOUBLE_TOP,
        i64::try_from(f64::MAX.to_bits()).unwrap()
    );
}

/// What decoding reads a float clock as (`decoding.md` §8): the count times
/// the unit in binary64, rounded down to whole nanoseconds.
fn float_decodes(x: f64, unit_nanos: i128) -> i128 {
    (x * unit_nanos as f64).floor() as i128
}

proptest::proptest! {
    /// A float clock's reading decodes to at most the time, is the least
    /// value that decodes to as much, and nothing decodes later without
    /// passing the time: where its values are finer than a nanosecond, one
    /// decodes to the time exactly; otherwise the first value up that
    /// decodes to more passes it. (Coarser values can still decode alike in
    /// twos or threes, as the product rounds to binary64 too.)
    #[test]
    fn a_float_clock_reads_its_latest_value_at_or_before(
        nanos in -(1_i128 << 70)..(1_i128 << 70),
        unit in proptest::sample::select(vec![TimeUnit::S, TimeUnit::Ms, TimeUnit::Us, TimeUnit::Ns]),
        single in proptest::bool::ANY,
    ) {
        let unit_nanos = crate::decode::unit_nanos(unit);
        let x = clock::float_count(nanos, unit, single).unwrap();
        let step = |x: f64, up: bool| {
            if single {
                let y = x as f32;
                f64::from(if up { y.next_up() } else { y.next_down() })
            } else if up {
                x.next_up()
            } else {
                x.next_down()
            }
        };
        if single {
            proptest::prop_assert_eq!(f64::from(x as f32), x);
        }
        let read = float_decodes(x, unit_nanos);
        proptest::prop_assert!(read <= nanos, "{x} reads {read}, past {nanos}");
        let below = step(x, false);
        proptest::prop_assert!(float_decodes(below, unit_nanos) < read, "{below} reads as {x} does");
        if (step(x, true) - x) * (unit_nanos as f64) < 1.0 {
            proptest::prop_assert_eq!(read, nanos);
        } else {
            let mut above = step(x, true);
            for _ in 0..8 {
                if float_decodes(above, unit_nanos) != read {
                    break;
                }
                above = step(above, true);
            }
            proptest::prop_assert!(
                float_decodes(above, unit_nanos) > nanos,
                "{above} reads {}, not past {nanos}", float_decodes(above, unit_nanos)
            );
        }
    }
}
