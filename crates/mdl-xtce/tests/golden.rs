//! Each example in `examples/xtce` is pinned as the IR bundle it imports
//! as, so any change to what the importer produces shows up as a diff of
//! that bundle. The golden bundles are IR JSON in their own right: each
//! builds the same artifacts as the document it came from.
//!
//! After an intended change to the importer or the examples,
//! `UPDATE_XTCE_GOLDEN=1 cargo test -p morpheus-mdl-xtce --test golden`
//! rewrites them.

use std::path::Path;

use mdl::artifact::{Artifacts, build};
use mdl::ir::Bundle;
use mdl::source::Sources;
use mdl::validate::validate;

/// Whether this run rewrites the goldens. Never in CI, where a stray
/// variable would pass the test by rewriting what it checks.
fn updating() -> bool {
    let update = std::env::var_os("UPDATE_XTCE_GOLDEN").is_some();
    assert!(
        !(update && std::env::var_os("CI").is_some()),
        "UPDATE_XTCE_GOLDEN is set in CI"
    );
    update
}

fn imported(name: &str) -> Bundle {
    let file = format!("{name}.xml");
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/xtce")
        .join(&file);
    let mut sources = Sources::new();
    sources.add(file, std::fs::read_to_string(path).unwrap());
    mdl_xtce::read(sources.iter(), &mdl_xtce::Options::default())
        .unwrap()
        .bundle
}

fn built(bundle: Bundle) -> Artifacts {
    build(&validate(bundle).unwrap()).unwrap()
}

/// Compares (or rewrites) one example's golden, then builds the golden's
/// text as IR JSON. Each example is its own test, so an update run writes
/// each file from one thread and reads nothing another is writing.
fn check(name: &str) {
    let bundle = imported(name);
    let text = format!("{}\n", serde_json::to_string_pretty(&bundle).unwrap());
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.json"));
    if updating() {
        std::fs::write(&path, &text).unwrap();
    } else {
        // A checkout that turns line ends into CRLF still matches.
        let golden = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            golden.replace("\r\n", "\n") == text,
            "the importer no longer gives {} (UPDATE_XTCE_GOLDEN=1 rewrites it after an \
             intended change; review the diff)",
            path.display()
        );
    }
    let mut sources = Sources::new();
    let id = sources.add(path.display().to_string(), text);
    let golden = mdl::json::read(sources.get(id).unwrap()).unwrap().bundle;
    assert_eq!(built(golden), built(bundle), "{name}");
}

#[test]
fn demosat_imports_as_its_golden_bundle() {
    check("demosat");
}

#[test]
fn imager_imports_as_its_golden_bundle() {
    check("imager");
}
