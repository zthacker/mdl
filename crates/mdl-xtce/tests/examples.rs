//! The examples in `examples/xtce` import, validate and build, and decode
//! packets the way their comments say.

use std::path::Path;

use mdl::v2::artifact::{Artifacts, Written, build};
use mdl::v2::bindings::Bindings;
use mdl::v2::decode::{Decoded, Quality, SampleValue};
use mdl::v2::ir::Artifact;
use mdl::v2::message::{Message, Payload};
use mdl::v2::read::{Loaded, read_artifact};
use mdl::v2::route::FamilySets;
use mdl::v2::source::{SourceDiagnostic, Sources};
use mdl::v2::time::Timestamp;
use mdl::v2::validate::validate;

/// An example imported, validated and built, with its warnings as people
/// see them. A problem fails the test with the diagnostics shown.
fn example(file: &str) -> (Artifacts, Vec<String>) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/xtce")
        .join(file);
    let mut sources = Sources::new();
    sources.add(file, std::fs::read_to_string(path).unwrap());
    let shown = |diagnostics: &[SourceDiagnostic]| {
        diagnostics
            .iter()
            .map(|d| d.display(&sources).to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let parsed = mdl_xtce::read(sources.iter(), &mdl_xtce::Options::default())
        .unwrap_or_else(|e| panic!("{}", shown(&e)));
    let warnings = parsed
        .warnings
        .iter()
        .map(|w| {
            let at = sources.location(w.span.unwrap()).unwrap();
            format!("{}:{} {}", at.line, at.column, w.message)
        })
        .collect();
    let validated =
        validate(parsed.bundle).unwrap_or_else(|d| panic!("{}", shown(&parsed.map.locate_all(&d))));
    (build(&validated).unwrap(), warnings)
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

/// A CCSDS primary header for telemetry: version 0, no secondary header,
/// standalone, sequence count 7, and the packet length field (octets
/// after the header, minus one).
fn header(apid: u16, data_bytes: u16) -> Vec<u8> {
    let mut bytes = apid.to_be_bytes().to_vec();
    bytes.extend_from_slice(&(0xC000_u16 | 7).to_be_bytes());
    bytes.extend_from_slice(&(data_bytes - 1).to_be_bytes());
    bytes
}

#[test]
fn demosat_decodes_its_eps_and_adcs_packets() {
    let (artifacts, warnings) = example("demosat.xml");
    assert_eq!(
        warnings,
        [
            "68:27 SpaceSystem EPS's shortDescription isn't imported: MDL keeps descriptions \
             for catalog params only",
            "150:28 SpaceSystem ADCS's shortDescription isn't imported: MDL keeps descriptions \
             for catalog params only",
        ]
    );
    let registry = Bindings::from_toml(
        r#"
mdl_version = "2.0"

[[binding]]
family = "demosat"
native_id = "DemoSat"
asset = "DEMO-1"
set = "demosat"
effective_from = "2026-01-01T00:00:00Z"
"#,
    )
    .unwrap();
    let ground: Timestamp = "2026-06-01T12:00:00Z".parse().unwrap();
    let sats = family_sets(&artifacts, "demosat");

    // EPS_HK: NOMINAL, 800 counts (20 Cel), 28410 mV, uptime 123456 s
    // little-endian, heater on.
    let mut eps = header(100, 9);
    let temperature = 800_u16 & 0x0FFF;
    eps.extend_from_slice(&((2_u16 << 12) | temperature).to_be_bytes());
    eps.extend_from_slice(&28_410_u16.to_be_bytes());
    eps.extend_from_slice(&123_456_u32.to_le_bytes());
    eps.push(1);
    let decoded = sats
        .decode(&Message::new(Payload::Binary(&eps), ground), &registry)
        .unwrap();
    assert_eq!(decoded.packet.name, "EPS_HK");
    assert_eq!(number(&decoded, "eps.bus_voltage"), 28.41);
    assert_eq!(number(&decoded, "eps.batt_temp"), 20.0);
    assert_eq!(
        value(&decoded, "demosat.eps.mode"),
        SampleValue::Enum("NOMINAL".into())
    );
    assert_eq!(
        value(&decoded, "demosat.eps.uptime"),
        SampleValue::U64(123_456)
    );
    assert_eq!(
        value(&decoded, "demosat.eps.heater"),
        SampleValue::Bool(true)
    );
    assert_eq!(
        value(&decoded, "demosat.ccsds_packet_id.apid"),
        SampleValue::U64(100)
    );

    // In SAFE mode the heater isn't reported, and a negative temperature
    // decodes through the spline: -400 counts is -10 Cel.
    let mut safe = header(100, 8);
    let temperature = (-400_i16 as u16) & 0x0FFF;
    safe.extend_from_slice(&((1_u16 << 12) | temperature).to_be_bytes());
    safe.extend_from_slice(&28_000_u16.to_be_bytes());
    safe.extend_from_slice(&5_u32.to_le_bytes());
    let decoded = sats
        .decode(&Message::new(Payload::Binary(&safe), ground), &registry)
        .unwrap();
    assert_eq!(number(&decoded, "eps.batt_temp"), -10.0);
    assert!(
        !decoded
            .samples
            .iter()
            .any(|s| s.param == "demosat.eps.heater")
    );

    // ADCS_HK: three body rates.
    let mut adcs = header(101, 12);
    for rate in [0.5_f32, -0.25, 1.0] {
        adcs.extend_from_slice(&rate.to_be_bytes());
    }
    let decoded = sats
        .decode(&Message::new(Payload::Binary(&adcs), ground), &registry)
        .unwrap();
    assert_eq!(decoded.packet.name, "ADCS_HK");
    assert_eq!(number(&decoded, "demosat.adcs.rate.x"), 0.5);
    assert_eq!(number(&decoded, "demosat.adcs.rate.y"), -0.25);
    assert_eq!(number(&decoded, "demosat.adcs.rate.z"), 1.0);
}
