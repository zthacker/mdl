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

#[test]
fn a_raw_value_that_doesnt_fit_its_field_is_refused() {
    // Through the polynomial's inverse, 7000 s is 70000 counts, past a u16.
    let mut bundle = fixture();
    bundle["catalog"]["commands"]["eps.set_mode"]["arguments"][1]["range"]["max"] = json!(7000);
    let sets = sets_of(&bundle, "vendor_x");
    let request = set_mode("vendor_x_v2")
        .with_argument("mode", "SAFE")
        .with_argument("delay_s", 7000);
    let error = sets.encode(&request).unwrap_err();
    assert_eq!(error.code(), "raw_range");
    assert_eq!(
        error.to_string(),
        "command packet SET_MODE, delay: raw value 70000 doesn't fit; the field holds 0 to 65535"
    );
}

/// Layouts the fixture doesn't have. Family `lab`: a fixed-length ASCII
/// name, a binary32, padding and a 12-bit signed field (lab_v1); a command
/// with no payload (lab_v2); linear and polynomial inverses (lab_v3); an
/// empty value read to the end after a gap (lab_v4); a length stored as
/// (n + 2) / 2 (lab_v5). Family `web`: a JSON command with no fields.
fn lab() -> Json {
    let bytes = |name: &str| json!({ "arguments": [{ "name": name, "type": "bytes" }] });
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
            "lab.load": bytes("data")
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
                      "argument": "trim" }
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
            .with_argument("trim", trim);
        sets.encode(&request).unwrap().payload
    };
    // (0.15 - 0) / 0.1 is 1.4999999999999998, not 1.5 as 0.15 * (1 / 0.1)
    // would give; -0.1 + 10 * 0.36 is 3.4999999999999996, not 3.5 as a
    // fused multiply-add would give.
    assert_eq!(scale(0.15, 0.36), [1, 3]);
    assert_eq!(scale(0.25, 0.0), [3, 0]);
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
