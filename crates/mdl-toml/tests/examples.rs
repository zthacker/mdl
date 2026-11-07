//! The examples in `examples/toml` read, validate and build, and decode
//! messages the way their comments say.

use std::path::Path;

use mdl::v2::artifact::{Artifacts, Written, build};
use mdl::v2::bindings::Bindings;
use mdl::v2::decode::{Decoded, EventTime, Quality, SampleValue};
use mdl::v2::ir::Artifact;
use mdl::v2::message::{Message, MetaValue, Payload};
use mdl::v2::read::{Loaded, read_artifact};
use mdl::v2::route::FamilySets;
use mdl::v2::source::{SourceDiagnostic, Sources};
use mdl::v2::time::Timestamp;
use mdl::v2::validate::validate;

/// Every TOML file of an example, read in name order, validated and built.
/// A problem fails the test with the diagnostics as a person would see them.
fn example(name: &str) -> Artifacts {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/toml")
        .join(name);
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "toml"))
        .collect();
    paths.sort();
    let mut sources = Sources::new();
    for path in &paths {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        sources.add(name, std::fs::read_to_string(path).unwrap());
    }
    let shown = |errors: &[SourceDiagnostic]| {
        errors
            .iter()
            .map(|e| e.display(&sources).to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let parsed = mdl_toml::read(sources.iter()).unwrap_or_else(|e| panic!("{}", shown(&e)));
    let validated =
        validate(parsed.bundle).unwrap_or_else(|d| panic!("{}", shown(&parsed.map.locate_all(&d))));
    build(&validated).unwrap()
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

fn bindings(rows: &str) -> Bindings {
    Bindings::from_toml(&format!("mdl_version = \"2.0\"\n{rows}")).unwrap()
}

fn time(text: &str) -> Timestamp {
    text.parse().unwrap()
}

/// The good value of `param` in a decoded message.
fn value(decoded: &Decoded<'_>, param: &str) -> SampleValue {
    let sample = decoded
        .samples
        .iter()
        .find(|s| s.param == param)
        .unwrap_or_else(|| panic!("no {param}"));
    assert_eq!(sample.quality, Quality::Good, "{param}");
    sample.value.clone()
}

fn number(decoded: &Decoded<'_>, param: &str) -> f64 {
    match value(decoded, param) {
        SampleValue::F64(x) => x,
        other => panic!("{param}: {other:?}"),
    }
}

#[test]
fn the_mission_decodes_a_ccsds_a_json_and_a_mavlink_asset() {
    let artifacts = example("mission");
    let registry = bindings(
        r#"
[[binding]]
family = "ccsds_sat"
native_id = 101
asset = "SAT-1"
set = "ccsds_sat_v1"
effective_from = "2026-01-01T00:00:00Z"

[[binding]]
family = "json_sat"
native_id = "PX-7"
asset = "SAT-2"
set = "json_sat_v1"
effective_from = "2026-01-01T00:00:00Z"

[[binding]]
family = "drone"
native_id = 1
asset = "UAV-1"
set = "drone_v1"
effective_from = "2026-01-01T00:00:00Z"
"#,
    );
    let ground = time("2026-06-01T12:00:31Z");

    // EPS_HK: CUC time, spacecraft 101, 28410 mV, 2000 counts, NOMINAL.
    let mut hk = vec![0; 6];
    hk.extend_from_slice(&101_u16.to_be_bytes());
    hk.extend_from_slice(&28_410_u16.to_be_bytes());
    hk.extend_from_slice(&2000_u16.to_be_bytes());
    hk.push(1);
    let apid = [("apid", MetaValue::Uint(0x064))];
    let sats = family_sets(&artifacts, "ccsds_sat");
    let message = Message::new(Payload::Binary(&hk), ground).with_metadata(&apid);
    let decoded = sats.decode(&message, &registry).unwrap();
    assert_eq!(decoded.route.binding.asset, "SAT-1");
    assert_eq!(number(&decoded, "eps.bus_voltage"), 28.41);
    assert_eq!(number(&decoded, "eps.batt_temp"), 30.0);
    assert_eq!(
        value(&decoded, "eps.mode"),
        SampleValue::Enum("NOMINAL".into())
    );

    // A JSON line in Fahrenheit, decoded into Celsius.
    let line = br#"{"sat":"PX-7","ch":"BATT.T","v":212,"t":"2026-06-01T12:00:30.25Z"}"#;
    let json = family_sets(&artifacts, "json_sat");
    let decoded = json
        .decode(&Message::new(Payload::Json(line), ground), &registry)
        .unwrap();
    assert!((number(&decoded, "eps.batt_temp") - 100.0).abs() < 1e-9);
    assert_eq!(
        decoded.event_time,
        EventTime::At(time("2026-06-01T12:00:30.25Z"))
    );

    // MAVLink SYS_STATUS: 15200 mV and 76 % at their offsets.
    let mut status = vec![0; 31];
    status[14..16].copy_from_slice(&15_200_u16.to_le_bytes());
    status[30] = 76;
    let meta = [("msgid", MetaValue::Uint(1)), ("sysid", MetaValue::Uint(1))];
    let drone = family_sets(&artifacts, "drone");
    let message = Message::new(Payload::Binary(&status), ground).with_metadata(&meta);
    let decoded = drone.decode(&message, &registry).unwrap();
    assert_eq!(number(&decoded, "eps.bus_voltage"), 15.2);
    assert_eq!(number(&decoded, "eps.batt_remaining"), 76.0);
    assert_eq!(decoded.event_time, EventTime::Ground);
}

#[test]
fn mixed_versions_on_one_link_each_decode_with_their_own_set() {
    let artifacts = example("mixed_versions");
    let registry = bindings(
        r#"
[[binding]]
family = "beacon"
native_id = 7
asset = "NODE-7"
effective_from = "2026-01-01T00:00:00Z"
"#,
    );
    let beacons = family_sets(&artifacts, "beacon");
    let ground = time("2026-06-01T12:00:00Z");

    let v1 = [0x00, 0x07, 0x01, 0x6E, 0xFA];
    let decoded = beacons
        .decode(&Message::new(Payload::Binary(&v1), ground), &registry)
        .unwrap();
    assert_eq!(decoded.route.set.name, "beacon_v1");
    assert_eq!(number(&decoded, "eps.bus_voltage"), 28.41);

    let v2 = [0x00, 0x07, 0x02, 0x0B, 0x19, 0x00, 0xFA];
    let decoded = beacons
        .decode(&Message::new(Payload::Binary(&v2), ground), &registry)
        .unwrap();
    assert_eq!(decoded.route.set.name, "beacon_v2");
    assert!((number(&decoded, "eps.bus_voltage") - 28.41).abs() < 1e-9);
    assert!((number(&decoded, "eps.batt_temp") - 25.0).abs() < 1e-9);
}
