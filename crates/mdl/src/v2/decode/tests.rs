use proptest::prelude::*;
use serde_json::json;

use super::*;
use crate::v2::artifact::{Artifacts, Written, build};
use crate::v2::bindings::Bindings;
use crate::v2::ir::Artifact;
use crate::v2::message::Payload;
use crate::v2::read::{Loaded, read_artifact, read_bundle};
use crate::v2::time::Timestamp;
use crate::v2::validate::validate;

const FULL_BUNDLE: &str = include_str!("../../../tests/fixtures/v2/full_bundle.json");

/// The fixture plus a family for identification: a fixed alias and four
/// packets keyed by their own fields.
fn bundle() -> Json {
    let mut value: Json = serde_json::from_str(FULL_BUNDLE).unwrap();
    // BUS also reports a temperature, but not always.
    value["sets"][1]["packets"][0]["entries"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "kind": "field", "name": "temp", "type": "float",
                      "pointer": "/temp", "optional": true }));
    // The legacy boot clock reads all ones until it's synced.
    value["sets"][2]["packets"][0]["entries"][1]["invalid"] = json!([4294967295_u32]);
    // And, when it has them, bus current at 100 Hz.
    value["sets"][1]["packets"][0]["entries"]
        .as_array_mut()
        .unwrap()
        .push(
            json!({ "kind": "array", "name": "currents", "pointer": "/i", "optional": true,
                      "element": { "kind": "scalar", "type": "float", "unit": "A" },
                      "canonical": "eps.bus_current", "sample_interval_ms": 10.0 }),
        );
    let field =
        |name: &str| json!({ "kind": "field", "name": name, "type": "uint", "size_bits": 8 });
    value["families"].as_array_mut().unwrap().push(json!({
        "name": "ident",
        "encoding": "binary",
        "byte_order": "big",
        "alias": { "kind": "fixed", "value": "x" }
    }));
    value["sets"].as_array_mut().unwrap().push(json!({
        "name": "ident_v1",
        "family": "ident",
        "packets": [
            { "name": "GENERIC", "entries": [field("kind")] },
            { "name": "TYPE1", "identify": { "kind": 1 }, "entries": [field("kind")] },
            {
                "name": "TYPE1_SUB2",
                "identify": { "kind": 1, "sub": 2 },
                "entries": [field("kind"), field("sub")]
            },
            {
                "name": "LATE",
                "identify": { "kind": 3, "late": 7 },
                "entries": [
                    field("kind"),
                    { "kind": "field", "name": "name", "type": "string",
                      "length": { "kind": "terminator", "byte": 0 } },
                    field("late"),
                    { "kind": "field", "name": "body", "type": "bytes",
                      "length": { "kind": "fixed", "bytes": 4 } }
                ]
            },
            {
                "name": "CHECKED",
                "identify": { "kind": 6 },
                "entries": [
                    field("kind"),
                    { "kind": "field", "name": "data", "type": "uint", "size_bits": 16 },
                    { "kind": "checksum", "name": "crc",
                      "algorithm": { "kind": "crc", "width": 16, "poly": 4129, "init": 65535,
                                     "reflect_in": false, "reflect_out": false, "xor_out": 0 } }
                ]
            },
            {
                "name": "TEXTED",
                "identify": { "kind": 8 },
                "entries": [
                    field("kind"),
                    { "kind": "field", "name": "name", "type": "string",
                      "length": { "kind": "fixed", "bytes": 4 } },
                    { "kind": "checksum", "name": "crc",
                      "algorithm": { "kind": "crc", "width": 16, "poly": 4129, "init": 65535,
                                     "reflect_in": false, "reflect_out": false, "xor_out": 0 } }
                ]
            },
            {
                "name": "ISO",
                "identify": { "kind": 20 },
                "entries": [
                    field("kind"),
                    { "kind": "field", "name": "body", "type": "bytes",
                      "length": { "kind": "fixed", "bytes": 5 } },
                    { "kind": "checksum", "name": "check", "algorithm": { "kind": "iso8473" } }
                ]
            },
            {
                "name": "SUMMED",
                "identify": { "kind": 7 },
                "entries": [
                    field("kind"),
                    field("data"),
                    { "kind": "checksum", "name": "sum", "from_bits": 8,
                      "algorithm": { "kind": "sum", "width": 8 } }
                ]
            },
            {
                "name": "NESTED",
                "identify": { "kind": 5 },
                "entries": [
                    field("kind"),
                    { "kind": "struct", "name": "pair", "struct": "pair" },
                    { "kind": "array", "name": "pairs", "count": { "kind": "fixed", "count": 2 },
                      "element": { "kind": "struct", "name": "pair" } }
                ]
            }
        ],
        "types": {
            "pair": { "entries": [
                field("len"),
                { "kind": "field", "name": "data", "type": "bytes",
                  "length": { "kind": "field", "field": "len" } }
            ] }
        }
    }));
    value
}

fn artifacts() -> Artifacts {
    artifacts_of(&bundle())
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

fn registry() -> Bindings {
    Bindings::from_toml(
        r#"
mdl_version = "2.0"

[[binding]]
family = "vendor_x"
native_id = 101
asset = "SAT-1"
effective_from = "2026-01-01T00:00:00Z"

[[binding]]
family = "vendor_j"
native_id = "PX-9"
asset = "SAT-57"
set = "vendor_j_v1"
effective_from = "2026-01-01T00:00:00Z"

[[binding]]
family = "legacy"
native_id = "box-1"
asset = "BOX-1"
set = "legacy_v1"
effective_from = "2026-01-01T00:00:00Z"

[[binding]]
family = "ident"
native_id = "x"
asset = "X-1"
set = "ident_v1"
effective_from = "2026-01-01T00:00:00Z"
"#,
    )
    .unwrap()
}

fn now() -> Timestamp {
    "2026-06-01T00:00:00Z".parse().unwrap()
}

const HK_METADATA: &[(&str, MetaValue)] =
    &[("apid", MetaValue::Uint(100)), ("fmt", MetaValue::Uint(2))];

/// A vendor_x HK payload: CUC time (6 bytes), alias 101, bus counts 742,
/// mode 1, then the variable part.
struct Hk<'t> {
    flags: u8,
    samples: &'t [i16],
    pos: Option<[i32; 3]>,
    label: &'t [u8],
    blobs: &'t [&'t [u8]],
}

impl Hk<'_> {
    fn bytes(&self) -> Vec<u8> {
        let mut out = vec![0; 6];
        out.extend_from_slice(&101_u16.to_be_bytes());
        out.extend_from_slice(&742_u16.to_be_bytes());
        out.push(0b0000_0001);
        out.push(self.flags);
        out.push(u8::try_from(self.samples.len()).unwrap());
        for sample in self.samples {
            out.extend_from_slice(&sample.to_be_bytes());
        }
        if let Some(pos) = self.pos {
            for axis in pos {
                out.extend_from_slice(&axis.to_be_bytes());
            }
        }
        out.push(u8::try_from(self.label.len()).unwrap());
        out.extend_from_slice(self.label);
        for blob in self.blobs {
            out.extend_from_slice(blob);
            out.push(0);
        }
        out
    }
}

fn decode_hk(payload: &[u8]) -> Result<Vec<(String, String)>, DecodeError> {
    let artifacts = artifacts();
    let sets = family_sets(&artifacts, "vendor_x");
    let message = Message::new(Payload::Binary(payload), now()).with_metadata(HK_METADATA);
    sets.decode(&message, &registry())
        .map(|decoded| shown(&decoded.record))
}

/// A record as (name, value) text pairs, for readable assertions.
fn shown(record: &Record<'_>) -> Vec<(String, String)> {
    record
        .iter()
        .map(|(name, value)| (name.to_string(), show(value)))
        .collect()
}

fn show(value: &Value<'_>) -> String {
    match value {
        Value::Uint(n) => n.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(x) => x.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Str(text) => format!("{text:?}"),
        Value::Bytes(bytes) => format!("{bytes:?}"),
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(show).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Record(record) => {
            let entries: Vec<String> = record
                .iter()
                .map(|(name, value)| format!("{name}: {}", show(value)))
                .collect();
            format!("{{{}}}", entries.join(", "))
        }
        Value::Null => "null".to_string(),
    }
}

fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
    expected
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

fn code<T: std::fmt::Debug>(result: Result<T, DecodeError>) -> &'static str {
    match result {
        Ok(decoded) => panic!("expected an error, decoded {decoded:?}"),
        Err(err) => err.code(),
    }
}

#[test]
fn a_binary_packet_decodes_every_entry() {
    let artifacts = artifacts();
    let sets = family_sets(&artifacts, "vendor_x");
    let payload = Hk {
        flags: 3,
        samples: &[100, -5],
        pos: Some([1, 2, -3]),
        label: b"ABC",
        blobs: &[b"hi", b"x"],
    }
    .bytes();
    let message = Message::new(Payload::Binary(&payload), now()).with_metadata(HK_METADATA);
    let decoded = sets.decode(&message, &registry()).unwrap();
    assert_eq!(decoded.packet.name, "HK");
    assert_eq!(decoded.route.binding.asset, "SAT-1");
    assert_eq!(
        decoded.route.def_version,
        &artifacts.sets["vendor_x_v2"].digest
    );
    assert_eq!(
        shown(&decoded.record),
        pairs(&[
            ("bus_counts", "742"),
            ("mode", "1"),
            ("flags", "3"),
            ("n", "2"),
            ("samples", "[100, -5]"),
            ("pos", "{x: 1, y: 2, z: -3}"),
            ("label_len", "3"),
            ("label", "\"ABC\""),
            ("blobs", "[[104, 105], [120]]"),
        ])
    );
}

#[test]
fn absent_entries_take_no_room() {
    // flags 0: no position (mask) and no label (flags not 2 or 3), so the
    // blobs start right after label_len.
    let payload = Hk {
        flags: 0,
        samples: &[],
        pos: None,
        label: b"",
        blobs: &[b"z"],
    }
    .bytes();
    assert_eq!(
        decode_hk(&payload).unwrap(),
        pairs(&[
            ("bus_counts", "742"),
            ("mode", "1"),
            ("flags", "0"),
            ("n", "0"),
            ("samples", "[]"),
            ("label_len", "0"),
            ("blobs", "[[122]]"),
        ])
    );
}

#[test]
fn a_binary_packet_that_cant_be_decoded_is_rejected_with_the_entry() {
    let full = Hk {
        flags: 3,
        samples: &[1],
        pos: Some([0, 0, 0]),
        label: b"ABC",
        blobs: &[b"hi"],
    }
    .bytes();

    // At least 12 bytes; the alias (bytes 6 and 7) still routes.
    assert_eq!(code(decode_hk(&full[..11])), "too_short");

    // A count far beyond the bytes left is refused before allocating.
    let mut huge = full.clone();
    huge[12] = 200;
    let err = decode_hk(&huge).unwrap_err();
    assert_eq!(err.code(), "past_end");
    assert!(err.to_string().starts_with("packet HK, samples: "), "{err}");

    let mut unterminated = full.clone();
    unterminated.pop();
    let err = decode_hk(&unterminated).unwrap_err();
    assert_eq!(
        err.to_string(),
        "packet HK, blobs[0]: no 0x00 byte ends it before the packet ends"
    );

    let mut not_ascii = full.clone();
    let label_at = not_ascii.len() - 6;
    not_ascii[label_at] = 0xC3;
    let err = decode_hk(&not_ascii).unwrap_err();
    assert_eq!(err.code(), "invalid_text");
    assert!(err.to_string().starts_with("packet HK, label: "), "{err}");

    // No packet of the set has apid 101.
    let artifacts = artifacts();
    let sets = family_sets(&artifacts, "vendor_x");
    let other: &[(&str, MetaValue)] =
        &[("apid", MetaValue::Uint(101)), ("fmt", MetaValue::Uint(2))];
    let message = Message::new(Payload::Binary(&full), now()).with_metadata(other);
    assert_eq!(code(sets.decode(&message, &registry())), "no_packet");
    // Routing errors come through as they are.
    let message = Message::new(Payload::Binary(&full), now());
    assert_eq!(
        code(sets.decode(&message, &registry())),
        "unreadable_locator"
    );
}

fn decode_json(text: &str) -> Result<Vec<(String, String)>, DecodeError> {
    let artifacts = artifacts();
    let sets = family_sets(&artifacts, "vendor_j");
    let message = Message::new(Payload::Json(text.as_bytes()), now());
    sets.decode(&message, &registry())
        .map(|decoded| shown(&decoded.record))
}

#[test]
fn a_json_packet_decodes_by_pointer() {
    let text = r#"{"ch": "PWR.BUS.V", "sat": "PX-9", "t": "2026-06-01T00:00:00Z",
                   "v": 28000, "cells": [{"v": 3.7, "ok": true}, {"v": 3.5, "ok": false}]}"#;
    assert_eq!(
        decode_json(text).unwrap(),
        pairs(&[
            ("v", "28000"),
            ("cells", "[{volts: 3.7, ok: true}, {volts: 3.5, ok: false}]"),
        ])
    );
    // 28000.0 is the integer 28000.
    let text = r#"{"ch": "PWR.BUS.V", "sat": "PX-9", "t": "2026-06-01T00:00:00Z",
                   "v": 28000.0, "cells": []}"#;
    assert_eq!(decode_json(text).unwrap()[0], ("v".into(), "28000".into()));
}

#[test]
fn a_json_packet_that_cant_be_decoded_is_rejected_with_the_entry() {
    let with = |v: &str, cells: &str| {
        format!(r#"{{"ch": "PWR.BUS.V", "sat": "PX-9", "v": {v}, "cells": {cells}}}"#)
    };
    assert_eq!(
        code(decode_json(r#"{"ch": "OTHER", "sat": "PX-9"}"#)),
        "no_packet"
    );
    let missing = r#"{"ch": "PWR.BUS.V", "sat": "PX-9", "v": 1}"#;
    let err = decode_json(missing).unwrap_err();
    assert_eq!(err.to_string(), "packet BUS, cells: no value at /cells");
    assert_eq!(code(decode_json(&with("\"1\"", "[]"))), "wrong_type");
    assert_eq!(code(decode_json(&with("1.5", "[]"))), "out_of_range");
    assert_eq!(code(decode_json(&with("1e30", "[]"))), "out_of_range");
    let err = decode_json(&with(
        "1",
        r#"[{"v": 1, "ok": true}, {"v": 1, "ok": "yes"}]"#,
    ))
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "packet BUS, cells[1].ok: expected a boolean, found a string"
    );
    assert_eq!(code(decode_json(&with("1", "{}"))), "wrong_type");
}

#[test]
fn a_little_endian_packet_with_an_exact_length() {
    let artifacts = artifacts();
    let sets = family_sets(&artifacts, "legacy");
    let mut payload = 1_700_000_000_000_000_u64.to_le_bytes().to_vec();
    payload.extend_from_slice(&1234_u32.to_le_bytes());
    let decode = |payload: &[u8]| {
        sets.decode(&Message::new(Payload::Binary(payload), now()), &registry())
            .map(|decoded| shown(&decoded.record))
    };
    let expected = pairs(&[("unix_us", "1700000000000000"), ("boot_ms", "1234")]);
    assert_eq!(decode(&payload).unwrap(), expected);
    // A longer payload is accepted: only the exact length is decoded.
    let mut longer = payload.clone();
    longer.extend_from_slice(&[0xFF; 4]);
    assert_eq!(decode(&longer).unwrap(), expected);
    assert_eq!(code(decode(&payload[..11])), "too_short");
}

fn identified(payload: &[u8]) -> Result<(String, Vec<(String, String)>), DecodeError> {
    identified_in(&bundle(), payload)
}

/// Decodes a payload of the `ident` family, from this bundle.
fn identified_in(
    bundle: &Json,
    payload: &[u8],
) -> Result<(String, Vec<(String, String)>), DecodeError> {
    let artifacts = artifacts_of(bundle);
    let sets = family_sets(&artifacts, "ident");
    let message = Message::new(Payload::Binary(payload), now());
    sets.decode(&message, &registry())
        .map(|decoded| (decoded.packet.name.clone(), shown(&decoded.record)))
}

#[test]
fn the_most_specific_matching_packet_wins() {
    let name = |payload: &[u8]| identified(payload).unwrap().0;
    assert_eq!(name(&[1, 2]), "TYPE1_SUB2");
    assert_eq!(name(&[1, 5]), "TYPE1");
    assert_eq!(name(&[9]), "GENERIC");
    // TYPE1_SUB2's sub can't be read from one byte, so it doesn't match.
    assert_eq!(name(&[1]), "TYPE1");
}

#[test]
fn a_key_without_a_static_position_is_read_by_decoding() {
    assert_eq!(
        identified(&[3, b'a', 0, 7, 1, 2, 3, 4]).unwrap(),
        (
            "LATE".to_string(),
            pairs(&[
                ("kind", "3"),
                ("name", "\"a\""),
                ("late", "7"),
                ("body", "[1, 2, 3, 4]")
            ])
        )
    );
    assert_eq!(
        identified(&[3, b'a', 0, 8, 1, 2, 3, 4]).unwrap().0,
        "GENERIC"
    );
    // LATE's key can't be read (no terminator before it), so it doesn't
    // match; the catch-all does.
    assert_eq!(identified(&[3, b'a']).unwrap().0, "GENERIC");
    // The catch-all matches anything, but this one is too short for it.
    let err = identified(&[]).unwrap_err();
    assert_eq!(
        err.to_string(),
        "packet GENERIC, kind: it needs the packet's bits up to 8, but the packet has 0"
    );
}

#[test]
fn a_packet_cut_short_after_its_key_is_an_error_not_a_fallback() {
    // LATE's key reads 7, so the message is LATE; its body is cut short.
    let err = identified(&[3, b'a', 0, 7, 1]).unwrap_err();
    assert_eq!(err.code(), "past_end");
    assert!(err.to_string().starts_with("packet LATE, body: "), "{err}");
}

#[test]
fn a_cheap_key_that_matches_nothing_skips_trial_decodes() {
    // 200 packets keyed by a static `kind` and a field that needs a trial
    // decode, like LATE.
    let identities = (0..200)
        .map(|packet| {
            BTreeMap::from([
                (
                    KeyId::Bits {
                        offset: 0,
                        size: 8,
                        signed: false,
                    },
                    KeyValue::Int(packet as i128),
                ),
                (
                    KeyId::Unplaced {
                        packet,
                        name: "late".to_string(),
                    },
                    KeyValue::Int(7),
                ),
            ])
        })
        .collect();
    let index = Index::new(identities);
    let trials = |kind: i128| {
        let mut count = 0;
        let found = index.find(&mut |key| match key {
            KeyId::Unplaced { .. } => {
                count += 1;
                Some(KeyValue::Int(7))
            }
            _ => Some(KeyValue::Int(kind)),
        });
        (found, count)
    };
    assert_eq!(trials(250), (None, 0));
    assert_eq!(trials(5), (Some(5), 1));
}

#[test]
fn references_are_found_in_their_own_container() {
    // Each pair's data length comes from that pair's own len.
    let (name, record) = identified(&[5, 2, 0xAA, 0xBB, 1, 0xCC, 0]).unwrap();
    assert_eq!(name, "NESTED");
    assert_eq!(
        record,
        pairs(&[
            ("kind", "5"),
            ("pair", "{len: 2, data: [170, 187]}"),
            ("pairs", "[{len: 1, data: [204]}, {len: 0, data: []}]"),
        ])
    );
}

/// Every family's sets and the registry, built once for the property tests.
fn shared() -> &'static (BTreeMap<&'static str, FamilySets>, Bindings) {
    static SHARED: std::sync::OnceLock<(BTreeMap<&'static str, FamilySets>, Bindings)> =
        std::sync::OnceLock::new();
    SHARED.get_or_init(|| {
        let artifacts = artifacts();
        let families = ["vendor_x", "vendor_j", "legacy", "ident"]
            .into_iter()
            .map(|family| (family, family_sets(&artifacts, family)))
            .collect();
        (families, registry())
    })
}

proptest! {
    #[test]
    fn decoding_any_bytes_never_panics(payload in proptest::collection::vec(any::<u8>(), 0..64)) {
        let (families, registry) = shared();
        for (family, metadata) in [("vendor_x", HK_METADATA), ("legacy", &[][..]), ("ident", &[][..])] {
            let message = Message::new(Payload::Binary(&payload), now()).with_metadata(metadata);
            let _ = families[family].decode(&message, registry);
        }
    }

    #[test]
    fn decoding_any_text_never_panics(text in "\\PC{0,48}") {
        let (families, registry) = shared();
        let wrapped = format!(r#"{{"ch": "PWR.BUS.V", "sat": "PX-9", "v": 1, "cells": [{text}]}}"#);
        for payload in [text.as_bytes(), wrapped.as_bytes()] {
            let message = Message::new(Payload::Json(payload), now());
            let _ = families["vendor_j"].decode(&message, registry);
        }
    }
}

fn hk_decoded(hk: &Hk<'_>) -> Vec<(String, String, String)> {
    let artifacts = artifacts();
    let sets = family_sets(&artifacts, "vendor_x");
    let payload = hk.bytes();
    let message = Message::new(Payload::Binary(&payload), now()).with_metadata(HK_METADATA);
    let decoded = sets.decode(&message, &registry()).unwrap();
    decoded.samples.iter().map(sample_text).collect()
}

/// A sample as (param, value and where it's from, quality).
fn sample_text(sample: &Sample<'_>) -> (String, String, String) {
    let mut place = sample.path.clone();
    if let Some(offset) = sample.time_offset_ns {
        place.push_str(&format!(" +{offset}ns"));
    }
    (
        sample.param.to_string(),
        format!("{:?} at {place}", sample.value),
        sample.quality.as_str().to_string(),
    )
}

fn triples(expected: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    expected
        .iter()
        .map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string()))
        .collect()
}

#[test]
fn mapped_values_become_canonical_samples() {
    let samples = hk_decoded(&Hk {
        flags: 1,
        samples: &[100, -5],
        pos: Some([1, 2, -3]),
        label: b"",
        blobs: &[],
    });
    assert_eq!(
        samples,
        triples(&[
            ("eps.bus_voltage", "F64(7.42) at bus_counts", "good"),
            ("eps.mode", "Enum(\"NOMINAL\") at mode", "good"),
            ("eps.bus_current", "F64(0.1) at samples[0] +0ns", "good"),
            (
                "eps.bus_current",
                "F64(-0.005) at samples[1] +100000000ns",
                "good"
            ),
            (
                "nav.pos_ecef",
                "Vec3 { frame: EciJ2000, x: 1.0, y: 2.0, z: -3.0 } at pos.x,pos.y,pos.z",
                "good"
            ),
        ])
    );
}

#[test]
fn bad_values_are_samples_with_a_quality_and_their_raw_value() {
    let mut payload = Hk {
        flags: 0,
        samples: &[],
        pos: None,
        label: b"",
        blobs: &[],
    }
    .bytes();
    // bus_counts 65535 is "no reading"; mode 2 is no state.
    payload[8..10].copy_from_slice(&65535_u16.to_be_bytes());
    payload[10] = 0b0000_0010;
    let artifacts = artifacts();
    let sets = family_sets(&artifacts, "vendor_x");
    let message = Message::new(Payload::Binary(&payload), now()).with_metadata(HK_METADATA);
    let decoded = sets.decode(&message, &registry()).unwrap();
    // No position, so no composite either.
    assert_eq!(
        decoded.samples.iter().map(sample_text).collect::<Vec<_>>(),
        triples(&[
            ("eps.bus_voltage", "U64(65535) at bus_counts", "invalid"),
            ("eps.mode", "U64(2) at mode", "unknown_state"),
        ])
    );
}

#[test]
fn engineering_values_are_there_for_every_field() {
    let artifacts = artifacts();
    let sets = family_sets(&artifacts, "vendor_x");
    let payload = Hk {
        flags: 3,
        samples: &[100],
        pos: Some([1, 2, -3]),
        label: b"AB",
        blobs: &[b"z"],
    }
    .bytes();
    let message = Message::new(Payload::Binary(&payload), now()).with_metadata(HK_METADATA);
    let decoded = sets.decode(&message, &registry()).unwrap();
    let value = |path: &str| decoded.engineering(path).map(|e| format!("{:?}", e.value));
    assert_eq!(value("bus_counts").as_deref(), Some("F64(7.42)"));
    assert_eq!(value("mode").as_deref(), Some("Enum(\"NOMINAL\")"));
    assert_eq!(value("samples[0]").as_deref(), Some("F64(0.1)"));
    assert_eq!(value("flags").as_deref(), Some("U64(3)"));
    assert_eq!(value("pos.y").as_deref(), Some("I64(2)"));
    assert_eq!(value("label").as_deref(), Some("Str(\"AB\")"));
    assert_eq!(value("blobs[0]").as_deref(), Some("Bytes([122])"));
    for nothing in [
        "samples",
        "pos",
        "nope",
        "samples[9]",
        "samples[01]",
        "pos..x",
    ] {
        assert_eq!(value(nothing), None, "{nothing}");
    }
    let paths: Vec<String> = decoded
        .engineering_values()
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    assert_eq!(
        paths,
        [
            "bus_counts",
            "mode",
            "flags",
            "n",
            "samples[0]",
            "pos.x",
            "pos.y",
            "pos.z",
            "label_len",
            "label",
            "blobs[0]"
        ]
    );
}

#[test]
fn an_optional_json_member_may_be_missing_or_null() {
    let decode = |extra: &str| {
        decode_json(&format!(
            r#"{{"ch": "PWR.BUS.V", "sat": "PX-9", "t": "2026-06-01T00:00:00Z", "v": 1,
                 "cells": []{extra}}}"#
        ))
    };
    let names = |decoded: Vec<(String, String)>| -> Vec<String> {
        decoded.into_iter().map(|(name, _)| name).collect()
    };
    assert_eq!(names(decode("").unwrap()), ["v", "cells"]);
    assert_eq!(names(decode(r#", "temp": null"#).unwrap()), ["v", "cells"]);
    assert_eq!(
        decode(r#", "temp": 21.5"#).unwrap()[2],
        ("temp".to_string(), "21.5".to_string())
    );
    // A required member that's null is missing.
    let err =
        decode_json(r#"{"ch": "PWR.BUS.V", "sat": "PX-9", "v": null, "cells": []}"#).unwrap_err();
    assert_eq!(err.to_string(), "packet BUS, v: no value at /v");
}

#[test]
fn a_json_value_maps_through_its_conversion() {
    let artifacts = artifacts();
    let sets = family_sets(&artifacts, "vendor_j");
    let text = br#"{"ch": "PWR.BUS.V", "sat": "PX-9", "t": "2026-06-01T00:00:00Z",
                    "v": 28000, "cells": []}"#;
    let decoded = sets
        .decode(&Message::new(Payload::Json(text), now()), &registry())
        .unwrap();
    assert_eq!(
        decoded.samples.iter().map(sample_text).collect::<Vec<_>>(),
        triples(&[("eps.bus_voltage", "F64(28.0) at v", "good")])
    );
}

fn instant(text: &str) -> Timestamp {
    text.parse().unwrap()
}

#[test]
fn a_cuc_time_code_is_seconds_and_a_binary_fraction_from_its_epoch() {
    let artifacts = artifacts();
    let sets = family_sets(&artifacts, "vendor_x");
    let hk = Hk {
        flags: 0,
        samples: &[],
        pos: None,
        label: b"",
        blobs: &[],
    }
    .bytes();
    let time_of = |coarse: u32, fine: u16| {
        let mut payload = hk.clone();
        payload[0..4].copy_from_slice(&coarse.to_be_bytes());
        payload[4..6].copy_from_slice(&fine.to_be_bytes());
        let message = Message::new(Payload::Binary(&payload), now()).with_metadata(HK_METADATA);
        sets.decode(&message, &registry()).unwrap().event_time
    };
    assert_eq!(
        time_of(0, 0),
        EventTime::At(instant("2000-01-01T00:00:00Z"))
    );
    assert_eq!(
        time_of(86_400, 0x8000),
        EventTime::At(instant("2000-01-02T00:00:00.5Z"))
    );
    // 1/65536 of a second is 15258.789... ns, truncated.
    assert_eq!(
        time_of(0, 1),
        EventTime::At(instant("2000-01-01T00:00:00.000015258Z"))
    );
}

#[test]
fn an_iso8601_time_is_any_rfc3339_instant_in_the_payload() {
    let artifacts = artifacts();
    let sets = family_sets(&artifacts, "vendor_j");
    let decode = |t: &str| {
        let text = format!(r#"{{"ch": "PWR.BUS.V", "sat": "PX-9", {t} "v": 1, "cells": []}}"#);
        sets.decode(
            &Message::new(Payload::Json(text.as_bytes()), now()),
            &registry(),
        )
        .map(|decoded| decoded.event_time)
    };
    assert_eq!(
        decode(r#""t": "2026-06-01T14:00:00.25+02:00","#),
        Ok(EventTime::At(instant("2026-06-01T12:00:00.25Z")))
    );
    assert_eq!(code(decode("")), "invalid_time");
    let err = decode(r#""t": "noon","#).unwrap_err();
    assert_eq!(err.code(), "invalid_time");
    assert!(
        err.to_string()
            .starts_with("packet BUS: its event time can't be read: "),
        "{err}"
    );
}

fn time_packet(unix_us: u64, boot_ms: u32) -> Vec<u8> {
    let mut payload = unix_us.to_le_bytes().to_vec();
    payload.extend_from_slice(&boot_ms.to_le_bytes());
    payload
}

#[test]
fn boot_time_comes_back_raw_and_a_correlator_makes_it_absolute() {
    let artifacts = artifacts();
    let legacy = family_sets(&artifacts, "legacy");
    let registry = registry();
    let mut correlator = Correlator::new();

    // 2023-11-14T22:13:20Z, at 1.234 s after boot.
    let correlation = time_packet(1_700_000_000_000_000, 1234);
    let later = time_packet(u64::MAX, 2234);
    let decode = |payload: &[u8]| {
        legacy
            .decode(&Message::new(Payload::Binary(payload), now()), &registry)
            .unwrap()
    };
    let first = decode(&correlation);
    let second = decode(&later);
    assert_eq!(
        second.event_time,
        EventTime::SinceBoot {
            value: 2234,
            unit: crate::v2::ir::TimeUnit::Ms
        }
    );
    // Nothing is known before the asset's first correlation.
    assert_eq!(correlator.absolute(&second), None);
    assert!(correlator.observe(&first));
    assert_eq!(
        correlator.absolute(&first),
        Some(instant("2023-11-14T22:13:20Z"))
    );
    assert_eq!(
        correlator.absolute(&second),
        Some(instant("2023-11-14T22:13:21Z"))
    );
    // A correlation whose absolute clock reads "no reading" teaches nothing.
    assert!(!correlator.observe(&second));
    // Nor does one whose boot clock does.
    let unsynced = decode(&time_packet(1_700_000_000_000_000, u32::MAX));
    assert!(!correlator.observe(&unsynced));
    // What it learned is the boot instant, which a restarted consumer can
    // restore.
    let booted = correlator.boot_time("legacy", "BOX-1");
    assert_eq!(booted, Some(instant("2023-11-14T22:13:18.766Z")));
    let mut restarted = Correlator::new();
    restarted.set_boot_time("legacy", "BOX-1", instant("2023-11-14T22:13:18.766Z"));
    assert_eq!(
        restarted.absolute(&second),
        Some(instant("2023-11-14T22:13:21Z"))
    );

    // Absolute event times pass through.
    let vendor_x = family_sets(&artifacts, "vendor_x");
    let hk = Hk {
        flags: 0,
        samples: &[],
        pos: None,
        label: b"",
        blobs: &[],
    }
    .bytes();
    let message = Message::new(Payload::Binary(&hk), now()).with_metadata(HK_METADATA);
    let decoded = vendor_x.decode(&message, &registry).unwrap();
    assert!(!correlator.observe(&decoded));
    assert_eq!(
        correlator.absolute(&decoded),
        Some(instant("2000-01-01T00:00:00Z"))
    );
}

#[test]
fn a_null_in_a_json_array_is_no_reading_at_that_position() {
    let artifacts = artifacts();
    let sets = family_sets(&artifacts, "vendor_j");
    let text = br#"{"ch": "PWR.BUS.V", "sat": "PX-9", "t": "2026-06-01T00:00:00Z", "v": 1000,
                    "cells": [{"v": 3.7, "ok": true}, null], "i": [1.5, null, 2.5]}"#;
    let decoded = sets
        .decode(&Message::new(Payload::Json(text), now()), &registry())
        .unwrap();
    let record = shown(&decoded.record);
    assert_eq!(
        record[1],
        ("cells".into(), "[{volts: 3.7, ok: true}, null]".into())
    );
    assert_eq!(record[2], ("currents".into(), "[1.5, null, 2.5]".into()));
    assert_eq!(
        decoded.samples.iter().map(sample_text).collect::<Vec<_>>(),
        triples(&[
            ("eps.bus_voltage", "F64(1.0) at v", "good"),
            ("eps.bus_current", "F64(1.5) at currents[0] +0ns", "good"),
            (
                "eps.bus_current",
                "F64(2.5) at currents[2] +20000000ns",
                "good"
            ),
        ])
    );
    assert_eq!(decoded.engineering("currents[1]"), None);
}

#[test]
fn json_payloads_nest_at_most_127_deep() {
    use crate::canonical::parse_without_duplicates;
    let arrays = |depth: usize| format!("{}{}", "[".repeat(depth), "]".repeat(depth));
    let objects = |depth: usize| format!("{}1{}", r#"{"a":"#.repeat(depth), "}".repeat(depth));
    for make in [&arrays as &dyn Fn(usize) -> String, &objects] {
        assert!(parse_without_duplicates(make(127).as_bytes()).is_ok());
        assert!(parse_without_duplicates(make(128).as_bytes()).is_err());
    }
}

#[test]
fn json_numbers_beyond_a_double_are_refused() {
    use crate::canonical::parse_without_duplicates;
    for text in [r#"{"x": 1e400}"#, r#"{"x": -1e400}"#, r#"[1, [2, 1e999]]"#] {
        let err = parse_without_duplicates(text.as_bytes()).unwrap_err();
        assert_eq!(err.code(), "invalid_json", "{text}");
    }
    assert!(
        parse_without_duplicates(br#"[18446744073709551615, -9223372036854775808, 1e308]"#).is_ok()
    );
}

#[test]
fn a_checksum_is_verified_when_decoding() {
    use crate::v2::checksum::preset;
    let crc = |bytes: &[u8]| {
        let algorithm =
            crate::v2::checksum::Algorithm::resolve(&preset("CRC-16/IBM-3740").unwrap().checksum())
                .unwrap();
        algorithm.compute(bytes) as u16
    };
    let body = [6_u8, 0x12, 0x34];
    let mut payload = body.to_vec();
    payload.extend_from_slice(&crc(&body).to_be_bytes());
    let (name, record) = identified(&payload).unwrap();
    assert_eq!(name, "CHECKED");
    assert_eq!(record[2], ("crc".to_string(), crc(&body).to_string()));

    // One flipped bit and the message is refused, naming the checksum.
    let mut damaged = payload.clone();
    damaged[1] ^= 0x01;
    let err = identified(&damaged).unwrap_err();
    assert_eq!(err.code(), "bad_checksum");
    assert!(
        err.to_string()
            .starts_with("packet CHECKED, crc: it reads "),
        "{err}"
    );
    // Too short to hold the checksum at all.
    assert_eq!(code(identified(&payload[..4])), "past_end");

    // A checksum covers from from_bits: here, everything after the first byte.
    assert_eq!(identified(&[7, 0x40, 0x40]).unwrap().0, "SUMMED");
    assert_eq!(code(identified(&[7, 0x40, 0x41])), "bad_checksum");
}

#[test]
fn the_iso_checksum_is_verified_first_byte_first_in_either_byte_order() {
    let mut little = bundle();
    let families = little["families"].as_array_mut().unwrap();
    families.last_mut().unwrap()["byte_order"] = json!("little");
    for bundle in [bundle(), little] {
        // The last test vector of ECSS-E-70-41A, A.2.5, with 0x14 as the kind.
        let payload = [0x14, 0x56, 0xF8, 0x9A, 0x00, 0x01, 0x24, 0xDC];
        let (name, record) = identified_in(&bundle, &payload).unwrap();
        assert_eq!(name, "ISO");
        assert_eq!(record[2], ("check".to_string(), 0x24DC.to_string()));
        let mut swapped = payload;
        swapped.swap(6, 7);
        assert_eq!(code(identified_in(&bundle, &swapped)), "bad_checksum");

        // Both sums of these bytes are 0: encoding writes FFFF, and the
        // standard's decoder also takes 0000, but not a single 0 byte.
        let zero_sums = [0x14, 0xD7, 0x14, 0x00, 0x00, 0x00];
        for (check, good) in [([0xFF, 0xFF], true), ([0, 0], true), ([0, 0xFF], false)] {
            let mut payload = zero_sums.to_vec();
            payload.extend_from_slice(&check);
            assert_eq!(
                identified_in(&bundle, &payload).is_ok(),
                good,
                "{payload:02X?}"
            );
        }
    }
}

#[test]
fn damage_a_static_checksum_covers_is_reported_as_a_bad_checksum() {
    let algorithm = crate::v2::checksum::Algorithm::resolve(
        &crate::v2::checksum::preset("CRC-16/IBM-3740")
            .unwrap()
            .checksum(),
    )
    .unwrap();
    let body = [8_u8, b'a', b'b', b'c', b'd'];
    let mut payload = body.to_vec();
    payload.extend_from_slice(&(algorithm.compute(&body) as u16).to_be_bytes());
    assert_eq!(identified(&payload).unwrap().0, "TEXTED");
    // The damage makes the text invalid UTF-8, which the checksum, verified
    // first, explains.
    let mut damaged = payload.clone();
    damaged[2] = 0xFF;
    assert_eq!(code(identified(&damaged)), "bad_checksum");
}

#[test]
fn tables_and_splines_give_engineering_values_and_flag_raw_values_outside_them() {
    let artifacts = artifacts();
    let sets = family_sets(&artifacts, "vendor_j");
    let engineering = |heater: f64, flow: f64| {
        let text = format!(
            r#"{{"ch": "THM", "sat": "PX-9", "t": "2026-06-01T00:00:00Z", "heater": {heater}, "flow": {flow}}}"#
        );
        let message = Message::new(Payload::Json(text.as_bytes()), now());
        let decoded = sets.decode(&message, &registry()).unwrap();
        assert_eq!(decoded.packet.name, "THERMAL");
        (
            decoded.engineering("heater").unwrap(),
            decoded.engineering("flow").unwrap(),
        )
    };
    // The step table holds -40 from raw 0 until raw 100; it extrapolates,
    // so past raw 300 it holds 60.
    let (heater, flow) = engineering(50.0, 0.5);
    assert_eq!(heater.value, SampleValue::F64(-40.0));
    assert_eq!(heater.quality, Quality::Good);
    // The spline through (0, 0), (1, 1), (2, 0) at 0.5.
    assert_eq!(flow.value, SampleValue::F64(0.6875));
    let (heater, _) = engineering(400.0, 1.0);
    assert_eq!(heater.value, SampleValue::F64(60.0));
    // The spline doesn't extrapolate: a raw value past its points keeps
    // the raw value, flagged.
    let (_, flow) = engineering(0.0, 2.5);
    assert_eq!(flow.value, SampleValue::F64(2.5));
    assert_eq!(flow.quality, Quality::OutOfCalibration);
    assert_eq!(flow.quality.as_str(), "out_of_calibration");
}

/// Bindings that send vendor_j's PX-9 and legacy's box-1 to the newer
/// sets, from well before any message.
fn timed_registry() -> Bindings {
    Bindings::from_toml(
        r#"
mdl_version = "2.0"

[[binding]]
family = "vendor_j"
native_id = "PX-9"
asset = "SAT-57"
set = "vendor_j_v2"
effective_from = "2000-01-01T00:00:00Z"

[[binding]]
family = "legacy"
native_id = "box-1"
asset = "BOX-1"
set = "legacy_v2"
effective_from = "2000-01-01T00:00:00Z"
"#,
    )
    .unwrap()
}

#[test]
fn an_elapsed_time_counts_units_since_its_epoch_on_its_scale() {
    let decode = |sets: &FamilySets, text: &str| {
        let message = Message::new(Payload::Json(text.as_bytes()), now());
        sets.decode(&message, &timed_registry())
            .map(|decoded| decoded.event_time)
    };
    // Unix milliseconds.
    let sets = family_sets(&artifacts(), "vendor_j");
    assert_eq!(
        decode(&sets, r#"{"sat": "PX-9", "t_ms": 1780315200250}"#),
        Ok(EventTime::At(instant("2026-06-01T12:00:00.25Z")))
    );
    // Float seconds on the tai scale from 2000: TAI was 32 s ahead of UTC
    // then.
    let mut bundle = bundle();
    bundle["sets"][3]["time"] = json!({ "kind": "elapsed", "unit": "s",
        "epoch": "2000-01-01T00:00:00Z", "scale": "tai" });
    bundle["sets"][3]["packets"][0]["entries"][0]["type"] = json!("float");
    let sets = family_sets(&artifacts_of(&bundle), "vendor_j");
    assert_eq!(
        decode(&sets, r#"{"sat": "PX-9", "t_ms": 1.5}"#),
        Ok(EventTime::At(instant("1999-12-31T23:59:29.5Z")))
    );
    assert_eq!(
        code(decode(&sets, r#"{"sat": "PX-9", "t_ms": 1e300}"#)),
        "invalid_time"
    );
}

#[test]
fn a_gps_time_is_a_week_and_time_of_week_and_a_short_week_is_resolved_by_ground_time() {
    let sets = family_sets(&artifacts(), "legacy");
    let decode = |week: u16, tow_ms: u32, ground: &str| {
        let mut payload = week.to_le_bytes().to_vec();
        payload.extend_from_slice(&tow_ms.to_le_bytes());
        let message = Message::new(Payload::Binary(&payload), ground.parse().unwrap());
        sets.decode(&message, &timed_registry())
            .map(|decoded| decoded.event_time)
    };
    // Week 2421, 373 in 10 bits; 129,618 s into it is 12:00:18 GPS, which
    // is 12:00:00 UTC (GPS is 18 s ahead since 2017).
    assert_eq!(
        decode(373, 129_618_000, "2026-06-01T12:00:05Z"),
        Ok(EventTime::At(instant("2026-06-01T12:00:00Z")))
    );
    // Received in 2006, the same count is a cycle earlier: week 1397, when
    // GPS was 14 s ahead.
    assert_eq!(
        decode(373, 129_618_000, "2006-10-20T00:00:00Z"),
        Ok(EventTime::At(instant("2006-10-16T12:00:04Z")))
    );
    // Only the low 10 bits of the week count.
    assert_eq!(
        decode(373 + 1024, 129_618_000, "2026-06-01T12:00:05Z"),
        Ok(EventTime::At(instant("2026-06-01T12:00:00Z")))
    );
}

#[test]
fn a_tai_time_code_is_turned_into_utc_through_the_leap_seconds() {
    // The CCSDS epoch: 1958-01-01 on the tai scale.
    let mut bundle = bundle();
    bundle["sets"][0]["time"]["epoch"] = json!("1958-01-01T00:00:00Z");
    bundle["sets"][0]["time"]["scale"] = json!("tai");
    let sets = family_sets(&artifacts_of(&bundle), "vendor_x");
    let hk = Hk {
        flags: 0,
        samples: &[],
        pos: None,
        label: b"",
        blobs: &[],
    }
    .bytes();
    let time_of = |coarse: u32| {
        let mut payload = hk.clone();
        payload[0..4].copy_from_slice(&coarse.to_be_bytes());
        payload[4..6].copy_from_slice(&[0, 0]);
        let message = Message::new(Payload::Binary(&payload), now()).with_metadata(HK_METADATA);
        sets.decode(&message, &registry()).unwrap().event_time
    };
    // 2026-06-01T12:00:37 TAI is 12:00:00 UTC.
    assert_eq!(
        time_of(2_159_006_437),
        EventTime::At(instant("2026-06-01T12:00:00Z"))
    );
    // TAI 2017-01-01T00:00:36 is the leap second 2016-12-31T23:59:60 UTC,
    // held to the instant before it ends.
    assert_eq!(
        time_of(1_861_920_036),
        EventTime::At(instant("2016-12-31T23:59:59.999999999Z"))
    );
}

#[test]
fn a_correlation_reads_its_absolute_clock_on_its_epoch_and_scale() {
    // The TIME packet's clock counts GPS microseconds since 1980.
    let mut bundle = bundle();
    let correlate = &mut bundle["sets"][2]["time"]["correlate"];
    correlate["epoch"] = json!("1980-01-06T00:00:00Z");
    correlate["scale"] = json!("gps");
    let artifacts = artifacts_of(&bundle);
    let legacy = family_sets(&artifacts, "legacy");
    // 2023-11-14T22:13:20Z is 1,700,000,000 Unix seconds; GPS is 18 s
    // ahead, from an epoch 315,964,800 s later.
    let gps_us = (1_700_000_000 + 18 - 315_964_800) * 1_000_000;
    let payload = time_packet(gps_us, 1234);
    let decoded = legacy
        .decode(&Message::new(Payload::Binary(&payload), now()), &registry())
        .unwrap();
    let mut correlator = Correlator::new();
    assert!(correlator.observe(&decoded));
    assert_eq!(
        correlator.absolute(&decoded),
        Some(instant("2023-11-14T22:13:20Z"))
    );
}
