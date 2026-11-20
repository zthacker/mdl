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
/// version locator, whose sets come from bindings; and a family `vers` of
/// two sets its version locator chooses between.
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
              "version": { "kind": "metadata", "name": "fmt" }, "metadata": ["fmt"] }
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
            versioned("vers_a", 1),
            versioned("vers_b", 2)
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
        ["pair", "sat", "vers"]
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
