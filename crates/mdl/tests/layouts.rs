//! Random valid layouts (`mdl_testkit::layout`): each validates, builds
//! artifacts that read back with their digests, and, loaded from those,
//! decodes every message packed for it to exactly the raw values and
//! samples the testkit worked out from the spec on its own.

use mdl::artifact::{Written, build};
use mdl::bindings::Bindings;
use mdl::decode::{EventTime, SampleValue, Value};
use mdl::ir::Artifact;
use mdl::message::{Message, Payload};
use mdl::read::{Loaded, read_artifact, read_bundle_value};
use mdl::route::FamilySets;
use mdl::time::Timestamp;
use mdl::validate::validate;
use mdl_testkit::layout::{self, Expected, Raw, Sample, Telemetry, telemetry};
use proptest::prelude::*;

proptest! {
    #[test]
    fn a_valid_layout_decodes_what_was_packed_for_it(case in telemetry()) {
        let sets = load(&case)?;
        let bindings = Bindings::from_toml(&format!(
            "mdl_version = \"2.0\"\n\n[[binding]]\nfamily = \"{}\"\nnative_id = \"{}\"\n\
             asset = \"ASSET\"\nset = \"{}\"\neffective_from = \"1980-01-06T00:00:00Z\"\n",
            case.family, case.native_id, case.set
        ))
        .unwrap();
        for message in &case.messages {
            let received = Timestamp::from_unix_nanos(i64::try_from(message.received).unwrap());
            let result =
                sets.decode(&Message::new(Payload::Binary(&message.payload), received), &bindings);
            // An event time that can't be read is the message's error.
            if message.event_time == layout::EventTime::Invalid {
                prop_assert_eq!(result.err().map(|error| error.code()), Some("invalid_time"));
                continue;
            }
            let decoded =
                result.map_err(|error| TestCaseError::fail(format!("{error}: {message:?}")))?;
            prop_assert_eq!(&decoded.packet.name, &message.packet);
            let record: Vec<(String, Raw)> = decoded
                .record
                .iter()
                .map(|(name, value)| (name.to_string(), raw(value)))
                .collect();
            prop_assert_eq!(&record, &message.record, "{:?}", message.payload);
            let samples: Vec<Sample> = decoded
                .samples
                .iter()
                .map(|sample| Sample {
                    param: sample.param.to_string(),
                    value: expected(&sample.value),
                    quality: sample.quality.as_str(),
                })
                .collect();
            prop_assert_eq!(&samples, &message.samples);
            let event_time = match decoded.event_time {
                EventTime::At(at) => {
                    layout::EventTime::At(i128::from(at.seconds()) * 1_000_000_000 + i128::from(at.nanos()))
                }
                _ => layout::EventTime::Ground,
            };
            prop_assert_eq!(&event_time, &message.event_time, "{}", case.bundle["sets"][0]["time"]);
        }
    }
}

/// The case's bundle validated and built, its artifacts read back (each
/// with the digest it was written with), and its family assembled from
/// them as a consumer would.
fn load(case: &Telemetry) -> Result<FamilySets, TestCaseError> {
    let bundle = read_bundle_value(&case.bundle)
        .map_err(|error| TestCaseError::fail(format!("read: {error}")))?
        .value;
    let valid = validate(bundle).map_err(|diagnostics| {
        let shown: Vec<String> = diagnostics
            .iter()
            .map(|d| format!("{:?} {}: {}", d.code, d.path, d.message))
            .collect();
        TestCaseError::fail(format!("invalid: {}\n{:#}", shown.join("\n"), case.bundle))
    })?;
    let artifacts =
        build(&valid).map_err(|error| TestCaseError::fail(format!("build: {error}")))?;
    let reread = |written: &Written| -> Result<Loaded<Artifact>, TestCaseError> {
        let loaded = read_artifact(written.canonical.as_bytes())
            .map_err(|error| TestCaseError::fail(format!("reread: {error}")))?;
        prop_assert_eq!(&loaded.digest, &written.digest);
        Ok(loaded)
    };
    let family = reread(&artifacts.families[case.family.as_str()])?;
    let set = reread(&artifacts.sets[case.set.as_str()])?;
    let catalog = reread(&artifacts.catalog)?;
    FamilySets::new(&family, &[set], &[catalog])
        .map_err(|error| TestCaseError::fail(format!("assemble: {error}")))
}

fn raw(value: &Value<'_>) -> Raw {
    match value {
        Value::Uint(n) => Raw::Uint(*n),
        Value::Int(n) => Raw::Int(*n),
        Value::Float(x) => Raw::Float(*x),
        Value::Bool(b) => Raw::Bool(*b),
        Value::Str(text) => Raw::Str(text.clone()),
        Value::Bytes(bytes) => Raw::Bytes(bytes.clone()),
        other => Raw::Str(format!("not generated: {other:?}")),
    }
}

fn expected(value: &SampleValue) -> Expected {
    match value {
        SampleValue::F64(x) => Expected::F64(*x),
        SampleValue::I64(n) => Expected::I64(*n),
        SampleValue::U64(n) => Expected::U64(*n),
        SampleValue::Bool(b) => Expected::Bool(*b),
        SampleValue::Str(text) => Expected::Str(text.clone()),
        SampleValue::Bytes(bytes) => Expected::Bytes(bytes.clone()),
        SampleValue::Enum(name) => Expected::Enum(name.clone()),
        other => Expected::Str(format!("not generated: {other:?}")),
    }
}
