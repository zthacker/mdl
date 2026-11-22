//! The runtime API (`spec/api.md`) as a binding uses it: a store opened
//! from its directory or its bytes, messages decoded and commands encoded,
//! and every result and error in the shape the spec fixes.

use std::fs;
use std::path::{Path, PathBuf};

use mdl::artifact::{Artifacts, build};
use mdl::encode::CommandRequest;
use mdl::message::{Message, MetaValue, Payload};
use mdl::read::read_bundle_value;
use mdl::runtime::{ApiError, Runtime, Value};
use mdl::store;
use mdl::time::Timestamp;
use mdl::validate::validate;
use serde_json::{Value as Json, json};

/// A family `sat` whose HK packet has a CUC time, an alias, a converted
/// field, a float, a count past 2^53 and bytes, and a deprecated command
/// with metadata and a default; a family `pair` of two sets with no
/// version locator, whose sets come from bindings; a family `vers` of two
/// sets its version locator chooses between, `vers_a`'s packet ending
/// after a terminated string; a JSON family `web`; and a catalog command
/// whose argument's range ends past 2^53.
fn bundle() -> Json {
    let set = |name: &str, family: &str| {
        json!({ "name": name, "family": family, "packets": [
            { "name": "P", "entries": [{ "kind": "field", "name": "x", "type": "uint", "size_bits": 8 }] }
        ] })
    };
    let versioned = |name: &str, version: u64| {
        let mut set = set(name, "vers");
        set["version"] = json!(version);
        set
    };
    json!({
        "mdl_version": "2.0",
        "catalog": {
            "params": {
                "eps.volts": { "type": "f64", "unit": "V" },
                "eps.temp": { "type": "f64", "unit": "Cel" },
                "eps.count": { "type": "u64" }
            },
            "commands": {
                "eps.reset": {
                    "deprecated": "use eps.restart",
                    "arguments": [
                        { "name": "delay", "type": "u64", "unit": "s", "range": { "max": 600 }, "default": 5 }
                    ]
                },
                "eps.stamp": {
                    "arguments": [
                        { "name": "n", "type": "u64", "range": { "min": 1, "max": "1152921504606846975" } }
                    ]
                }
            }
        },
        "families": [
            {
                "name": "sat", "encoding": "binary", "byte_order": "big",
                "alias": { "kind": "bits", "offset_bits": 48, "size_bits": 8 },
                "metadata": ["apid"]
            },
            { "name": "pair", "encoding": "binary", "byte_order": "big",
              "alias": { "kind": "fixed", "value": "p1" } },
            { "name": "vers", "encoding": "binary", "byte_order": "big",
              "alias": { "kind": "fixed", "value": "v1" },
              "version": { "kind": "metadata", "name": "fmt" }, "metadata": ["fmt"] },
            { "name": "web", "encoding": "json", "alias": { "kind": "path", "pointer": "/id" } }
        ],
        "sets": [
            {
                "name": "sat_v1", "family": "sat",
                "time": { "kind": "cuc", "offset_bits": 0, "coarse_bytes": 4, "fine_bytes": 2,
                          "epoch": "2000-01-01T00:00:00Z", "scale": "utc" },
                "packets": [{ "name": "HK", "entries": [
                    { "kind": "field", "name": "id", "type": "uint", "offset_bits": 48, "size_bits": 8 },
                    { "kind": "field", "name": "volts", "type": "uint", "size_bits": 16,
                      "conversion": { "kind": "linear", "scale": 0.5, "offset": 0.0 },
                      "unit": "V", "canonical": "eps.volts" },
                    { "kind": "field", "name": "temp", "type": "float", "size_bits": 32,
                      "unit": "Cel", "canonical": "eps.temp" },
                    { "kind": "field", "name": "count", "type": "uint", "size_bits": 64,
                      "canonical": "eps.count" },
                    { "kind": "field", "name": "tag", "type": "bytes",
                      "length": { "kind": "fixed", "bytes": 2 } }
                ] }],
                "commands": [{ "name": "RESET", "canonical": "eps.reset", "metadata": { "apid": 7 },
                    "entries": [
                        { "kind": "field", "name": "op", "type": "uint", "size_bits": 8, "value": 9 },
                        { "kind": "field", "name": "delay", "type": "uint", "size_bits": 16,
                          "unit": "s", "argument": "delay" }
                    ] }]
            },
            set("pair_v1", "pair"),
            set("pair_v2", "pair"),
            {
                "name": "vers_a", "family": "vers", "version": 1,
                "packets": [{ "name": "P", "entries": [
                    { "kind": "field", "name": "x", "type": "uint", "size_bits": 8 },
                    { "kind": "field", "name": "label", "type": "string",
                      "length": { "kind": "terminator", "byte": 0 } },
                    { "kind": "field", "name": "tail", "type": "uint", "size_bits": 8 }
                ] }]
            },
            versioned("vers_b", 2),
            { "name": "web_v1", "family": "web", "packets": [{ "name": "P", "entries": [
                { "kind": "field", "name": "id", "type": "uint", "pointer": "/id" },
                { "kind": "field", "name": "v", "type": "float", "pointer": "/v" }
            ] }] }
        ]
    })
}

fn artifacts() -> Artifacts {
    let bundle = read_bundle_value(&bundle()).unwrap().value;
    build(&validate(bundle).unwrap()).unwrap()
}

/// A store of the bundle's build in a fresh directory of this test's.
fn store_dir(test: &str) -> (PathBuf, Artifacts) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("runtime")
        .join(test);
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    let artifacts = artifacts();
    store::write(&dir, &artifacts, None).unwrap();
    (dir, artifacts)
}

/// CUC 1.5 s after 2000-01-01, alias 1, volts 25 counts, temp NaN, count
/// 2^60, tag c0ff.
fn hk() -> Vec<u8> {
    let mut payload = vec![0, 0, 0, 1, 0x80, 0x00, 1];
    payload.extend_from_slice(&25_u16.to_be_bytes());
    payload.extend_from_slice(&f32::NAN.to_be_bytes());
    payload.extend_from_slice(&(1_u64 << 60).to_be_bytes());
    payload.extend_from_slice(&[0xc0, 0xff]);
    payload
}

fn ground() -> Timestamp {
    Timestamp::from_rfc3339("2026-06-01T12:00:00Z").unwrap()
}

fn json_of(value: &Value) -> Json {
    serde_json::to_value(value).unwrap()
}

fn code<T: std::fmt::Debug>(result: Result<T, ApiError>) -> &'static str {
    match result {
        Ok(value) => panic!("expected an error, got {value:?}"),
        Err(error) => error.code,
    }
}

#[test]
fn every_value_has_one_json_spelling() {
    let safe = (1_i128 << 53) - 1;
    let cases = [
        (Value::Integer(safe), json!(9_007_199_254_740_991_i64)),
        (Value::Integer(-safe), json!(-9_007_199_254_740_991_i64)),
        (Value::Integer(safe + 1), json!("9007199254740992")),
        (
            Value::Integer(i128::from(u64::MAX)),
            json!("18446744073709551615"),
        ),
        (Value::Float(2.0), json!(2.0)),
        (Value::Float(f64::NAN), json!({ "non_finite": "NaN" })),
        (Value::Float(f64::INFINITY), json!({ "non_finite": "inf" })),
        (
            Value::Float(f64::NEG_INFINITY),
            json!({ "non_finite": "-inf" }),
        ),
        (Value::Bytes(vec![0xc0, 0xff, 0x00]), json!("c0ff00")),
        (
            Value::Instant(Timestamp::new(1, 500_000_001).unwrap()),
            json!("1970-01-01T00:00:01.500000001Z"),
        ),
        (Value::Absent, json!(null)),
        (Value::Integer(i128::MIN), json!(i128::MIN.to_string())),
    ];
    for (value, expected) in cases {
        assert_eq!(json_of(&value), expected, "{value:?}");
    }
    // A float is never written as an integer.
    assert!(
        serde_json::to_string(&Value::Float(2.0))
            .unwrap()
            .contains('.')
    );
    // Bindings return instants as Unix nanoseconds.
    let at = Timestamp::new(-1, 999_999_999).unwrap();
    assert_eq!(Value::unix_nanos(at), -1);
    assert_eq!(
        Value::unix_nanos(Timestamp::MAX),
        253_402_300_799_999_999_999
    );
}

#[test]
fn a_store_decodes_and_encodes_with_the_results_the_spec_gives() {
    let (dir, artifacts) = store_dir("decode_encode");
    let runtime = Runtime::open(&dir, None).unwrap();
    let payload = hk();
    let message = Message::new(Payload::Binary(&payload), ground());
    let decoded = runtime.decode("sat", &message).unwrap();
    let good = |path: &str, value: Json| json!({ "path": path, "value": value, "quality": "good" });
    let nan = json!({ "non_finite": "NaN" });
    let big = json!("1152921504606846976");
    assert_eq!(
        json_of(&decoded.to_value()),
        json!({
            "family": "sat",
            "native_id": 1,
            "asset": "1",
            "stand_in": true,
            "set": "sat_v1",
            "def_version": artifacts.sets["sat_v1"].digest.to_string(),
            "packet": "HK",
            "event_time": { "kind": "at", "time": "2000-01-01T00:00:01.5Z" },
            "samples": [
                { "param": "eps.volts", "value": 12.5, "unit": "V", "path": "volts",
                  "index": null, "time_offset_ns": null, "quality": "good" },
                { "param": "eps.temp", "value": nan, "unit": "Cel", "path": "temp",
                  "index": null, "time_offset_ns": null, "quality": "non_finite" },
                { "param": "eps.count", "value": big, "unit": null, "path": "count",
                  "index": null, "time_offset_ns": null, "quality": "good" }
            ],
            "record": { "id": 1, "volts": 25, "temp": nan, "count": big, "tag": "c0ff" },
            "engineering": [
                good("id", json!(1)),
                good("volts", json!(12.5)),
                { "path": "temp", "value": nan, "quality": "non_finite" },
                good("count", big),
                good("tag", json!("c0ff"))
            ]
        })
    );

    let encoded = runtime
        .encode("sat", &CommandRequest::new("sat_v1", "eps.reset"))
        .unwrap();
    assert_eq!(
        json_of(&encoded.to_value()),
        json!({
            "family": "sat",
            "set": "sat_v1",
            "def_version": artifacts.sets["sat_v1"].digest.to_string(),
            "packet": "RESET",
            "command": "eps.reset",
            "payload": "090005",
            "metadata": { "apid": 7 },
            "defaulted": ["delay"],
            "deprecated": "use eps.restart",
            "record": { "op": 9, "delay": 5 }
        })
    );
    let refused = runtime.encode(
        "sat",
        &CommandRequest::new("sat_v1", "eps.reset").with_argument("delay", 601),
    );
    let error = refused.unwrap_err();
    assert_eq!(error.code, "out_of_range");
    assert_eq!(
        json_of(&error.to_value()),
        json!({ "code": "out_of_range", "message": error.message, "problems": [] })
    );
}

#[test]
fn a_store_from_bytes_reads_as_its_directory_does() {
    let (dir, _) = store_dir("bytes");
    let mut files: Vec<(String, Vec<u8>)> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, fs::read(&path).unwrap())
        })
        .collect();
    files.sort();
    // Artifacts are found by their identity, in any order and unnamed.
    files.reverse();
    let manifest = files
        .iter()
        .find(|(name, _)| name == store::MANIFEST)
        .map(|(_, bytes)| bytes.clone())
        .unwrap();
    let artifacts: Vec<&[u8]> = files
        .iter()
        .filter(|(name, _)| name != store::MANIFEST)
        .map(|(_, bytes)| bytes.as_slice())
        .collect();
    let from_bytes = Runtime::open_bytes(&manifest, &artifacts, None).unwrap();
    let from_dir = Runtime::open(&dir, None).unwrap();
    let payload = hk();
    let message = Message::new(Payload::Binary(&payload), ground());
    // As JSON, since the decoded NaN isn't equal to itself.
    assert_eq!(
        json_of(&from_bytes.decode("sat", &message).unwrap().to_value()),
        json_of(&from_dir.decode("sat", &message).unwrap().to_value())
    );
    assert_eq!(
        from_bytes.families().collect::<Vec<_>>(),
        ["pair", "sat", "vers", "web"]
    );
    // One the manifest names, missing.
    let fewer = &artifacts[1..];
    assert_eq!(
        code(Runtime::open_bytes(&manifest, fewer, None)),
        "store_unreadable"
    );
    // As in a directory, what the manifest doesn't need isn't read: an
    // artifact of a newer version, bytes that aren't JSON.
    let newer = String::from_utf8(artifacts[0].to_vec())
        .unwrap()
        .replace("\"mdl_version\":\"2.0\"", "\"mdl_version\":\"2.9\"");
    let mut more = artifacts.clone();
    more.push(newer.as_bytes());
    more.push(b"not json");
    assert!(Runtime::open_bytes(&manifest, &more, None).is_ok());
}

/// The store with its set `sat_v1` replaced by `edit` of it, under its new
/// digest: an artifact that reads, and so a build only validation refuses.
fn replace_set(dir: &Path, artifacts: &Artifacts, edit: impl FnOnce(&mut Json)) {
    let mut set: Json = serde_json::from_str(&artifacts.sets["sat_v1"].canonical).unwrap();
    edit(&mut set);
    let canonical = mdl::canonical::to_canonical_string(&set).unwrap();
    let digest = mdl::ir::Digest::of_canonical(&canonical);
    fs::write(dir.join(format!("{digest}.json")), &canonical).unwrap();
    let manifest_path = dir.join(store::MANIFEST);
    let mut manifest: Json =
        serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
    manifest["sets"]["sat_v1"] = json!(digest.to_string());
    fs::write(&manifest_path, manifest.to_string()).unwrap();
}

#[test]
fn problems_opening_a_store_have_their_codes() {
    let (dir, artifacts) = store_dir("problems");
    let manifest_path = dir.join(store::MANIFEST);
    let good = fs::read_to_string(&manifest_path).unwrap();
    assert_eq!(
        code(Runtime::open(&dir.join("nowhere"), None)),
        "store_unreadable"
    );
    let cases = [
        (
            good.replace("\"manifest\": 1", "\"manifest\": 2"),
            "manifest_version",
        ),
        ("{".to_string(), "bad_manifest"),
        (
            good.replace("\"manifest\": 1", "\"manifest\": 1, \"owner\": \"ops\""),
            "bad_manifest",
        ),
    ];
    for (text, expected) in cases {
        fs::write(&manifest_path, &text).unwrap();
        assert_eq!(code(Runtime::open(&dir, None)), expected, "{text}");
    }
    fs::write(&manifest_path, &good).unwrap();
    // A set's file holding the catalog.
    let set = dir.join(format!("{}.json", artifacts.sets["sat_v1"].digest));
    let catalog = dir.join(format!("{}.json", artifacts.catalog.digest));
    let kept = fs::read(&set).unwrap();
    fs::copy(&catalog, &set).unwrap();
    assert_eq!(code(Runtime::open(&dir, None)), "wrong_digest");
    // A file that isn't an artifact at all: the read's own code.
    fs::write(&set, "[]").unwrap();
    let error = Runtime::open(&dir, None).unwrap_err();
    assert_eq!(error.code, "wrong_shape", "{}", error.message);
    assert!(error.message.contains(&set.display().to_string()));
    // One of a version this MDL doesn't read is sound: no advice to delete
    // it.
    let newer = String::from_utf8(kept.clone())
        .unwrap()
        .replace("\"mdl_version\":\"2.0\"", "\"mdl_version\":\"2.9\"");
    fs::write(&set, newer).unwrap();
    let error = Runtime::open(&dir, None).unwrap_err();
    assert_eq!(error.code, "unsupported_version", "{}", error.message);
    assert!(!error.message.contains("delete"), "{}", error.message);
    fs::write(&set, kept).unwrap();
    assert!(Runtime::open(&dir, None).is_ok());
    // A build that reads but doesn't validate: invalid_build, with each
    // problem.
    replace_set(&dir, &artifacts, |set| {
        set["set"]["packets"][0]["entries"][1]["canonical"] = json!("eps.vols");
    });
    let error = Runtime::open(&dir, None).unwrap_err();
    assert_eq!(error.code, "invalid_build", "{}", error.message);
    let problems: Vec<(&str, &str)> = error
        .problems
        .iter()
        .map(|problem| (problem.code, problem.path.as_str()))
        .collect();
    assert_eq!(
        problems,
        // The build as one bundle, its sets in name order: pair_v1,
        // pair_v2, sat_v1.
        [("unknown_canonical", "/sets/2/packets/0/entries/1/canonical")],
        "{}",
        error.message
    );
    assert!(
        error.message.contains("did you mean eps.volts?"),
        "{}",
        error.message
    );
    let shown = json_of(&error.to_value());
    assert_eq!(shown["problems"][0]["code"], "unknown_canonical");
}

/// A spelling a release before 1.0 began refusing when building, which an
/// earlier one wrote: a packet's limits restating the argument's own unit
/// (`spec/compatibility.md` §7). The store still opens, and the limits
/// still hold.
#[test]
fn a_store_an_earlier_release_built_with_a_refused_spelling_still_opens() {
    let (dir, artifacts) = store_dir("refused_spelling");
    replace_set(&dir, &artifacts, |set| {
        set["mdl_version"] = json!("2.2");
        set["set"]["commands"][0]["arguments"] =
            json!({ "delay": { "unit": "s", "range": { "max": 60 } } });
    });
    let runtime = Runtime::open(&dir, None).unwrap();
    let reset = |delay: u64| {
        runtime.encode(
            "sat",
            &CommandRequest::new("sat_v1", "eps.reset").with_argument("delay", delay),
        )
    };
    assert!(reset(60).is_ok());
    assert_eq!(code(reset(61)), "out_of_range");
    // A build of the same definitions refuses it.
    let mut definitions = bundle();
    definitions["mdl_version"] = json!("2.2");
    definitions["sets"][0]["commands"][0]["arguments"] =
        json!({ "delay": { "unit": "s", "range": { "max": 60 } } });
    let refused = validate(read_bundle_value(&definitions).unwrap().value).unwrap_err();
    let codes: Vec<&str> = refused.iter().map(|d| d.code.as_str()).collect();
    assert_eq!(codes, ["explicit_default"]);
}

/// A checksum that covers no bytes, which a release before 1.0 began
/// refusing when building after an earlier one built it: it can't do what
/// it says (`spec/compatibility.md` §7). The store still opens, and the
/// checksum is read as written (`spec/decoding.md` §5.6).
#[test]
fn a_store_an_earlier_release_built_with_a_checksum_over_no_bytes_still_opens() {
    // CRC-16/IBM-3740, whose value for no bytes is its init.
    let crc = |from_bits: u64| {
        json!({ "kind": "checksum", "name": "crc", "from_bits": from_bits,
                "algorithm": { "kind": "crc", "width": 16, "poly": 4129, "init": 65535,
                               "reflect_in": false, "reflect_out": false, "xor_out": 0 } })
    };
    let push = |set: &mut Json, kind: &str, entry: Json| {
        set[kind][0]["entries"].as_array_mut().unwrap().push(entry);
    };
    let decode = |runtime: &Runtime, check: [u8; 2]| {
        let mut payload = hk();
        payload.extend_from_slice(&check);
        runtime.decode("sat", &Message::new(Payload::Binary(&payload), ground()))
    };
    let reset = CommandRequest::new("sat_v1", "eps.reset");
    // From where each starts, after HK's tag and RESET's delay: nothing.
    let (dir, artifacts) = store_dir("empty_checksum");
    replace_set(&dir, &artifacts, |set| {
        push(&mut set["set"], "packets", crc(184));
        push(&mut set["set"], "commands", crc(24));
    });
    let runtime = Runtime::open(&dir, None).unwrap();
    assert!(decode(&runtime, [0xff, 0xff]).is_ok());
    assert_eq!(code(decode(&runtime, [0x12, 0x34])), "bad_checksum");
    let encoded = runtime.encode("sat", &reset).unwrap();
    assert_eq!(encoded.payload, [9, 0, 5, 0xff, 0xff]);
    // From past where each starts: every message is invalid_layout.
    replace_set(&dir, &artifacts, |set| {
        push(&mut set["set"], "packets", crc(192));
        push(&mut set["set"], "commands", crc(32));
    });
    let runtime = Runtime::open(&dir, None).unwrap();
    assert_eq!(code(decode(&runtime, [0xff, 0xff])), "invalid_layout");
    assert_eq!(code(runtime.encode("sat", &reset)), "invalid_layout");
    // A build of the same definitions refuses each.
    let mut definitions = bundle();
    push(&mut definitions["sets"][0], "packets", crc(184));
    push(&mut definitions["sets"][0], "commands", crc(32));
    let refused = validate(read_bundle_value(&definitions).unwrap().value).unwrap_err();
    let problems: Vec<(&str, &str)> = refused
        .iter()
        .map(|d| (d.code.as_str(), d.path.as_str()))
        .collect();
    assert_eq!(
        problems,
        [
            ("checksum", "/sets/0/commands/0/entries/2/from_bits"),
            ("checksum", "/sets/0/packets/0/entries/5/from_bits"),
        ]
    );
}

#[test]
fn without_bindings_each_native_id_stands_for_an_asset() {
    let (dir, _) = store_dir("stand_ins");
    let runtime = Runtime::open(&dir, None).unwrap();
    let payload = [5_u8];
    let pair = Message::new(Payload::Binary(&payload), ground());
    // Which of pair's two sets would decode it is a binding's to say.
    let error = runtime.decode("pair", &pair).unwrap_err();
    assert_eq!(error.code, "missing_set");
    assert!(
        error.message.contains("open the store with bindings"),
        "{}",
        error.message
    );
    assert_eq!(code(runtime.decode("nope", &pair)), "unknown_family");
    // Where the version locator chooses the set, a stand-in needs none.
    let fmt = [("fmt", MetaValue::Uint(2))];
    let vers = runtime
        .decode(
            "vers",
            &Message::new(Payload::Binary(&payload), ground()).with_metadata(&fmt),
        )
        .unwrap();
    assert_eq!(
        (vers.asset.as_str(), vers.set.as_str(), vers.stand_in),
        ("v1", "vers_b", true)
    );
    // Each message gets its own result, in order.
    let good = hk();
    let short = &good[..10];
    let messages = [
        Message::new(Payload::Binary(&good), ground()),
        Message::new(Payload::Binary(short), ground()),
        Message::new(Payload::Binary(&good), ground()),
    ];
    let results = runtime.decode_many("sat", &messages);
    let codes: Vec<Result<&str, &str>> = results
        .iter()
        .map(|result| match result {
            Ok(decoded) => Ok(decoded.packet.as_str()),
            Err(error) => Err(error.code),
        })
        .collect();
    // The packet has no length of its own, so a payload cut short ends
    // inside an entry.
    assert_eq!(codes, [Ok("HK"), Err("past_end"), Ok("HK")]);
}

#[test]
fn bindings_name_each_asset_and_its_set() {
    let (dir, _) = store_dir("bindings");
    let bindings = r#"mdl_version = "2.0"

[[binding]]
family = "pair"
native_id = "p1"
asset = "PAIR-1"
set = "pair_v2"
effective_from = "2026-01-01T00:00:00Z"

[[binding]]
family = "sat"
native_id = 1
asset = "SAT-1"
set = "sat_v1"
effective_from = "2026-01-01T00:00:00Z"
"#;
    let runtime = Runtime::open(&dir, Some(bindings)).unwrap();
    let payload = [5_u8];
    let decoded = runtime
        .decode("pair", &Message::new(Payload::Binary(&payload), ground()))
        .unwrap();
    assert_eq!(
        (
            decoded.asset.as_str(),
            decoded.set.as_str(),
            decoded.stand_in
        ),
        ("PAIR-1", "pair_v2", false)
    );
    let hk = hk();
    let sat = runtime
        .decode("sat", &Message::new(Payload::Binary(&hk), ground()))
        .unwrap();
    assert_eq!(sat.asset, "SAT-1");
    // Before the row takes effect, the native id is no asset.
    let early = Timestamp::from_rfc3339("2025-12-31T23:59:59Z").unwrap();
    assert_eq!(
        code(runtime.decode("sat", &Message::new(Payload::Binary(&hk), early))),
        "unknown_asset"
    );
    assert!(runtime.warnings().is_empty());
    // Bindings that aren't TOML, or whose rows aren't valid in themselves,
    // refuse the open.
    assert_eq!(
        code(Runtime::open(&dir, Some("binding = ["))),
        "invalid_toml"
    );
    let empty_asset = bindings.replace("\"PAIR-1\"", "\"\"");
    let error = Runtime::open(&dir, Some(&empty_asset)).unwrap_err();
    assert_eq!(error.code, "invalid_bindings", "{}", error.message);
    assert_eq!(error.problems[0].code, "empty");
    // Rows the build doesn't fit are warnings, and refuse only the messages
    // they affect; rows for families it doesn't have aren't mentioned.
    let ahead = format!(
        "{}\n[[binding]]\nfamily = \"new_vendor\"\nnative_id = 1\nasset = \"NEW-1\"\nset = \"new_v1\"\neffective_from = \"2026-01-01T00:00:00Z\"\n",
        bindings.replace("pair_v2", "pair_v9")
    );
    let runtime = Runtime::open(&dir, Some(&ahead)).unwrap();
    let warnings: Vec<&str> = runtime.warnings().iter().map(|w| w.code).collect();
    assert_eq!(warnings, ["unknown_set"], "{:?}", runtime.warnings());
    assert_eq!(
        code(runtime.decode("pair", &Message::new(Payload::Binary(&payload), ground()))),
        "unknown_set"
    );
    assert_eq!(
        runtime
            .decode("sat", &Message::new(Payload::Binary(&hk), ground()))
            .unwrap()
            .asset,
        "SAT-1"
    );
    // Other bindings for the same build, without opening it again.
    let bare = runtime.with_bindings(None);
    let stand_in = bare
        .decode("sat", &Message::new(Payload::Binary(&hk), ground()))
        .unwrap();
    assert_eq!((stand_in.asset.as_str(), stand_in.stand_in), ("1", true));
    assert!(bare.warnings().is_empty());
}

#[test]
fn a_runtime_is_shared_across_threads() {
    fn shared<T: Send + Sync + Clone>() {}
    shared::<Runtime>();
    let (dir, _) = store_dir("threads");
    let runtime = Runtime::open(&dir, None).unwrap();
    let payload = hk();
    let message = Message::new(Payload::Binary(&payload), ground());
    // Compared as JSON, since the decoded NaN isn't equal to itself.
    let expected = json_of(&runtime.decode("sat", &message).unwrap().to_value());
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let runtime = runtime.clone();
                let payload = payload.clone();
                scope.spawn(move || {
                    let message = Message::new(Payload::Binary(&payload), ground());
                    json_of(&runtime.decode("sat", &message).unwrap().to_value())
                })
            })
            .collect();
        for handle in handles {
            assert_eq!(handle.join().unwrap(), expected);
        }
    });
    // Metadata the link delivers reaches the decoder.
    let with_meta = message.with_metadata(&[("apid", MetaValue::Uint(3))]);
    assert!(runtime.decode("sat", &with_meta).is_ok());
}

#[test]
fn describe_gives_the_build_with_its_digests_and_layouts() {
    let (dir, artifacts) = store_dir("describe");
    let runtime = Runtime::open(&dir, None).unwrap();
    let description = runtime.describe().unwrap();
    let shown = json_of(&description.to_value().unwrap());
    assert_eq!(shown["format"], 1);
    assert_eq!(
        shown["catalog"]["digest"],
        artifacts.catalog.digest.to_string()
    );
    assert_eq!(
        shown["catalog"]["catalog"]["params"]["eps.volts"],
        json!({ "type": "f64", "unit": "V" })
    );
    let names = |list: &Json| -> Vec<String> {
        list.as_array()
            .unwrap()
            .iter()
            .map(|item| item["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(names(&shown["families"]), ["pair", "sat", "vers", "web"]);
    let sat = &shown["families"][1];
    assert_eq!(sat["digest"], artifacts.families["sat"].digest.to_string());
    assert_eq!(sat["sets"], json!(["sat_v1"]));
    assert_eq!(
        sat["family"]["alias"],
        json!({ "kind": "bits", "offset_bits": 48, "size_bits": 8 })
    );
    assert_eq!(
        names(&shown["sets"]),
        ["pair_v1", "pair_v2", "sat_v1", "vers_a", "vers_b", "web_v1"]
    );
    let sat_v1 = &shown["sets"][2];
    assert_eq!(
        sat_v1["def_version"],
        artifacts.sets["sat_v1"].digest.to_string()
    );
    assert_eq!(sat_v1["pins_catalog"], Json::Null);
    assert_eq!(sat_v1["set"]["packets"][0]["name"], "HK");
    // Each entry where it is: after the CUC time, the id, volts, temp,
    // count and tag; and the command's opcode and delay.
    let starts = |container: &Json| -> Vec<Json> {
        container["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|placement| placement["static_offset_bits"].clone())
            .collect()
    };
    let hk = &sat_v1["layout"]["packets"][0];
    assert_eq!(starts(hk), [48, 56, 72, 104, 168].map(Json::from));
    assert_eq!(hk["size_min_bits"], 184);
    assert_eq!(
        hk["entries"][4],
        json!({ "static_offset_bits": 168, "start_min_bits": 168, "start_max_bits": 168,
                "size_min_bits": 16, "size_max_bits": 16, "conditional": false })
    );
    let reset = &sat_v1["layout"]["commands"][0];
    assert_eq!(starts(reset), [0, 8].map(Json::from));
    // After a string of any length, an entry has no static position and
    // no latest start.
    let vers_a = &shown["sets"][3]["layout"]["packets"][0];
    assert_eq!(
        vers_a["entries"][2],
        json!({ "static_offset_bits": null, "start_min_bits": 16, "start_max_bits": null,
                "size_min_bits": 8, "size_max_bits": 8, "conditional": false })
    );
    assert_eq!(vers_a["size_max_bits"], Json::Null);
    // A JSON family's entries have no positions.
    assert_eq!(shown["sets"][5]["layout"], Json::Null);
    // The IR's own values, not converted: a range end past 2^53 stays the
    // IR's decimal string, in a binding too.
    let stamp = &shown["catalog"]["catalog"]["commands"]["eps.stamp"]["arguments"][0];
    assert_eq!(stamp["range"]["max"], "1152921504606846975");
    let Value::Object(top) = description.to_value().unwrap() else {
        panic!("a description is an object");
    };
    let catalog = &top.iter().find(|(key, _)| key == "catalog").unwrap().1;
    let text = format!("{catalog:?}");
    assert!(text.contains("Text(\"1152921504606846975\")"), "{text}");
}

#[test]
fn bindings_are_looked_up_by_native_id_and_by_asset() {
    use mdl::bindings::NativeId;

    let (dir, _) = store_dir("lookups");
    let bindings = r#"mdl_version = "2.0"

[[binding]]
family = "pair"
native_id = "p1"
asset = "PAIR-1"
set = "pair_v2"
effective_from = "2026-01-01T00:00:00Z"

[[binding]]
family = "sat"
native_id = 1
asset = "SAT-1"
set = "sat_v1"
effective_from = "2026-01-01T00:00:00Z"

[[binding]]
family = "sat"
native_id = 1
retired = true
effective_from = "2026-07-01T00:00:00Z"
"#;
    let runtime = Runtime::open(&dir, Some(bindings)).unwrap();
    let at = |text: &str| Timestamp::from_rfc3339(text).unwrap();
    let p1 = NativeId::from_text("p1");
    let found = runtime.binding("pair", &p1, ground()).unwrap().unwrap();
    assert_eq!(
        json_of(&found.to_value()),
        json!({ "family": "pair", "native_id": "p1", "asset": "PAIR-1", "set": "pair_v2",
                "stand_in": false })
    );
    // Before its row takes effect, and after a row retires it, a native id
    // is no asset.
    assert_eq!(
        runtime
            .binding("pair", &p1, at("2025-12-31T23:59:59Z"))
            .unwrap(),
        None
    );
    let one = NativeId::from_text("1");
    assert!(runtime.binding("sat", &one, ground()).unwrap().is_some());
    assert_eq!(
        runtime
            .binding("sat", &one, at("2026-07-01T00:00:00Z"))
            .unwrap(),
        None
    );
    // The other way: where a command to an asset goes.
    let sat = runtime.asset("sat", "SAT-1", ground()).unwrap().unwrap();
    assert_eq!(
        (sat.native_id.as_int(), sat.set.as_deref()),
        (Some(1), Some("sat_v1"))
    );
    assert_eq!(
        runtime
            .asset("sat", "SAT-1", at("2026-08-01T00:00:00Z"))
            .unwrap(),
        None
    );
    assert_eq!(runtime.asset("sat", "nobody", ground()).unwrap(), None);
    assert_eq!(
        code(runtime.asset("nope", "SAT-1", ground())),
        "unknown_family"
    );
    // Two native ids that are one asset at once: no one destination.
    let twice = format!(
        "{bindings}\n[[binding]]\nfamily = \"sat\"\nnative_id = 2\nasset = \"SAT-1\"\nset = \"sat_v1\"\neffective_from = \"2026-01-01T00:00:00Z\"\n"
    );
    let runtime = Runtime::open(&dir, Some(&twice)).unwrap();
    let error = runtime.asset("sat", "SAT-1", ground()).unwrap_err();
    assert_eq!(error.code, "ambiguous_asset");
    assert!(
        error.message.contains("native ids 1, 2"),
        "{}",
        error.message
    );

    // Without bindings, a native id stands for an asset of its own.
    let bare = runtime.with_bindings(None);
    let stand_in = bare.binding("sat", &one, ground()).unwrap().unwrap();
    assert_eq!(
        json_of(&stand_in.to_value()),
        json!({ "family": "sat", "native_id": 1, "asset": "1", "set": "sat_v1",
                "stand_in": true })
    );
    let by_name = bare.asset("sat", "1", ground()).unwrap().unwrap();
    assert_eq!(by_name.native_id.as_int(), Some(1));
    assert_eq!(code(bare.binding("pair", &p1, ground())), "missing_set");

    // Rows the build doesn't fit: the lookups refuse them as decoding a
    // message does, with the same codes.
    let misfits = r#"mdl_version = "2.0"

[[binding]]
family = "pair"
native_id = "p1"
asset = "PAIR-1"
effective_from = "2026-01-01T00:00:00Z"

[[binding]]
family = "vers"
native_id = "v1"
asset = "V-1"
set = "vers_a"
effective_from = "2026-01-01T00:00:00Z"

[[binding]]
family = "sat"
native_id = 1
asset = "SAT-1"
set = "sat_v9"
effective_from = "2026-01-01T00:00:00Z"
"#;
    let runtime = Runtime::open(&dir, Some(misfits)).unwrap();
    let mut warned: Vec<&str> = runtime.warnings().iter().map(|w| w.code).collect();
    warned.sort_unstable();
    assert_eq!(warned, ["binding_set", "binding_set", "unknown_set"]);
    let payload = [5_u8];
    let hk = hk();
    let fmt = [("fmt", MetaValue::Uint(1))];
    let cases = [
        (
            "pair",
            "p1",
            "PAIR-1",
            Message::new(Payload::Binary(&payload), ground()),
            "missing_set",
        ),
        (
            "vers",
            "v1",
            "V-1",
            Message::new(Payload::Binary(&payload), ground()).with_metadata(&fmt),
            "unexpected_set",
        ),
        (
            "sat",
            "1",
            "SAT-1",
            Message::new(Payload::Binary(&hk), ground()),
            "unknown_set",
        ),
    ];
    for (family, native_id, asset, message, expected) in cases {
        let native_id = NativeId::from_text(native_id);
        assert_eq!(code(runtime.decode(family, &message)), expected, "{family}");
        assert_eq!(
            code(runtime.binding(family, &native_id, ground())),
            expected,
            "{family}"
        );
        assert_eq!(
            code(runtime.asset(family, asset, ground())),
            expected,
            "{family}"
        );
    }
}

/// A message's bytes take their family's encoding (`spec/api.md` §2.2), so
/// a binding passes bytes and holds no rule of its own.
#[test]
fn a_payload_takes_its_familys_encoding() {
    let (dir, _) = store_dir("payload_kinds");
    let runtime = Runtime::open(&dir, None).unwrap();
    let bytes = br#"{"id":1,"v":2.5}"#;
    assert_eq!(runtime.payload("web", bytes), Ok(Payload::Json(bytes)));
    assert_eq!(runtime.payload("sat", bytes), Ok(Payload::Binary(bytes)));
    assert_eq!(
        runtime.payload("nope", bytes).map_err(|error| error.code),
        Err("unknown_family")
    );
}
