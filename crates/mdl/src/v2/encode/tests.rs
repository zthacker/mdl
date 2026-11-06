use proptest::prelude::*;
use serde_json::{Value as Json, json};

use super::*;
use crate::v2::artifact::{Artifacts, Written, build};
use crate::v2::ir::Artifact;
use crate::v2::read::{Loaded, read_artifact, read_bundle};
use crate::v2::validate::validate;

const FULL_BUNDLE: &str = include_str!("../../../tests/fixtures/v2/full_bundle.json");

fn fixture() -> Json {
    serde_json::from_str(FULL_BUNDLE).unwrap()
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

fn family_sets(artifacts: &Artifacts, family: &str) -> FamilySets {
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

fn sets_of(bundle: &Json, family: &str) -> FamilySets {
    family_sets(&artifacts_of(bundle), family)
}

fn shown(record: &Record<'_>) -> Vec<(String, Value<'static>)> {
    record
        .iter()
        .map(|(name, value)| (name.to_string(), owned(value)))
        .collect()
}

fn owned(value: &Value<'_>) -> Value<'static> {
    match value {
        Value::Uint(n) => Value::Uint(*n),
        Value::Int(n) => Value::Int(*n),
        Value::Float(x) => Value::Float(*x),
        Value::Bool(b) => Value::Bool(*b),
        Value::Str(text) => Value::Str(text.clone()),
        Value::Bytes(bytes) => Value::Bytes(bytes.clone()),
        _ => Value::Null,
    }
}

fn code<T: fmt::Debug>(result: Result<T, EncodeError>) -> &'static str {
    match result {
        Ok(encoded) => panic!("expected an error, encoded {encoded:?}"),
        Err(err) => err.code(),
    }
}

fn set_mode(set: &str) -> CommandRequest {
    CommandRequest::new(set, "eps.set_mode")
}

#[test]
fn arguments_are_written_through_the_inverse_of_their_conversions() {
    let artifacts = artifacts_of(&fixture());
    let sets = family_sets(&artifacts, "vendor_x");
    let request = set_mode("vendor_x_v2")
        .with_argument("mode", "NOMINAL")
        .with_argument("delay_s", 1.5);
    let encoded = sets.encode(&request).unwrap();
    // Opcode 26, NOMINAL is 1, 1.5 s is 15 counts of 0.1 s, then the ISO
    // checksum of those four bytes.
    assert_eq!(encoded.payload, [26, 1, 0x00, 0x0F, 0x5B, 0x7A]);
    assert_eq!(encoded.packet.name, "SET_MODE");
    assert_eq!(encoded.packet.metadata["apid"], Scalar::Int(300));
    assert_eq!(encoded.def_version, &artifacts.sets["vendor_x_v2"].digest);
    assert_eq!(
        shown(&encoded.record),
        vec![
            ("opcode".to_string(), Value::Uint(26)),
            ("mode".to_string(), Value::Uint(1)),
            ("delay".to_string(), Value::Uint(15)),
            ("check".to_string(), Value::Uint(0x5B7A)),
        ]
    );
    assert!(encoded.defaulted.is_empty());
    assert_eq!(encoded.deprecated, None);
}

#[test]
fn a_left_out_argument_takes_its_default() {
    let sets = sets_of(&fixture(), "vendor_x");
    let encoded = sets
        .encode(&set_mode("vendor_x_v2").with_argument("mode", "SAFE"))
        .unwrap();
    assert_eq!(encoded.payload, [26, 0, 0, 0, 0x7D, 0x68]);
    assert_eq!(encoded.defaulted, ["delay_s"]);
}

#[test]
fn converted_values_round_to_the_nearest_raw_value_ties_away_from_zero() {
    let sets = sets_of(&fixture(), "vendor_x");
    let delay = |seconds: ArgumentValue| {
        let request = set_mode("vendor_x_v2")
            .with_argument("mode", "SAFE")
            .with_argument("delay_s", seconds);
        owned(sets.encode(&request).unwrap().record.get("delay").unwrap())
    };
    // 0.15 * 10 is 1.5000000000000002 in binary; 0.25 * 10 is exactly 2.5.
    assert_eq!(delay(0.15.into()), Value::Uint(2));
    assert_eq!(delay(0.25.into()), Value::Uint(3));
    assert_eq!(delay(0.3.into()), Value::Uint(3));
    assert_eq!(delay(0.04.into()), Value::Uint(0));
    // An integer is a number too.
    assert_eq!(delay(2_u64.into()), Value::Uint(20));
}

#[test]
fn lengths_are_computed_and_checksums_written_last() {
    let sets = sets_of(&fixture(), "vendor_x");
    let request = CommandRequest::new("vendor_x_v2", "eps.load_table")
        .with_argument("slot", 2_u64)
        .with_argument("table", vec![0xAA, 0xBB, 0xCC]);
    let encoded = sets.encode(&request).unwrap();
    assert_eq!(
        encoded.payload,
        [3, 2, 0x00, 0x03, 0xAA, 0xBB, 0xCC, 0x99, 0xBA]
    );
    assert_eq!(encoded.record.get("len"), Some(&Value::Uint(3)));
    // CRC-16/IBM-3740 of everything before it.
    assert_eq!(encoded.record.get("crc"), Some(&Value::Uint(0x99BA)));
}

#[test]
fn a_target_field_holds_the_native_id_and_checksums_cover_each_other() {
    let sets = sets_of(&fixture(), "legacy");
    let request = set_mode("legacy_v1")
        .with_argument("mode", "NOMINAL")
        .with_target(NativeId::from_text("box-1"));
    let encoded = sets.encode(&request).unwrap();
    // Mode, the id as zero-terminated ASCII, the xor of those, then their
    // 16-bit sum (the xor byte included), little-endian like the family.
    let mut expected = vec![1];
    expected.extend_from_slice(b"box-1\0");
    expected.extend_from_slice(&[0x68, 0x10, 0x02]);
    assert_eq!(encoded.payload, expected);
    assert_eq!(
        encoded.record.get("unit"),
        Some(&Value::Str("box-1".to_string()))
    );

    let safe = || set_mode("legacy_v1").with_argument("mode", "SAFE");
    assert_eq!(code(sets.encode(&safe())), "missing_target");
    let with_target = |id: &str| safe().with_target(NativeId::from_text(id));
    assert_eq!(code(sets.encode(&with_target("bóx"))), "invalid_text");
    assert_eq!(
        code(sets.encode(&with_target("a\0b"))),
        "contains_terminator"
    );
    // An integer id in a string field is written in decimal.
    let encoded = sets.encode(&safe().with_target(42_u64)).unwrap();
    assert_eq!(&encoded.payload[1..4], b"42\0");
}

#[test]
fn an_argument_a_set_leaves_out_can_only_be_its_default() {
    let sets = sets_of(&fixture(), "legacy");
    let request = |delay: f64| {
        set_mode("legacy_v1")
            .with_argument("mode", "SAFE")
            .with_argument("delay_s", delay)
            .with_target(NativeId::from_text("box-1"))
    };
    assert!(sets.encode(&request(0.0)).is_ok());
    assert_eq!(code(sets.encode(&request(5.0))), "not_default");
}

#[test]
fn a_json_payload_is_compact_with_its_keys_in_order() {
    let sets = sets_of(&fixture(), "vendor_j");
    let request = set_mode("vendor_j_v1")
        .with_argument("mode", "NOMINAL")
        .with_argument("delay_s", 1.5);
    let encoded = sets.encode(&request).unwrap();
    assert_eq!(
        String::from_utf8(encoded.payload).unwrap(),
        r#"{"args":{"delay":1.5,"mode":"NOMINAL"},"op":"set_mode"}"#
    );
    let encoded = sets
        .encode(&set_mode("vendor_j_v1").with_argument("mode", "SAFE"))
        .unwrap();
    assert_eq!(
        String::from_utf8(encoded.payload).unwrap(),
        r#"{"args":{"delay":0,"mode":"SAFE"},"op":"set_mode"}"#
    );
}

#[test]
fn requests_are_checked_against_the_catalog() {
    let sets = sets_of(&fixture(), "vendor_x");
    let nominal = || set_mode("vendor_x_v2").with_argument("mode", "NOMINAL");
    let table = |slot: ArgumentValue| {
        CommandRequest::new("vendor_x_v2", "eps.load_table")
            .with_argument("table", vec![1])
            .with_argument("slot", slot)
    };
    let cases: Vec<(CommandRequest, &str)> = vec![
        (set_mode("vendor_x_v9"), "unknown_set"),
        (
            CommandRequest::new("vendor_x_v2", "eps.nope"),
            "unknown_command",
        ),
        (nominal().with_argument("speed", 1), "unknown_argument"),
        (
            nominal().with_argument("mode", "SAFE"),
            "duplicate_argument",
        ),
        (set_mode("vendor_x_v2"), "missing_argument"),
        (
            set_mode("vendor_x_v2").with_argument("mode", 1),
            "wrong_type",
        ),
        (
            set_mode("vendor_x_v2").with_argument("mode", "SCIENCE"),
            "unknown_state",
        ),
        (nominal().with_argument("delay_s", 3600.5), "out_of_range"),
        (nominal().with_argument("delay_s", -0.5), "out_of_range"),
        (nominal().with_argument("delay_s", f64::NAN), "non_finite"),
        (nominal().with_argument("delay_s", "soon"), "wrong_type"),
        (table(ArgumentValue::Uint(8)), "out_of_range"),
        (table(ArgumentValue::Int(-1)), "out_of_range"),
        (table(ArgumentValue::Float(1.5)), "wrong_type"),
        (table(ArgumentValue::Bool(true)), "wrong_type"),
        // An integral double past i128 is still an integer out of range.
        (table(ArgumentValue::Float(1e39)), "out_of_range"),
    ];
    for (request, expected) in cases {
        assert_eq!(code(sets.encode(&request)), expected, "{request:?}");
    }
    // Numbers compare by value: 3.0 is the integer 3.
    assert!(sets.encode(&table(ArgumentValue::Float(3.0))).is_ok());
    // The JSON set doesn't encode eps.load_table.
    let json = sets_of(&fixture(), "vendor_j");
    assert_eq!(
        code(json.encode(&CommandRequest::new("vendor_j_v1", "eps.load_table"))),
        "unknown_command"
    );
}

#[test]
fn errors_name_the_command_and_what_went_wrong() {
    let sets = sets_of(&fixture(), "vendor_x");
    let request = set_mode("vendor_x_v2")
        .with_argument("mode", "NOMINAL")
        .with_argument("delay_s", 4000);
    assert_eq!(
        sets.encode(&request).unwrap_err().to_string(),
        "command eps.set_mode, argument delay_s: 4000 is outside its range, 0 to 3600"
    );
}

/// Layouts the fixture doesn't have. Family `lab`: a fixed-length ASCII
/// name, a binary32, padding and a 12-bit signed field (lab_v1); a command
/// with no payload (lab_v2); linear and polynomial inverses (lab_v3); an
/// empty value read to the end after a gap (lab_v4); a length stored as
/// (n + 2) / 2 (lab_v5); two commands told apart by a text tag (lab_v6);
/// conversions whose inverse doesn't undo them exactly, and a negative
/// scale (lab_v7). Family `web`: a JSON command with no fields.
fn lab() -> Json {
    let bytes = |name: &str| json!({ "arguments": [{ "name": name, "type": "bytes" }] });
    let tag = |text: &str| {
        json!({ "kind": "field", "name": "tag", "type": "string", "text": "ascii",
                "length": { "kind": "fixed", "bytes": 4 }, "value": text })
    };
    json!({
        "mdl_version": "2.0",
        "catalog": { "commands": {
            "lab.tune": { "arguments": [
                { "name": "name", "type": "str" },
                { "name": "gain", "type": "f64", "default": 1.0 },
                { "name": "offset", "type": "i64", "range": { "min": -2048, "max": 2047 },
                  "default": 0 }
            ] },
            "lab.ping": {},
            "lab.scale": { "arguments": [
                { "name": "level", "type": "f64", "range": { "min": 0, "max": 25 } },
                { "name": "trim", "type": "f64", "range": { "min": 0, "max": 10 } }
            ] },
            "lab.blob": bytes("data"),
            "lab.load": bytes("data"),
            "lab.pong": {},
            "lab.drift": { "arguments": [
                { "name": "temp", "type": "f64", "range": { "min": -100, "max": 100 } },
                { "name": "delay", "type": "f64", "range": { "min": 0, "max": 3600.06 } },
                { "name": "count", "type": "f64", "range": { "min": 0, "max": 100.09 } },
                { "name": "gain", "type": "f64", "range": { "min": 0, "max": 10 } }
            ] }
        } },
        "families": [
            { "name": "lab", "encoding": "binary", "byte_order": "big",
              "alias": { "kind": "fixed", "value": "lab-1" } },
            { "name": "web", "encoding": "json", "alias": { "kind": "path", "pointer": "/id" } }
        ],
        "sets": [
            { "name": "lab_v1", "family": "lab", "commands": [{
                "name": "TUNE", "canonical": "lab.tune", "entries": [
                    { "kind": "field", "name": "name", "type": "string", "text": "ascii",
                      "length": { "kind": "fixed", "bytes": 4 }, "argument": "name" },
                    { "kind": "field", "name": "gain", "type": "float", "size_bits": 32,
                      "argument": "gain" },
                    { "kind": "padding", "size_bits": 4 },
                    { "kind": "field", "name": "offset", "type": "int", "size_bits": 12,
                      "argument": "offset" }
                ]
            }] },
            { "name": "lab_v2", "family": "lab", "commands": [
                { "name": "PING", "canonical": "lab.ping" }
            ] },
            { "name": "lab_v3", "family": "lab", "commands": [{
                "name": "SCALE", "canonical": "lab.scale", "entries": [
                    { "kind": "field", "name": "level", "type": "uint", "size_bits": 8,
                      "conversion": { "kind": "linear", "scale": 0.1, "offset": 0.0 },
                      "argument": "level" },
                    { "kind": "field", "name": "trim", "type": "uint", "size_bits": 8,
                      "conversion": { "kind": "poly", "coefficients": [0.01, 0.1] },
                      "inverse": { "kind": "poly", "coefficients": [-0.1, 10] },
                      "argument": "trim" },
                    { "kind": "field", "name": "unit", "type": "int", "size_bits": 16,
                      "target": true }
                ]
            }] },
            { "name": "lab_v4", "family": "lab", "commands": [{
                "name": "BLOB", "canonical": "lab.blob", "entries": [
                    { "kind": "field", "name": "kind", "type": "uint", "size_bits": 8,
                      "value": 44 },
                    { "kind": "field", "name": "data", "type": "bytes", "offset_bits": 32,
                      "length": { "kind": "remaining" }, "argument": "data" }
                ]
            }] },
            { "name": "lab_v5", "family": "lab", "commands": [{
                "name": "LOAD", "canonical": "lab.load", "entries": [
                    { "kind": "field", "name": "len", "type": "uint", "size_bits": 8 },
                    { "kind": "field", "name": "data", "type": "bytes",
                      "length": { "kind": "field", "field": "len", "multiply": 2, "add": -2 },
                      "argument": "data" }
                ]
            }] },
            { "name": "lab_v6", "family": "lab", "commands": [
                { "name": "PING", "canonical": "lab.ping", "entries": [tag("PING")] },
                { "name": "PONG", "canonical": "lab.pong", "entries": [tag("PONG")] }
            ] },
            { "name": "lab_v7", "family": "lab", "commands": [{
                "name": "DRIFT", "canonical": "lab.drift", "entries": [
                    { "kind": "field", "name": "temp", "type": "float", "size_bits": 64,
                      "conversion": { "kind": "linear", "scale": 0.01, "offset": -40.0 },
                      "argument": "temp" },
                    { "kind": "field", "name": "delay", "type": "int", "size_bits": 32,
                      "conversion": { "kind": "linear", "scale": -0.1, "offset": 0.0 },
                      "argument": "delay" },
                    { "kind": "field", "name": "count", "type": "uint", "size_bits": 16,
                      "conversion": { "kind": "poly", "coefficients": [0, 0.1] },
                      "inverse": { "kind": "poly", "coefficients": [0.7, 10] },
                      "argument": "count" },
                    { "kind": "field", "name": "gain", "type": "float", "size_bits": 64,
                      "conversion": { "kind": "poly", "coefficients": [0, 2] },
                      "inverse": { "kind": "poly", "coefficients": [0.001, 0.5] },
                      "argument": "gain" }
                ]
            }] },
            { "name": "web_v1", "family": "web", "commands": [
                { "name": "PING", "canonical": "lab.ping" }
            ] }
        ]
    })
}

fn tune(name: &str) -> CommandRequest {
    CommandRequest::new("lab_v1", "lab.tune").with_argument("name", name)
}

#[test]
fn fixed_strings_floats_padding_and_signed_fields_are_laid_out_bit_by_bit() {
    let sets = sets_of(&lab(), "lab");
    let request = tune("PING")
        .with_argument("gain", 1.5)
        .with_argument("offset", -2);
    let encoded = sets.encode(&request).unwrap();
    // 1.5 as binary32 is 3FC00000; four zero bits; -2 in 12 bits is FFE.
    assert_eq!(
        encoded.payload,
        [b'P', b'I', b'N', b'G', 0x3F, 0xC0, 0x00, 0x00, 0x0F, 0xFE]
    );
    assert_eq!(encoded.record.get("offset"), Some(&Value::Int(-2)));
    // A binary32 field holds the nearest binary32, as decoding reads it.
    let encoded = sets
        .encode(&tune("PING").with_argument("gain", 0.1))
        .unwrap();
    assert_eq!(
        encoded.record.get("gain"),
        Some(&Value::Float(f64::from(0.1_f32)))
    );

    assert_eq!(code(sets.encode(&tune("PIN"))), "bad_length");
    assert_eq!(code(sets.encode(&tune("PÏNG"))), "invalid_text");
    assert_eq!(
        code(sets.encode(&tune("PING").with_argument("gain", 1e39))),
        "raw_range"
    );
}

#[test]
fn a_command_with_no_payload_encodes_to_nothing() {
    let sets = sets_of(&lab(), "lab");
    let encoded = sets
        .encode(&CommandRequest::new("lab_v2", "lab.ping"))
        .unwrap();
    assert!(encoded.payload.is_empty());
    assert_eq!(encoded.record, Record::default());
}

#[test]
fn inverse_conversions_round_each_operation_as_written() {
    let sets = sets_of(&lab(), "lab");
    let scale = |level: f64, trim: f64| {
        let request = CommandRequest::new("lab_v3", "lab.scale")
            .with_argument("level", level)
            .with_argument("trim", trim)
            .with_target(-5_i64);
        sets.encode(&request).unwrap().payload
    };
    // (0.15 - 0) / 0.1 is 1.4999999999999998, not 1.5 as 0.15 * (1 / 0.1)
    // would give; -0.1 + 10 * 0.36 is 3.4999999999999996, not 3.5 as a
    // fused multiply-add would give. Then the target, -5 as an int16.
    assert_eq!(scale(0.15, 0.36), [1, 3, 0xFF, 0xFB]);
    assert_eq!(scale(0.25, 0.0), [3, 0, 0xFF, 0xFB]);
}

#[test]
fn binary32_fields_round_ties_to_even() {
    let sets = sets_of(&lab(), "lab");
    let gain = |gain: f64| {
        sets.encode(&tune("PING").with_argument("gain", gain))
            .map(|encoded| encoded.payload[4..8].to_vec())
    };
    // 1 + 2^-24 is halfway between 1 and the next binary32: the even one.
    assert_eq!(
        gain(1.0 + 2_f64.powi(-24)).unwrap(),
        [0x3F, 0x80, 0x00, 0x00]
    );
    assert_eq!(
        gain(1.0 + 3.0 * 2_f64.powi(-24)).unwrap(),
        [0x3F, 0x80, 0x00, 0x02]
    );
    // Halfway between f32::MAX and 2^128 rounds to infinity; just below, it
    // doesn't.
    let midpoint: f64 = 3.402_823_567_797_336_6e38;
    assert_eq!(
        gain(midpoint.next_down()).unwrap(),
        [0x7F, 0x7F, 0xFF, 0xFF]
    );
    let error = gain(midpoint).unwrap_err();
    assert_eq!(error.code(), "raw_range");
    assert_eq!(
        error.to_string(),
        "command packet TUNE, gain: raw value 3.4028235677973366e+38 doesn't fit; the field \
         holds finite binary32 numbers, up to 3.4028234663852886e+38 (this one rounds to \
         infinity)"
    );
}

#[test]
fn the_payload_reaches_the_start_of_an_empty_value_after_a_gap() {
    let sets = sets_of(&lab(), "lab");
    let blob = |data: Vec<u8>| {
        let request = CommandRequest::new("lab_v4", "lab.blob").with_argument("data", data);
        sets.encode(&request).unwrap().payload
    };
    assert_eq!(blob(Vec::new()), [44, 0, 0, 0]);
    assert_eq!(blob(vec![9]), [44, 0, 0, 0, 9]);
}

#[test]
fn a_length_is_stored_as_its_field_says_or_refused() {
    let sets = sets_of(&lab(), "lab");
    let load = |data: Vec<u8>| {
        sets.encode(&CommandRequest::new("lab_v5", "lab.load").with_argument("data", data))
    };
    // (4 + 2) / 2 is 3; a decoder reads 3 * 2 - 2.
    assert_eq!(load(vec![1, 2, 3, 4]).unwrap().payload, [3, 1, 2, 3, 4]);
    assert_eq!(load(Vec::new()).unwrap().payload, [1]);
    let error = load(vec![1, 2, 3]).unwrap_err();
    assert_eq!(
        error.to_string(),
        "command packet LOAD, data: a value of 3 bytes doesn't fit: len holds (length + 2) / 2, \
         which must be a whole number of at least 0"
    );
    assert_eq!(code(load(vec![0; 510])), "bad_length");
}

#[test]
fn a_json_command_with_no_fields_is_an_empty_object() {
    let sets = sets_of(&lab(), "web");
    let encoded = sets
        .encode(&CommandRequest::new("web_v1", "lab.ping"))
        .unwrap();
    assert_eq!(encoded.payload, b"{}");
}

#[test]
fn a_json_record_holds_what_decoding_the_text_gives() {
    let sets = sets_of(&fixture(), "vendor_j");
    let request = set_mode("vendor_j_v1")
        .with_argument("mode", "SAFE")
        .with_argument("delay_s", -0.0);
    let encoded = sets.encode(&request).unwrap();
    assert!(
        String::from_utf8(encoded.payload)
            .unwrap()
            .contains(r#""delay":0"#)
    );
    // -0.0 is written 0, which reads back as +0.0.
    let Some(Value::Float(delay)) = encoded.record.get("delay") else {
        panic!("a float delay");
    };
    assert!(delay.is_sign_positive());
}

#[test]
fn bindings_are_checked_against_target_fields() {
    use crate::v2::bindings::{Binding, Bindings, Row};
    use crate::v2::diagnostics::Code;
    let t = "2026-01-01T00:00:00Z".parse().unwrap();
    let bindings = Bindings::new(vec![Row::bound(
        "legacy",
        NativeId::from_text("box-1"),
        t,
        Binding::new("BOX").with_set("legacy_v1"),
    )])
    .unwrap();
    let validated = |bundle: &Json| {
        let bytes = serde_json::to_vec(bundle).unwrap();
        validate(read_bundle(&bytes).unwrap().value).unwrap()
    };
    // The fixture's target field is zero-terminated ASCII: box-1 fits.
    assert_eq!(bindings.check(&validated(&fixture())), Ok(()));
    // A uint8 target can't hold it.
    let mut bundle = fixture();
    bundle["sets"][2]["commands"][0]["entries"][1] = json!(
        { "kind": "field", "name": "unit", "type": "uint", "size_bits": 8, "target": true });
    let diagnostics = bindings.check(&validated(&bundle)).unwrap_err();
    let mut found: Vec<_> = diagnostics
        .iter()
        .map(|d| (d.code, d.path.clone(), d.message.clone()))
        .collect();
    assert_eq!(
        found,
        vec![(
            Code::NativeId,
            "/binding/0/native_id".to_string(),
            "commands can't be sent to box-1: set legacy_v1, command packet SET_MODE, field \
             unit: native id box-1 isn't an integer"
                .to_string()
        )]
    );
    // A text target whose length another field holds: 5 bytes can't be
    // stored as length / 2.
    let mut bundle = fixture();
    let entries = bundle["sets"][2]["commands"][0]["entries"]
        .as_array_mut()
        .unwrap();
    entries[1] = json!({ "kind": "field", "name": "unit", "type": "string", "text": "ascii",
                         "length": { "kind": "field", "field": "unit_len", "multiply": 2 },
                         "target": true });
    entries.insert(
        1,
        json!({ "kind": "field", "name": "unit_len", "type": "uint", "size_bits": 8 }),
    );
    found = bindings
        .check(&validated(&bundle))
        .unwrap_err()
        .iter()
        .map(|d| (d.code, d.path.clone(), d.message.clone()))
        .collect();
    assert_eq!(found.len(), 1);
    assert!(
        found[0]
            .2
            .ends_with("unit_len holds length / 2, which must be a whole number of at least 0"),
        "{}",
        found[0].2
    );
}

fn argument_value() -> impl Strategy<Value = ArgumentValue> {
    prop_oneof![
        any::<bool>().prop_map(ArgumentValue::Bool),
        any::<i64>().prop_map(ArgumentValue::Int),
        any::<u64>().prop_map(ArgumentValue::Uint),
        any::<f64>().prop_map(ArgumentValue::Float),
        prop_oneof![Just("SAFE"), Just("NOMINAL"), Just("PING"), Just("x\0")]
            .prop_map(|text| ArgumentValue::Text(text.to_string())),
        ".{0,6}".prop_map(ArgumentValue::Text),
        proptest::collection::vec(any::<u8>(), 0..300).prop_map(ArgumentValue::Bytes),
    ]
}

proptest! {
    #[test]
    fn any_request_is_encoded_or_refused_without_panicking(
        lab_family in any::<bool>(),
        set_index in 0_usize..3,
        command in prop_oneof![
            Just("eps.set_mode"), Just("eps.load_table"), Just("lab.tune"), Just("lab.ping")
        ],
        arguments in proptest::collection::vec(
            (prop_oneof![
                Just("mode"), Just("delay_s"), Just("slot"), Just("table"),
                Just("name"), Just("gain"), Just("offset")
            ], argument_value()),
            0..5,
        ),
        target in proptest::option::of(prop_oneof![
            any::<u64>().prop_map(NativeId::from),
            ".{0,4}".prop_map(NativeId::from_text),
        ]),
    ) {
        let (bundle, family, set) = if lab_family {
            (lab(), "lab", ["lab_v1", "lab_v2", "lab_v1"][set_index])
        } else {
            let families = [("vendor_x", "vendor_x_v2"), ("vendor_j", "vendor_j_v1"), ("legacy", "legacy_v1")];
            let (family, set) = families[set_index];
            (fixture(), family, set)
        };
        let sets = sets_of(&bundle, family);
        let mut request = CommandRequest::new(set, command);
        for (name, value) in arguments {
            request = request.with_argument(name, value);
        }
        request.target = target;
        if let Err(error) = sets.encode(&request) {
            // Every refusal is about the request, never the definition.
            prop_assert_ne!(error.code(), "invalid_layout", "{}", error);
        }
    }
}

#[test]
fn json_values_are_written_as_rfc_8785_writes_them_with_exact_integers() {
    use super::write::json_text;
    let text = |value: Value<'static>| json_text(&value).unwrap();
    assert_eq!(text(Value::Float(1.5)), "1.5");
    assert_eq!(text(Value::Float(0.0)), "0");
    assert_eq!(text(Value::Float(-0.0)), "0");
    assert_eq!(text(Value::Float(2.0)), "2");
    assert_eq!(text(Value::Float(1e21)), "1e+21");
    assert_eq!(text(Value::Float(1e-7)), "1e-7");
    assert_eq!(text(Value::Float(0.1)), "0.1");
    // Past 2^53, where RFC 8785 would write the nearest double.
    assert_eq!(text(Value::Uint(u64::MAX)), "18446744073709551615");
    assert_eq!(text(Value::Int(i64::MIN)), "-9223372036854775808");
    assert_eq!(text(Value::Bool(true)), "true");
    let escaped = text(Value::Str(format!("a{}b{}é", '"', '\u{1}')));
    assert_eq!(escaped, format!("{0}a{1}{0}b{1}u0001é{0}", '"', '\\'));
    assert!(json_text(&Value::Float(f64::NAN)).is_err());
}

// ---- Decoding commands ----

use crate::v2::decode::{CommandDecodeError, DecodedCommand};
use crate::v2::ir::{Argument, Bundle, ByteLength, Field, ParamType, ScalarType, TextEncoding};
use crate::v2::message::{Message, MetaValue, Payload};
use crate::v2::time::Timestamp;

fn binary(payload: &[u8]) -> Message<'_> {
    Message::new(Payload::Binary(payload), Timestamp::UNIX_EPOCH)
}

/// vendor_x frames its commands with apid 300, which identifies them along
/// with their opcodes.
const APID: &[(&str, MetaValue<'_>)] = &[("apid", MetaValue::Uint(300))];

fn framed(payload: &[u8]) -> Message<'_> {
    binary(payload).with_metadata(APID)
}

fn arguments(decoded: &DecodedCommand<'_>) -> Vec<(String, ArgumentValue)> {
    decoded
        .arguments
        .iter()
        .map(|(name, value)| (name.to_string(), value.clone()))
        .collect()
}

fn decode_code<T: fmt::Debug>(result: Result<T, CommandDecodeError>) -> &'static str {
    match result {
        Ok(decoded) => panic!("expected an error, decoded {decoded:?}"),
        Err(err) => err.code(),
    }
}

#[test]
fn a_command_payload_decodes_to_its_command_and_arguments() {
    let sets = sets_of(&fixture(), "vendor_x");
    let decoded = sets
        .decode_command("vendor_x_v2", &framed(&[26, 1, 0x00, 0x0F, 0x5B, 0x7A]))
        .unwrap();
    assert_eq!(decoded.command(), "eps.set_mode");
    assert_eq!(decoded.packet.name, "SET_MODE");
    assert_eq!(
        arguments(&decoded),
        vec![
            ("mode".to_string(), ArgumentValue::from("NOMINAL")),
            ("delay_s".to_string(), ArgumentValue::Float(1.5)),
        ]
    );
    assert!(decoded.defaulted.is_empty());
    assert_eq!(decoded.target, None);
    assert_eq!(decoded.record.get("check"), Some(&Value::Uint(0x5B7A)));

    let decoded = sets
        .decode_command(
            "vendor_x_v2",
            &framed(&[3, 2, 0x00, 0x03, 0xAA, 0xBB, 0xCC, 0x99, 0xBA]),
        )
        .unwrap();
    assert_eq!(decoded.command(), "eps.load_table");
    assert_eq!(
        arguments(&decoded),
        vec![
            (
                "table".to_string(),
                ArgumentValue::Bytes(vec![0xAA, 0xBB, 0xCC])
            ),
            ("slot".to_string(), ArgumentValue::Uint(2)),
        ]
    );
}

#[test]
fn a_decoded_command_names_its_target_and_its_defaulted_arguments() {
    let sets = sets_of(&fixture(), "legacy");
    let mut payload = vec![1];
    payload.extend_from_slice(b"box-1\0");
    payload.extend_from_slice(&[0x68, 0x10, 0x02]);
    let decoded = sets.decode_command("legacy_v1", &binary(&payload)).unwrap();
    assert_eq!(decoded.target, Some(NativeId::from_text("box-1")));
    assert_eq!(decoded.defaulted, ["delay_s"]);
    assert_eq!(
        arguments(&decoded),
        vec![
            ("mode".to_string(), ArgumentValue::from("NOMINAL")),
            // An f64 default is a number, however the catalog spells it.
            ("delay_s".to_string(), ArgumentValue::Float(0.0)),
        ]
    );
    // The decoded request encodes to the same payload.
    assert_eq!(sets.encode(&decoded.request()).unwrap().payload, payload);
}

#[test]
fn a_json_command_ignores_keys_its_packet_doesnt_define() {
    let sets = sets_of(&fixture(), "vendor_j");
    let text = br#"{"op":"set_mode","args":{"mode":"SAFE","delay":0.5,"speed":9},"extra":true}"#;
    let message = Message::new(Payload::Json(text), Timestamp::UNIX_EPOCH);
    let decoded = sets.decode_command("vendor_j_v1", &message).unwrap();
    assert_eq!(
        arguments(&decoded),
        vec![
            ("mode".to_string(), ArgumentValue::from("SAFE")),
            ("delay_s".to_string(), ArgumentValue::Float(0.5)),
        ]
    );
    let missing = Message::new(
        Payload::Json(br#"{"op":"set_mode","args":{}}"#),
        Timestamp::UNIX_EPOCH,
    );
    assert_eq!(
        decode_code(sets.decode_command("vendor_j_v1", &missing)),
        "missing_value"
    );
}

#[test]
fn commands_are_told_apart_by_text_tags() {
    let sets = sets_of(&lab(), "lab");
    let decoded = sets.decode_command("lab_v6", &binary(b"PONG")).unwrap();
    assert_eq!(decoded.command(), "lab.pong");
    let decoded = sets.decode_command("lab_v6", &binary(b"PING")).unwrap();
    assert_eq!(decoded.command(), "lab.ping");
    assert_eq!(
        decode_code(sets.decode_command("lab_v6", &binary(b"PANG"))),
        "no_packet"
    );
    assert_eq!(
        sets.encode(&CommandRequest::new("lab_v6", "lab.pong"))
            .unwrap()
            .payload,
        b"PONG"
    );
}

#[test]
fn a_payload_the_encoder_wouldnt_write_is_refused() {
    let sets = sets_of(&fixture(), "vendor_x");
    // The ISO checksum of the first four bytes, so only the named fault is
    // wrong.
    let with_check = |body: [u8; 4]| {
        let algorithm =
            crate::v2::checksum::Algorithm::resolve(&crate::v2::ir::Checksum::iso8473()).unwrap();
        let mut payload = body.to_vec();
        payload.extend_from_slice(&(algorithm.compute(&body) as u16).to_be_bytes());
        payload
    };
    let decode = |payload: &[u8]| sets.decode_command("vendor_x_v2", &framed(payload));
    assert_eq!(
        decode_code(sets.decode_command("vendor_x_v9", &binary(&[26]))),
        "unknown_set"
    );
    assert_eq!(decode_code(decode(&with_check([99, 1, 0, 0]))), "no_packet");
    // The packet's metadata identifies it too, so without the link's apid
    // nothing matches.
    assert_eq!(
        decode_code(sets.decode_command("vendor_x_v2", &binary(&with_check([26, 1, 0, 15])))),
        "no_packet"
    );
    // Mode 7 has no state; 40000 counts is 4000 s, past the range.
    assert_eq!(
        decode_code(decode(&with_check([26, 7, 0, 0]))),
        "unknown_state"
    );
    assert_eq!(
        decode_code(decode(&with_check([26, 0, 0x9C, 0x40]))),
        "out_of_range"
    );
    let mut damaged = with_check([26, 1, 0, 15]);
    damaged[3] = 16;
    assert_eq!(decode_code(decode(&damaged)), "bad_checksum");
    assert_eq!(decode_code(decode(&[26, 1])), "past_end");
    let error = decode(&with_check([26, 7, 0, 0])).unwrap_err();
    assert_eq!(
        error.to_string(),
        "command eps.set_mode, argument mode: raw value 7 names none of its states"
    );
}

#[test]
fn every_constant_is_checked_not_only_those_that_identify_the_packet() {
    // A constant after a value of varying length can't identify the
    // packet, but it must still hold its value.
    let mut bundle = lab();
    bundle["sets"][4]["commands"][0]["entries"]
        .as_array_mut()
        .unwrap()
        .push(
            json!({ "kind": "field", "name": "end", "type": "uint", "size_bits": 8,
                      "value": 85 }),
        );
    let sets = sets_of(&bundle, "lab");
    assert!(
        sets.decode_command("lab_v5", &binary(&[2, 7, 7, 85]))
            .is_ok()
    );
    let error = sets
        .decode_command("lab_v5", &binary(&[2, 7, 7, 0]))
        .unwrap_err();
    assert_eq!(error.code(), "wrong_constant");
    assert_eq!(
        error.to_string(),
        "command packet LOAD, end: it holds 0, not its constant 85"
    );
}

#[test]
fn a_value_past_its_range_only_by_rounding_decodes_to_the_ranges_end() {
    // 3600.06 s is 36000.6 counts, written 36001, which reads back as
    // 3600.1 s: past the range, but only because encoding rounded.
    let mut bundle = fixture();
    bundle["catalog"]["commands"]["eps.set_mode"]["arguments"][1]["range"]["max"] = json!(3600.06);
    let sets = sets_of(&bundle, "vendor_x");
    let request = set_mode("vendor_x_v2")
        .with_argument("mode", "SAFE")
        .with_argument("delay_s", 3600.06);
    let encoded = sets.encode(&request).unwrap();
    assert_eq!(encoded.record.get("delay"), Some(&Value::Uint(36001)));
    let decoded = sets
        .decode_command("vendor_x_v2", &framed(&encoded.payload))
        .unwrap();
    assert_eq!(decoded.arguments[1].1, ArgumentValue::Float(3600.06));
    assert_eq!(
        sets.encode(&decoded.request()).unwrap().payload,
        encoded.payload
    );
}

#[test]
fn what_the_encoder_writes_decodes_even_when_the_inverse_isnt_exact() {
    let sets = sets_of(&lab(), "lab");
    let drift = |temp: f64, delay: f64, count: f64, gain: f64| {
        CommandRequest::new("lab_v7", "lab.drift")
            .with_argument("temp", temp)
            .with_argument("delay", delay)
            .with_argument("count", count)
            .with_argument("gain", gain)
    };
    let decoded_arguments = |request: &CommandRequest| {
        let encoded = sets.encode(request).unwrap();
        let decoded = sets
            .decode_command("lab_v7", &binary(&encoded.payload))
            .unwrap();
        assert_eq!(decoded.record, encoded.record);
        decoded
            .arguments
            .iter()
            .map(|(_, value)| match value {
                ArgumentValue::Float(x) => *x,
                other => panic!("a number, not {other:?}"),
            })
            .collect::<Vec<f64>>()
    };
    // 100.03 counts is raw 0.7 + 1000.3, written 1001, which the forward
    // conversion reads as 100.1: past 100.09, but the encoder wrote it for
    // an in-range value, so it decodes, held to the range. Likewise gain
    // 9.999 is raw 5.0005, which reads as 10.001.
    let values = decoded_arguments(&drift(0.0, 0.0, 100.03, 9.999));
    assert_eq!(values[2], 100.09);
    assert_eq!(values[3], 10.0);
    // A negative scale reverses the raw order; 3600.06 s is raw -36001,
    // which reads as 3600.1 s and is held to 3600.06.
    let values = decoded_arguments(&drift(0.0, 3600.06, 0.0, 0.0));
    assert_eq!(values[1], 3600.06);
    // Mid-range, an approximate inverse decodes to what the raw value
    // means: 50 counts is raw 501, which reads as 50.1.
    let values = decoded_arguments(&drift(0.0, 0.0, 50.0, 0.0));
    assert!((values[2] - 50.1).abs() < 1e-9, "{}", values[2]);
    // A raw value no in-range value encodes to is refused: count raw 1003
    // is past what 100.09 encodes to (1002).
    let mut payload = sets.encode(&drift(0.0, 0.0, 0.0, 0.0)).unwrap().payload;
    payload[12..14].copy_from_slice(&1003_u16.to_be_bytes());
    let error = sets
        .decode_command("lab_v7", &binary(&payload))
        .unwrap_err();
    assert_eq!(error.code(), "out_of_range");
    assert_eq!(
        error.to_string(),
        "command lab.drift, argument count: raw value 1003 is outside 1 to 1002, the raw values \
         its range encodes to"
    );
}

/// Each command of the fixture and the lab, by bundle, family and set, and
/// whether its decoded arguments re-encode to the same payload (every
/// converted field's inverse undoes its conversion).
fn command_cases() -> Vec<(Json, &'static str, &'static str, &'static str, bool)> {
    let mut cases = vec![
        (fixture(), "vendor_x", "vendor_x_v2", "eps.set_mode", true),
        (fixture(), "vendor_x", "vendor_x_v2", "eps.load_table", true),
        (fixture(), "vendor_j", "vendor_j_v1", "eps.set_mode", true),
        (fixture(), "legacy", "legacy_v1", "eps.set_mode", true),
    ];
    for (set, command, exact) in [
        ("lab_v1", "lab.tune", true),
        ("lab_v2", "lab.ping", true),
        ("lab_v3", "lab.scale", true),
        ("lab_v4", "lab.blob", true),
        ("lab_v5", "lab.load", true),
        ("lab_v6", "lab.ping", true),
        ("lab_v6", "lab.pong", true),
        ("lab_v7", "lab.drift", false),
    ] {
        cases.push((lab(), "lab", set, command, exact));
    }
    cases.push((lab(), "web", "web_v1", "lab.ping", true));
    cases
}

fn read_ir(bundle: &Json) -> Bundle {
    read_bundle(&serde_json::to_vec(bundle).unwrap())
        .unwrap()
        .value
}

/// Valid requests for a command, built from its definition: each argument
/// the set encodes gets a value its catalog entry and field allow; those
/// it leaves out take their defaults.
fn valid_requests(bundle: &Json, set: &str, command: &str) -> BoxedStrategy<CommandRequest> {
    let ir = read_ir(bundle);
    let family = ir
        .sets
        .iter()
        .find(|s| s.name == set)
        .unwrap()
        .family
        .clone();
    let binary = ir
        .families
        .iter()
        .any(|f| f.name == family && f.encoding == crate::v2::ir::Encoding::Binary);
    let packet = ir
        .sets
        .iter()
        .find(|s| s.name == set)
        .and_then(|s| s.commands.iter().find(|p| p.canonical == command))
        .unwrap()
        .clone();
    let fields: Vec<Field> = packet
        .entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Field(field) => Some(field.clone()),
            _ => None,
        })
        .collect();
    let mut strategies: Vec<BoxedStrategy<(String, ArgumentValue)>> = Vec::new();
    for argument in ir.catalog.commands[command].arguments.clone() {
        let Some(field) = fields
            .iter()
            .find(|field| field.argument.as_deref() == Some(argument.name.as_str()))
        else {
            continue;
        };
        let lengths = byte_lengths(&packet.entries, field, binary);
        let name = argument.name.clone();
        strategies.push(
            value_strategy(&argument, field, lengths)
                .prop_map(move |value| (name.clone(), value))
                .boxed(),
        );
    }
    let target = fields
        .iter()
        .find(|field| field.target == Some(true))
        .map(|field| target_strategy(field, byte_lengths(&packet.entries, field, binary)));
    let set = set.to_string();
    let command = command.to_string();
    (strategies, target.unwrap_or_else(|| Just(None).boxed()))
        .prop_map(move |(arguments, target)| {
            let mut request = CommandRequest::new(set.as_str(), command.as_str());
            request.arguments = arguments;
            request.target = target;
            request
        })
        .boxed()
}

/// The byte lengths a binary string or bytes field can have: its fixed
/// length, or any its length field can hold (up to 300 raw); a short range
/// otherwise.
fn byte_lengths(entries: &[Entry], field: &Field, binary: bool) -> BoxedStrategy<usize> {
    match (&field.length, binary) {
        (Some(ByteLength::Fixed { bytes }), true) => Just(*bytes as usize).boxed(),
        (Some(ByteLength::Field(from)), true) => {
            let multiply = i64::from(from.multiply.unwrap_or(1));
            let add = from.add.unwrap_or(0);
            let size = entries
                .iter()
                .find_map(|entry| match entry {
                    Entry::Field(f) if f.name == from.field => f.size_bits,
                    _ => None,
                })
                .unwrap_or(8);
            let largest = (1_i64 << size.min(16)) - 1;
            (0..=largest.min(300))
                .prop_filter_map("a length of at least 0", move |raw| {
                    usize::try_from(raw * multiply + add).ok()
                })
                .boxed()
        }
        _ => (0_usize..24).boxed(),
    }
}

fn text_strategy(field: &Field, lengths: BoxedStrategy<usize>) -> BoxedStrategy<String> {
    let terminator = match field.length {
        Some(ByteLength::Terminator { byte }) => Some(char::from(byte)),
        _ => None,
    };
    let range = move |low: u32, high: u32| {
        (low..=high).prop_filter_map("a character", move |code| {
            char::from_u32(code).filter(|c| Some(*c) != terminator)
        })
    };
    let fixed = matches!(field.length, Some(ByteLength::Fixed { .. }));
    match field.text {
        Some(TextEncoding::Ascii) => lengths
            .prop_flat_map(move |n| proptest::collection::vec(range(0x20, 0x7E), n))
            .prop_map(String::from_iter)
            .boxed(),
        Some(TextEncoding::Latin1) => lengths
            .prop_flat_map(move |n| proptest::collection::vec(range(0x20, 0xFF), n))
            .prop_map(String::from_iter)
            .boxed(),
        // UTF-8 at a fixed length: some two-byte characters among one-byte
        // ones, so the bytes still add up to the length.
        _ if fixed => lengths
            .prop_flat_map(move |n| {
                (0..=n / 2).prop_flat_map(move |two| {
                    (
                        proptest::collection::vec(range(0x80, 0x7FF), two),
                        proptest::collection::vec(range(0x20, 0x7E), n - 2 * two),
                    )
                        .prop_map(|(wide, narrow)| {
                            wide.into_iter().chain(narrow).collect::<Vec<char>>()
                        })
                        .prop_shuffle()
                })
            })
            .prop_map(String::from_iter)
            .boxed(),
        _ => lengths
            .prop_flat_map(move |n| proptest::collection::vec(range(0x20, 0x2FF), n))
            .prop_map(String::from_iter)
            .boxed(),
    }
}

fn value_strategy(
    argument: &Argument,
    field: &Field,
    lengths: BoxedStrategy<usize>,
) -> BoxedStrategy<ArgumentValue> {
    let range = argument.range.unwrap_or_default();
    match argument.ty {
        ParamType::F64 => {
            let low = range.min.unwrap_or(-1e6);
            let high = range.max.unwrap_or(1e6);
            // The ends, and just inside them, where rounding matters most.
            prop_oneof![
                4 => (low..=high).boxed(),
                1 => Just(low).boxed(),
                1 => Just(high).boxed(),
                1 => Just(low.next_up()).boxed(),
                1 => Just(high.next_down()).boxed(),
            ]
            .prop_map(ArgumentValue::Float)
            .boxed()
        }
        ParamType::I64 | ParamType::U64 => {
            let (type_low, type_high) = if argument.ty == ParamType::U64 {
                (0, i128::from(u64::MAX))
            } else {
                (i128::from(i64::MIN), i128::from(i64::MAX))
            };
            let low = range.min.map_or(type_low, |min| min.ceil() as i128);
            let high = range.max.map_or(type_high, |max| max.floor() as i128);
            let unsigned = argument.ty == ParamType::U64;
            prop_oneof![4 => (low..=high).boxed(), 1 => Just(low).boxed(), 1 => Just(high).boxed()]
                .prop_map(move |n| {
                    if unsigned {
                        ArgumentValue::Uint(n as u64)
                    } else {
                        ArgumentValue::Int(n as i64)
                    }
                })
                .boxed()
        }
        ParamType::Bool => any::<bool>().prop_map(ArgumentValue::Bool).boxed(),
        ParamType::Enum => proptest::sample::select(argument.states.clone())
            .prop_map(ArgumentValue::Text)
            .boxed(),
        ParamType::Str => text_strategy(field, lengths)
            .prop_map(ArgumentValue::Text)
            .boxed(),
        ParamType::Bytes => {
            let terminator = match field.length {
                Some(ByteLength::Terminator { byte }) => Some(byte),
                _ => None,
            };
            lengths
                .prop_flat_map(move |length| {
                    proptest::collection::vec(
                        any::<u8>()
                            .prop_filter("not the terminator", move |b| Some(*b) != terminator),
                        length,
                    )
                })
                .prop_map(ArgumentValue::Bytes)
                .boxed()
        }
        ParamType::Vec3 | ParamType::Quat => Just(ArgumentValue::Bool(false)).boxed(),
    }
}

fn target_strategy(
    field: &Field,
    lengths: BoxedStrategy<usize>,
) -> BoxedStrategy<Option<NativeId>> {
    let size = field.size_bits.unwrap_or(64).clamp(1, 64);
    match field.ty {
        ScalarType::Uint => {
            let high = if size >= 64 {
                u64::MAX
            } else {
                (1 << size) - 1
            };
            (0..=high).prop_map(|n| Some(NativeId::from(n))).boxed()
        }
        ScalarType::Int => {
            let high = (1_i64 << (size - 1)).wrapping_sub(1);
            let low = if size >= 64 {
                i64::MIN
            } else {
                -(1_i64 << (size - 1))
            };
            let high = if size >= 64 { i64::MAX } else { high };
            (low..=high).prop_map(|n| Some(NativeId::from(n))).boxed()
        }
        _ => text_strategy(field, lengths)
            .prop_map(|text| Some(NativeId::from_text(text)))
            .boxed(),
    }
}

/// Two argument values are the same value: numbers by value.
fn same(a: &ArgumentValue, b: &ArgumentValue) -> bool {
    let integer = |value: &ArgumentValue| match value {
        ArgumentValue::Int(n) => Some(i128::from(*n)),
        ArgumentValue::Uint(n) => Some(i128::from(*n)),
        _ => None,
    };
    match (integer(a), integer(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

fn payload_of<'p>(family: &str, bytes: &'p [u8]) -> Payload<'p> {
    if family == "vendor_j" || family == "web" {
        Payload::Json(bytes)
    } else {
        Payload::Binary(bytes)
    }
}

#[test]
fn every_command_round_trips_through_its_payload() {
    use proptest::test_runner::{Config, TestRunner};
    for (bundle, family, set, command, exact_round_trip) in command_cases() {
        let sets = sets_of(&bundle, family);
        let exact: Vec<String> = read_ir(&bundle).catalog.commands[command]
            .arguments
            .iter()
            .filter(|argument| argument.ty != ParamType::F64)
            .map(|argument| argument.name.clone())
            .collect();
        let mut runner = TestRunner::new(Config::with_cases(128));
        let result = runner.run(&valid_requests(&bundle, set, command), |request| {
            let encoded = sets
                .encode(&request)
                .map_err(|error| TestCaseError::fail(format!("encode {request:?}: {error}")))?;
            let metadata = encoded.metadata();
            let message = Message::new(payload_of(family, &encoded.payload), Timestamp::UNIX_EPOCH)
                .with_metadata(&metadata);
            // Whatever the encoder writes decodes.
            let decoded = sets
                .decode_command(set, &message)
                .map_err(|error| TestCaseError::fail(format!("decode {request:?}: {error}")))?;
            prop_assert_eq!(decoded.command(), command);
            prop_assert_eq!(&decoded.record, &encoded.record);
            prop_assert_eq!(&decoded.target, &request.target);
            // Arguments that aren't converted come back exactly.
            for (name, given) in &request.arguments {
                if exact.contains(name) {
                    let back = decoded
                        .arguments
                        .iter()
                        .find(|(n, _)| n == name)
                        .map(|(_, v)| v);
                    prop_assert!(
                        back.is_some_and(|back| same(back, given)),
                        "{} {:?} {:?}",
                        name,
                        given,
                        back
                    );
                }
            }
            // The decoded arguments are a valid request, and where every
            // inverse is exact it encodes to the same payload.
            let again = sets
                .encode(&decoded.request())
                .map_err(|error| TestCaseError::fail(format!("encode again: {error}")))?;
            if exact_round_trip {
                prop_assert_eq!(again.payload, encoded.payload);
            }
            Ok(())
        });
        if let Err(error) = result {
            panic!("{set} {command}: {error}");
        }
    }
}

proptest! {
    #[test]
    fn any_payload_decodes_or_is_refused_without_panicking(
        case in 0_usize..13,
        bytes in proptest::collection::vec(any::<u8>(), 0..48),
        framed_with_apid in any::<bool>(),
        mode in prop_oneof![Just(json!("SAFE")), Just(json!("NOMINAL")), Just(json!("X")), Just(json!(1))],
        delay in prop_oneof![any::<f64>().prop_filter("finite", |x| x.is_finite()).prop_map(|x| json!(x)), Just(json!("soon")), Just(json!(null))],
    ) {
        let cases = command_cases();
        let (bundle, family, set, _, _) = &cases[case];
        let sets = sets_of(bundle, family);
        let text;
        let payload = if *family == "vendor_j" {
            text = serde_json::to_vec(&json!({ "op": "set_mode", "args": { "mode": mode, "delay": delay } })).unwrap();
            Payload::Json(&text)
        } else {
            payload_of(family, &bytes)
        };
        let mut message = Message::new(payload, Timestamp::UNIX_EPOCH);
        if framed_with_apid {
            message = message.with_metadata(APID);
        }
        match sets.decode_command(set, &message) {
            Ok(decoded) => {
                // What decodes is a command the catalog allows.
                let again = sets.encode(&decoded.request());
                prop_assert!(again.is_ok(), "{:?}: {:?}", decoded.request(), again);
            }
            Err(error) => prop_assert_ne!(error.code(), "invalid_layout", "{}", error),
        }
    }
}
