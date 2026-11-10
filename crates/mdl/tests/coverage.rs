//! The full fixture is the kitchen sink: it uses every kind and option the
//! IR has, so the tests that read, validate, build and decode it exercise
//! them all, and the TOML twin proves the TOML frontend spells each one.
//! Its `sink` families and sets are there to cover forms, not to model a
//! device: a length of `2n + 1` bytes, one conditional field per operator.
//!
//! When the IR gains a kind or an option, add it to
//! `mdl_testkit::coverage::REQUIRED` and to the fixture; this test names
//! whatever the fixture leaves out, and any kind the fixture uses that
//! `REQUIRED` doesn't list.

use std::collections::BTreeSet;

use mdl_testkit::coverage::{REQUIRED, features};
use serde_json::Value;

const FULL_BUNDLE: &str = include_str!("fixtures/full_bundle.json");

#[test]
fn the_full_fixture_uses_every_feature_of_the_ir() {
    let value: Value = serde_json::from_str(FULL_BUNDLE).unwrap();
    let found = features(&value);
    let required: BTreeSet<String> = REQUIRED.iter().map(|f| f.to_string()).collect();
    let missing: Vec<&String> = required.difference(&found).collect();
    assert!(
        missing.is_empty(),
        "the full fixture doesn't use: {missing:?}"
    );
    // Keys are many and most are mandatory, so only the ones worth naming
    // are listed; every kind and enumerated value is.
    let unlisted: Vec<&String> = found
        .difference(&required)
        .filter(|feature| !feature.starts_with("key:"))
        .collect();
    assert!(
        unlisted.is_empty(),
        "the full fixture uses what REQUIRED doesn't list: {unlisted:?}"
    );
}
