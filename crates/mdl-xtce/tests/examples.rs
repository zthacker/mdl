//! The examples in `examples/xtce` import, validate and build, decode
//! packets the way their comments say, and encode commands to the bytes
//! they describe.

use std::path::Path;

use mdl::v2::artifact::{Artifacts, Written, build};
use mdl::v2::bindings::Bindings;
use mdl::v2::decode::{Decoded, Quality, SampleValue};
use mdl::v2::encode::{ArgumentValue, CommandRequest};
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
            "106:27 SpaceSystem EPS's shortDescription isn't imported: MDL keeps descriptions \
             of catalog params, commands and arguments only",
            "261:28 SpaceSystem ADCS's shortDescription isn't imported: MDL keeps descriptions \
             of catalog params, commands and arguments only",
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

#[test]
fn demosat_encodes_its_commands_and_decodes_them_back() {
    let (artifacts, _) = example("demosat.xml");
    let sats = family_sets(&artifacts, "demosat");
    let ground: Timestamp = "2026-06-01T12:00:00Z".parse().unwrap();
    // The telecommand header for an APID: version 0, type 1 (a command),
    // no secondary header, standalone, the count and length left as zeros.
    let header = |apid: u16| {
        let mut bytes = (0x1000 | apid).to_be_bytes().to_vec();
        bytes.extend_from_slice(&[0xC0, 0x00, 0x00, 0x00]);
        bytes
    };
    let encode = |request: CommandRequest| sats.encode(&request).map(|e| e.payload);
    let decode = |bytes: &[u8]| {
        let decoded = sats
            .decode_command("demosat", &Message::new(Payload::Binary(bytes), ground))
            .unwrap();
        (decoded.command().to_string(), decoded.arguments)
    };

    // SetMode: opcode 1 to APID 100 (both assigned along the way), then the
    // mode's raw value and the delay.
    let set_mode = CommandRequest::new("demosat", "eps.set_mode")
        .with_argument("mode", "NOMINAL")
        .with_argument("delay", 30_u64);
    let bytes = encode(set_mode).unwrap();
    let mut expected = header(100);
    expected.extend_from_slice(&[1, 2, 0x00, 0x1E]);
    assert_eq!(bytes, expected);
    assert_eq!(
        decode(&bytes),
        (
            "eps.set_mode".to_string(),
            vec![
                ("mode", ArgumentValue::Text("NOMINAL".into())),
                ("delay", ArgumentValue::Uint(30))
            ]
        )
    );
    // The delay's type gives its default and its valid range.
    let defaulted = sats
        .encode(&CommandRequest::new("demosat", "eps.set_mode").with_argument("mode", "SAFE"))
        .unwrap();
    assert_eq!(defaulted.defaulted, ["delay"]);
    assert_eq!(&defaulted.payload[6..], [1, 1, 0, 0]);
    let late = CommandRequest::new("demosat", "eps.set_mode")
        .with_argument("mode", "SAFE")
        .with_argument("delay", 3601_u64);
    assert_eq!(encode(late).unwrap_err().code(), "out_of_range");

    // SetHeater: opcode 2, the switch's initial value, True, as 1, and
    // the minutes, which may be anything their 16 bits hold.
    let heater = |minutes: u64| {
        encode(
            CommandRequest::new("demosat", "demosat.eps.setheater")
                .with_argument("minutes", minutes),
        )
    };
    let mut expected = header(100);
    expected.extend_from_slice(&[2, 1, 0x00, 0x5A]);
    assert_eq!(heater(90).unwrap(), expected);
    assert_eq!(heater(65_536).unwrap_err().code(), "out_of_range");

    // Point: APID 101, opcode 1, the target NUL-padded to eight bytes, and
    // the rate as a 32-bit float, which must be above 0.
    let point = CommandRequest::new("demosat", "demosat.adcs.point")
        .with_argument("target", "SUN")
        .with_argument("rate", 0.25);
    let bytes = encode(point).unwrap();
    let mut expected = header(101);
    expected.push(1);
    expected.extend_from_slice(b"SUN\0\0\0\0\0");
    expected.extend_from_slice(&0.25_f32.to_be_bytes());
    assert_eq!(bytes, expected);
    assert_eq!(
        decode(&bytes).1,
        [
            ("target", ArgumentValue::Text("SUN".into())),
            ("rate", ArgumentValue::Float(0.25))
        ]
    );
    let still = CommandRequest::new("demosat", "demosat.adcs.point")
        .with_argument("target", "SUN")
        .with_argument("rate", 0.0);
    assert_eq!(encode(still).unwrap_err().code(), "out_of_range");
}

/// Every good sample of `param`, by index.
fn samples(decoded: &Decoded<'_>, param: &str) -> Vec<(Option<u64>, SampleValue)> {
    decoded
        .samples
        .iter()
        .filter(|s| s.param == param)
        .map(|s| {
            assert_eq!(s.quality, Quality::Good, "{param}");
            (s.index, s.value.clone())
        })
        .collect()
}

#[test]
fn imager_decodes_sizes_the_data_gives() {
    let (artifacts, warnings) = example("imager.xml");
    assert_eq!(
        warnings,
        [
            "48:21 Variable's maxSizeInBits isn't imported: MDL doesn't bound a variable string's size"
        ]
    );
    let registry = Bindings::from_toml(
        r#"
mdl_version = "2.0"

[[binding]]
family = "imager"
native_id = "Imager"
asset = "CAM-1"
set = "imager"
effective_from = "2026-01-01T00:00:00Z"
"#,
    )
    .unwrap();
    let ground: Timestamp = "2026-06-01T12:00:00Z".parse().unwrap();
    let imager = family_sets(&artifacts, "imager");
    let decode = |bytes: &[u8]| {
        imager
            .decode(&Message::new(Payload::Binary(bytes), ground), &registry)
            .unwrap()
    };

    // A frame without the secondary header: three pixels, then "STAR" in a
    // Pascal string.
    let mut frame = vec![1, 0, 3];
    for raw in [10_u16, 20, 4000] {
        frame.extend_from_slice(&raw.to_be_bytes());
    }
    frame.push(4);
    frame.extend_from_slice(b"STAR");
    let decoded = decode(&frame);
    assert_eq!(decoded.packet.name, "Frame");
    assert_eq!(
        samples(&decoded, "imager.pixel"),
        [
            (Some(0), SampleValue::F64(5.0)),
            (Some(1), SampleValue::F64(10.0)),
            (Some(2), SampleValue::F64(2000.0))
        ]
    );
    assert_eq!(
        value(&decoded, "imager.name"),
        SampleValue::Str("STAR".into())
    );
    assert!(samples(&decoded, "imager.time").is_empty());

    // The same with the secondary header: the time, then one pixel.
    let mut timed = vec![1, 1];
    timed.extend_from_slice(&1_000_u32.to_be_bytes());
    timed.extend_from_slice(&[1, 0, 2, 0]);
    let decoded = decode(&timed);
    assert_eq!(value(&decoded, "imager.time"), SampleValue::U64(1_000));
    assert_eq!(
        samples(&decoded, "imager.pixel"),
        [(Some(0), SampleValue::F64(1.0))]
    );
    assert_eq!(
        value(&decoded, "imager.name"),
        SampleValue::Str(String::new())
    );

    // Two targets, then three temperatures, each followed by a spare byte.
    let mut report = vec![2, 0, 2];
    report.push(7);
    report.extend_from_slice(&1_500_u16.to_be_bytes());
    report.push(9);
    report.extend_from_slice(&250_u16.to_be_bytes());
    for temperature in [-5_i8, 0, 21] {
        report.push(temperature as u8);
        report.push(0xEE);
    }
    report.push(3);
    let decoded = decode(&report);
    assert_eq!(decoded.packet.name, "TargetReport");
    assert_eq!(
        samples(&decoded, "imager.targets.id"),
        [
            (Some(0), SampleValue::U64(7)),
            (Some(1), SampleValue::U64(9))
        ]
    );
    assert_eq!(
        samples(&decoded, "imager.targets.range"),
        [
            (Some(0), SampleValue::F64(1500.0)),
            (Some(1), SampleValue::F64(250.0))
        ]
    );
    assert_eq!(
        samples(&decoded, "imager.detector"),
        [
            (Some(0), SampleValue::F64(-5.0)),
            (Some(1), SampleValue::F64(0.0)),
            (Some(2), SampleValue::F64(21.0))
        ]
    );
    // The mode follows the last spare byte.
    assert_eq!(value(&decoded, "imager.mode"), SampleValue::U64(3));
}
