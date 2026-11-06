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
    let bundle = read_bundle(&serde_json::to_vec(&bundle()).unwrap())
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
    let text = r#"{"ch": "PWR.BUS.V", "sat": "PX-9", "v": 28000.0, "cells": []}"#;
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
    assert_eq!(err.to_string(), "packet BUS, cells: nothing at /cells");
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
    let artifacts = artifacts();
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
