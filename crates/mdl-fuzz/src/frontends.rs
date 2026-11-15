//! Targets for the text people write: bindings files, and definitions in
//! TOML and XTCE.

use mdl::bindings::Bindings;
use mdl::source::{Parsed, SourceDiagnostic, Sources};
use mdl_xtce::Options;

use crate::pipeline::{check_order, check_placed, check_spans, files, kitchen_sink, load};

/// A bindings file (`spec/bindings.md` §1), checked against the kitchen
/// sink when it reads (§6). Every error and diagnostic has a message.
pub fn bindings(data: &[u8]) {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    match Bindings::from_toml(text) {
        Ok(bindings) => {
            if let Err(diagnostics) = bindings.check(kitchen_sink()) {
                for diagnostic in diagnostics.iter() {
                    assert!(!diagnostic.message.is_empty(), "{diagnostic:?}");
                }
            }
        }
        Err(error) => assert!(!error.to_string().is_empty(), "{error:?}"),
    }
}

/// TOML files (`spec/toml.md`), split at NUL bytes and merged in order.
/// What reads goes on as a consumer takes it.
pub fn toml(data: &[u8]) {
    let Some(sources) = files(data, "toml") else {
        return;
    };
    frontend(&sources, mdl_toml::read(sources.iter()));
}

/// XTCE documents (`spec/xtce.md`), split at NUL bytes, read with the
/// default options. What reads goes on as a consumer takes it.
pub fn xtce(data: &[u8]) {
    let Some(sources) = files(data, "xml") else {
        return;
    };
    frontend(
        &sources,
        mdl_xtce::read(sources.iter(), &Options::default()),
    );
}

/// What a frontend read, with every span a diagnostic gives inside a file
/// read, and its own diagnostics in order (`spec/frontends.md` §3, §4).
fn frontend(sources: &Sources, read: Result<Parsed, Vec<SourceDiagnostic>>) {
    match read {
        Ok(parsed) => {
            check_spans(sources, &parsed.warnings);
            check_order(&parsed.warnings);
            if let Err(diagnostics) = load(parsed.bundle) {
                check_placed(sources, &parsed.map, &diagnostics);
            }
        }
        Err(diagnostics) => {
            check_spans(sources, &diagnostics);
            check_order(&diagnostics);
        }
    }
}
