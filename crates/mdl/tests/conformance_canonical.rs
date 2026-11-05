//! The language-neutral canonical JSON vectors in `conformance/canonical/`,
//! generated from a real ECMAScript engine (see `generate.mjs`). Every MDL
//! implementation, in any language, runs these same files.

use std::path::PathBuf;

use mdl::canonical::{CanonicalError, parse_strict, to_canonical_string};
use serde_json::Value;

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/canonical")
        .join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    serde_json::from_str(&text).unwrap()
}

#[test]
fn numbers_match_ecmascript() {
    let vectors = fixture("numbers.json")["vectors"]
        .as_array()
        .unwrap()
        .clone();
    assert!(vectors.len() > 2000);
    let mut failures = Vec::new();
    for vector in &vectors {
        let hex = vector[0].as_str().unwrap();
        let expected = vector[1].as_str().unwrap();
        let double = f64::from_bits(u64::from_str_radix(hex, 16).unwrap());
        let ours = to_canonical_string(&Value::from(double)).unwrap();
        if ours != expected {
            failures.push(format!("{hex}: expected {expected}, got {ours}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn documents_canonicalize_like_the_reference() {
    let file = fixture("documents.json");
    let accepted = file["accepted"].as_array().unwrap();
    assert!(accepted.len() > 400);
    for case in accepted {
        let name = case["name"].as_str().unwrap();
        let input = case["input"].as_str().unwrap();
        let value = parse_strict(input.as_bytes()).unwrap_or_else(|e| panic!("{name}: {e}"));
        let ours = to_canonical_string(&value).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(ours, case["output"].as_str().unwrap(), "{name}");
    }
}

/// A case's input bytes: `input` as UTF-8, or `input_hex` decoded.
fn input_bytes(case: &Value) -> Vec<u8> {
    if let Some(text) = case["input"].as_str() {
        return text.as_bytes().to_vec();
    }
    let hex = case["input_hex"].as_str().unwrap();
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn mdl_input_rules_reject_what_they_should() {
    let file = fixture("documents.json");
    let rejected = file["rejected"].as_array().unwrap();
    assert!(rejected.len() >= 14);
    for case in rejected {
        let name = case["name"].as_str().unwrap();
        let result = parse_strict(&input_bytes(case)).and_then(|v| to_canonical_string(&v));
        let kind = match result {
            Ok(text) => panic!("{name}: expected an error, got {text}"),
            Err(CanonicalError::DuplicateKey { .. }) => "duplicate_key",
            Err(CanonicalError::InexactInteger { .. }) => "inexact_integer",
            Err(CanonicalError::InvalidJson { .. }) => "invalid_json",
            Err(other) => panic!("{name}: unexpected error {other}"),
        };
        assert_eq!(kind, case["error"].as_str().unwrap(), "{name}");
    }
}
