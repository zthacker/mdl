//! What the random-layout tests share: loading a generated bundle as a
//! consumer would, and checking a decoded message against what the testkit
//! worked out for it.

use mdl::artifact::{Written, build};
use mdl::decode::{Decoded, EventTime, SampleValue, Value};
use mdl::ir::Artifact;
use mdl::read::{Loaded, read_artifact, read_bundle_value};
use mdl::route::FamilySets;
use mdl::validate::validate;
use mdl_testkit::layout::{self, Expected, Raw, Sample};
use proptest::prelude::*;
use serde_json::Value as Json;

/// The bundle validated and built, its artifacts read back (each with the
/// digest it was written with), and a family assembled from them with its
/// sets.
pub fn load(bundle: &Json, family: &str) -> Result<FamilySets, TestCaseError> {
    let read = read_bundle_value(bundle)
        .map_err(|error| TestCaseError::fail(format!("read: {error}")))?
        .value;
    let valid = validate(read).map_err(|diagnostics| {
        let shown: Vec<String> = diagnostics
            .iter()
            .map(|d| format!("{:?} {}: {}", d.code, d.path, d.message))
            .collect();
        TestCaseError::fail(format!("invalid: {}\n{bundle:#}", shown.join("\n")))
    })?;
    let artifacts =
        build(&valid).map_err(|error| TestCaseError::fail(format!("build: {error}")))?;
    let reread = |written: &Written| -> Result<Loaded<Artifact>, TestCaseError> {
        let loaded = read_artifact(written.canonical.as_bytes())
            .map_err(|error| TestCaseError::fail(format!("reread: {error}")))?;
        prop_assert_eq!(&loaded.digest, &written.digest);
        Ok(loaded)
    };
    let sets = artifacts
        .sets
        .values()
        .filter(|written| matches!(&written.artifact, Artifact::Set { set, .. } if set.family == family))
        .map(reread)
        .collect::<Result<Vec<_>, _>>()?;
    let family = reread(&artifacts.families[family])?;
    let catalog = reread(&artifacts.catalog)?;
    FamilySets::new(&family, &sets, &[catalog])
        .map_err(|error| TestCaseError::fail(format!("assemble: {error}")))
}

/// The decoded packet, every raw value, every sample and the event time
/// are what the testkit packed.
pub fn check(decoded: &Decoded<'_>, message: &layout::Message) -> Result<(), TestCaseError> {
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
    prop_assert_eq!(&event_time(&decoded.event_time), &message.event_time);
    Ok(())
}

/// A timestamp as Unix nanoseconds.
pub fn nanos(at: mdl::time::Timestamp) -> i128 {
    i128::from(at.seconds()) * 1_000_000_000 + i128::from(at.nanos())
}

fn event_time(time: &EventTime) -> layout::EventTime {
    match time {
        EventTime::At(at) => layout::EventTime::At(nanos(*at)),
        EventTime::SinceBoot { value, unit } => {
            let named = serde_json::to_value(unit).unwrap_or_default();
            let unit = match named.as_str() {
                Some("s") => "s",
                Some("ms") => "ms",
                Some("us") => "us",
                Some("ns") => "ns",
                _ => "other",
            };
            layout::EventTime::SinceBoot {
                value: *value,
                unit,
            }
        }
        _ => layout::EventTime::Ground,
    }
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
