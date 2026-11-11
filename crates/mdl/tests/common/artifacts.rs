//! Loading a generated bundle as a consumer would, and reading a record's
//! raw values: what every random-layout test needs. Tests that check
//! telemetry take it through `common`; others include this file alone, so
//! each test binary compiles only what it uses.

use mdl::artifact::{Written, build};
use mdl::decode::{Record, Value};
use mdl::ir::Artifact;
use mdl::read::{Loaded, read_artifact, read_bundle_value};
use mdl::route::FamilySets;
use mdl::validate::validate;
use mdl_testkit::layout::Raw;
use proptest::prelude::*;
use serde_json::Value as Json;

/// The bundle validated and built, its artifacts read back (each with the
/// digest it was written with), and a family assembled from them with its
/// sets, but for those a consumer hasn't loaded.
pub fn load(bundle: &Json, family: &str, unloaded: &[String]) -> Result<FamilySets, TestCaseError> {
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
        .filter(|written| {
            matches!(&written.artifact, Artifact::Set { set, .. }
                if set.family == family && !unloaded.contains(&set.name))
        })
        .map(reread)
        .collect::<Result<Vec<_>, _>>()?;
    let family = reread(&artifacts.families[family])?;
    let catalog = reread(&artifacts.catalog)?;
    FamilySets::new(&family, &sets, &[catalog])
        .map_err(|error| TestCaseError::fail(format!("assemble: {error}")))
}

/// Each field's raw value, by name, in record order.
pub fn record(record: &Record<'_>) -> Vec<(String, Raw)> {
    record
        .iter()
        .map(|(name, value)| (name.to_string(), raw(value)))
        .collect()
}

fn raw(value: &Value<'_>) -> Raw {
    match value {
        Value::Uint(n) => Raw::Uint(*n),
        Value::Int(n) => Raw::Int(*n),
        Value::Float(x) => Raw::Float(*x),
        Value::Bool(b) => Raw::Bool(*b),
        Value::Str(text) => Raw::Str(text.clone()),
        Value::Bytes(bytes) => Raw::Bytes(bytes.clone()),
        Value::Array(values) => Raw::Array(values.iter().map(raw).collect()),
        Value::Record(fields) => Raw::Record(record(fields)),
        other => Raw::Str(format!("not generated: {other:?}")),
    }
}
