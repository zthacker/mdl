//! The compiled artifact: definitions that passed validation, plus their
//! JSON form and its SHA-256, the `def_version`.
//!
//! The plan's JSON *is* MDL IR (the validated `Definitions`), so a plan can
//! be fed back to `morphc build --ir`, and loading one re-validates it.
//! `def_version` names the content: the same definitions always hash the
//! same, and any change gives a new version. Every output records it, which
//! is what makes reprocessing traceable (DDIA Ch. 11: derived data names the
//! code and inputs that produced it).

use sha2::{Digest, Sha256};

use crate::diagnostics::{Diagnostic, Errors};
use crate::ir::Definitions;

/// Validated definitions. The constructor is crate-private and only
/// `validate` calls it, so code that takes a `Plan` can
/// never be handed unchecked definitions.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    definitions: Definitions,
    /// Compact JSON of `definitions`: exactly the bytes `def_version` hashes.
    json: Vec<u8>,
    /// Lowercase hex SHA-256 of `json`.
    def_version: String,
}

impl Plan {
    /// Serialize and hash once, here: consumers typically stamp
    /// `def_version` on every message they produce.
    pub(crate) fn new(definitions: Definitions) -> Result<Self, Errors> {
        let json = serde_json::to_vec(&definitions).map_err(|err| {
            Errors(vec![Diagnostic {
                source: "plan".into(),
                location: String::new(),
                message: format!("can't serialize: {err}"),
            }])
        })?;
        let def_version = to_hex(&Sha256::digest(&json));
        Ok(Plan {
            definitions,
            json,
            def_version,
        })
    }

    /// Load a plan (or any MDL IR) from JSON. It's validated again: a plan
    /// read from KV or disk is input like any other.
    pub fn from_json(bytes: &[u8]) -> Result<Plan, Errors> {
        let definitions: Definitions = serde_json::from_slice(bytes).map_err(|err| {
            Errors(vec![Diagnostic {
                source: "plan".into(),
                location: String::new(),
                message: format!("isn't valid MDL IR JSON: {err}"),
            }])
        })?;
        crate::validate::validate(definitions)
    }

    pub fn definitions(&self) -> &Definitions {
        &self.definitions
    }

    /// The compact JSON that `def_version` is the hash of.
    pub fn json(&self) -> &[u8] {
        &self.json
    }

    pub fn def_version(&self) -> &str {
        &self.def_version
    }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY_BOX: &str = include_str!("../tests/fixtures/ir_legacy_box.json");

    fn legacy_box() -> Plan {
        Plan::from_json(LEGACY_BOX.as_bytes()).unwrap()
    }

    #[test]
    fn def_version_is_the_sha256_of_the_json() {
        let plan = legacy_box();
        assert_eq!(plan.def_version().len(), 64);
        assert_eq!(plan.def_version(), to_hex(&Sha256::digest(plan.json())));
    }

    /// Golden value. If this fails, the plan's bytes changed: a field was
    /// added, renamed or reordered in `ir.rs`, or serde's output changed.
    /// That's a new def_version for every deployment, so update it on
    /// purpose, never just to make the test pass.
    #[test]
    fn def_version_is_stable_across_builds() {
        assert_eq!(
            legacy_box().def_version(),
            "3a22f8b24a02dada552ea6e7f3c2223680fd4196af92ae3934832c43d9037c86"
        );
    }

    #[test]
    fn formatting_doesnt_matter_but_content_does() {
        let plan = legacy_box();
        // Same content, compact instead of pretty: same version.
        let compact = Plan::from_json(plan.json()).unwrap();
        assert_eq!(compact.def_version(), plan.def_version());
        assert_eq!(compact, plan);
        // One number changes: new version.
        let edited = LEGACY_BOX.replacen("\"scale\": 0.001", "\"scale\": 0.01", 1);
        assert_ne!(edited, LEGACY_BOX, "test bug: the edit must apply");
        let edited = Plan::from_json(edited.as_bytes()).unwrap();
        assert_ne!(edited.def_version(), plan.def_version());
    }

    #[test]
    fn loading_validates() {
        let newer = LEGACY_BOX.replacen("\"mdl_version\": 1", "\"mdl_version\": 2", 1);
        let errors = Plan::from_json(newer.as_bytes()).unwrap_err().to_string();
        assert!(errors.contains("MDL IR version 1, got 2"), "{errors}");

        let errors = Plan::from_json(b"{ not json").unwrap_err().to_string();
        assert!(
            errors.starts_with("plan: isn't valid MDL IR JSON"),
            "{errors}"
        );
    }
}
