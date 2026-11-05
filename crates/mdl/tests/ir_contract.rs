#![cfg(feature = "v1")]

//! The IR's JSON is the contract with frontends in any language. This
//! fixture is written by hand, the way a Python or Go frontend would write
//! it, and no TOML is involved. If a change to the IR breaks it, outside
//! producers would break too, so treat a failure here as a breaking change
//! to MDL (bump `MDL_VERSION`) rather than editing the fixture to match.

use mdl::ir::{Definitions, MDL_VERSION};
use mdl::validate::validate;

const LEGACY_BOX: &str = include_str!("fixtures/ir_legacy_box.json");

#[test]
fn hand_written_ir_json_validates() {
    let definitions: Definitions =
        serde_json::from_str(LEGACY_BOX).expect("the fixture must match the IR");
    assert_eq!(definitions.mdl_version, MDL_VERSION);
    let plan = validate(definitions).unwrap_or_else(|errors| panic!("{errors}"));
    assert_eq!(
        plan.definitions().sets["legacy_box"].packets[0]
            .fields
            .len(),
        3
    );
}

#[test]
fn an_unknown_ir_version_is_rejected() {
    let mut definitions: Definitions = serde_json::from_str(LEGACY_BOX).unwrap();
    definitions.mdl_version = 2;
    let errors = validate(definitions).unwrap_err().to_string();
    assert_eq!(
        errors.trim(),
        "ir: mdl_version: this compiler reads MDL IR version 1, got 2"
    );
}
