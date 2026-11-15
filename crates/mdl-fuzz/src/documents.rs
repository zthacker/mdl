//! Targets for JSON documents: canonical JSON itself, bundles and
//! artifacts.

use mdl::canonical::{parse_strict, parse_without_duplicates, to_canonical_string};
use mdl::ir::Digest;
use mdl::read::{read_artifact, read_bundle};
use mdl::source::Sources;

use crate::pipeline::{check_placed, check_spans, load};

/// Any bytes, as JSON (`spec/canonical-json.md`). What reads strictly
/// (§1) also reads without duplicates, and has a canonical form (§2); a
/// canonical form reads strictly and is its own canonical form.
pub fn canonical(data: &[u8]) {
    let strict = parse_strict(data);
    let value = match parse_without_duplicates(data) {
        Ok(value) => value,
        Err(error) => {
            assert!(
                strict.is_err(),
                "read strictly, not without duplicates: {error}"
            );
            assert!(!error.to_string().is_empty(), "{error:?}");
            return;
        }
    };
    let text = match to_canonical_string(&value) {
        Ok(text) => text,
        Err(error) => {
            assert!(
                strict.is_err(),
                "read strictly, has no canonical form: {error}"
            );
            return;
        }
    };
    let again = match parse_strict(text.as_bytes()) {
        Ok(again) => again,
        Err(error) => panic!("canonical text didn't read strictly: {error}: {text}"),
    };
    match to_canonical_string(&again) {
        Ok(twice) => assert_eq!(twice, text, "canonical text isn't its own canonical form"),
        Err(error) => panic!("canonical text has no canonical form: {error}: {text}"),
    }
}

/// Any bytes, as a bundle, read by the reader and by the JSON frontend
/// (`spec/frontends.md` §6), whose diagnostics carry spans: the same strict
/// parse and typed reading, so both read it or neither, and the same
/// bundle. What reads goes on as a consumer takes it.
pub fn read(data: &[u8]) {
    let read = read_bundle(data);
    if let Err(error) = &read {
        assert!(!error.to_string().is_empty(), "{error:?}");
    }
    let Ok(text) = std::str::from_utf8(data) else {
        assert!(read.is_err(), "the reader read text that isn't UTF-8");
        return;
    };
    let mut sources = Sources::new();
    let id = sources.add("bundle.json", text);
    let Some(file) = sources.get(id) else {
        panic!("no file where one was just added");
    };
    match (mdl::json::read(file), read) {
        (Ok(parsed), Ok(loaded)) => {
            assert_eq!(
                parsed.bundle, loaded.value,
                "the frontend's bundle isn't the reader's"
            );
            check_spans(&sources, &parsed.warnings);
            if let Err(diagnostics) = load(parsed.bundle) {
                check_placed(&sources, &parsed.map, &diagnostics);
            }
        }
        (Err(diagnostics), Err(_)) => check_spans(&sources, &diagnostics),
        (Ok(_), Err(error)) => panic!("the JSON frontend read what the reader refused: {error}"),
        (Err(diagnostics), Ok(_)) => {
            panic!("the reader read what the JSON frontend refused: {diagnostics:?}")
        }
    }
}

/// Any bytes, as an artifact. One that reads has its canonical text's
/// digest (`spec/canonical-json.md` §3), and reads from that text again as
/// itself.
pub fn artifact(data: &[u8]) {
    let loaded = match read_artifact(data) {
        Ok(loaded) => loaded,
        Err(error) => {
            assert!(!error.to_string().is_empty(), "{error:?}");
            return;
        }
    };
    assert_eq!(
        loaded.digest,
        Digest::of_canonical(&loaded.canonical),
        "digest"
    );
    let again = match read_artifact(loaded.canonical.as_bytes()) {
        Ok(again) => again,
        Err(error) => panic!("an artifact didn't read from its canonical text: {error}"),
    };
    assert_eq!(
        again.value, loaded.value,
        "read again from its canonical text"
    );
    assert_eq!(
        again.canonical, loaded.canonical,
        "canonical text read again"
    );
    assert_eq!(again.digest, loaded.digest, "digest read again");
}
