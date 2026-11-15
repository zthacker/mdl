//! What every target that reaches a bundle does with it, and the checks
//! the targets share.

use std::fmt::Debug;
use std::sync::OnceLock;

use mdl::artifact::{Written, build};
use mdl::diagnostics::Diagnostics;
use mdl::ir::{Artifact, Bundle};
use mdl::read::{Loaded, read_artifact, read_bundle};
use mdl::route::FamilySets;
use mdl::source::{SourceDiagnostic, SourceMap, Sources, Span};
use mdl::validate::{Validated, validate};

/// What a consumer does with a bundle: validate it, build its artifacts,
/// read each back, and assemble each family with the sets built for it.
/// Past validation, whose diagnostics are returned, every step must work:
/// validation rules out every build error, an artifact built reads back as
/// itself with the digest it was built with, and a family assembles.
pub(crate) fn load(bundle: Bundle) -> Result<(), Diagnostics> {
    let valid = validate(bundle)?;
    let artifacts = match build(&valid) {
        Ok(artifacts) => artifacts,
        Err(error) => panic!("a valid bundle didn't build: {error}"),
    };
    let catalog = read_back(&artifacts.catalog);
    let sets: Vec<Loaded<Artifact>> = artifacts.sets.values().map(read_back).collect();
    for (name, written) in &artifacts.families {
        let family = read_back(written);
        let its_sets: Vec<Loaded<Artifact>> = sets
            .iter()
            .filter(
                |loaded| matches!(&loaded.value, Artifact::Set { set, .. } if set.family == *name),
            )
            .cloned()
            .collect();
        if let Err(error) = FamilySets::new(&family, &its_sets, std::slice::from_ref(&catalog)) {
            panic!("family {name} didn't assemble from the artifacts built for it: {error}");
        }
    }
    Ok(())
}

/// An artifact read from the canonical text it was built as, which gives
/// it back with the same digest.
fn read_back(written: &Written) -> Loaded<Artifact> {
    let loaded = match read_artifact(written.canonical.as_bytes()) {
        Ok(loaded) => loaded,
        Err(error) => panic!("a built artifact didn't read back: {error}"),
    };
    assert_eq!(loaded.value, written.artifact, "a built artifact read back");
    assert_eq!(loaded.digest, written.digest, "a built artifact's digest");
    assert_eq!(
        loaded.canonical, written.canonical,
        "a built artifact's text"
    );
    loaded
}

/// The kitchen sink (`crates/mdl/tests/fixtures/full_bundle.json`),
/// validated once.
pub(crate) fn kitchen_sink() -> &'static Validated {
    static KITCHEN_SINK: OnceLock<Validated> = OnceLock::new();
    KITCHEN_SINK.get_or_init(|| {
        let bytes = include_bytes!("../../mdl/tests/fixtures/full_bundle.json");
        let bundle = match read_bundle(bytes) {
            Ok(loaded) => loaded.value,
            Err(error) => panic!("the kitchen sink didn't read: {error}"),
        };
        match validate(bundle) {
            Ok(valid) => valid,
            Err(diagnostics) => panic!("the kitchen sink isn't valid: {diagnostics:?}"),
        }
    })
}

/// The input as one or more files, split at NUL bytes, which neither TOML
/// nor XML text may hold; none if one isn't UTF-8.
pub(crate) fn files(data: &[u8], extension: &str) -> Option<Sources> {
    let mut sources = Sources::new();
    for (index, part) in data.split(|byte| *byte == 0).enumerate() {
        let text = std::str::from_utf8(part).ok()?;
        sources.add(format!("{index}.{extension}"), text);
    }
    Some(sources)
}

/// A frontend's diagnostics (`spec/frontends.md` §3): each has a code and a
/// message, and each span, its own and its first location's, lies in a
/// file read.
pub(crate) fn check_spans<'d>(
    sources: &Sources,
    diagnostics: impl IntoIterator<Item = &'d SourceDiagnostic>,
) {
    for diagnostic in diagnostics {
        assert!(
            !diagnostic.code.is_empty() && !diagnostic.message.is_empty(),
            "{diagnostic:?}"
        );
        for span in [diagnostic.span, diagnostic.first].into_iter().flatten() {
            check_span(sources, span, diagnostic);
        }
    }
}

/// A frontend's own diagnostics come in file order, then by position
/// within each file (`spec/frontends.md` §4).
pub(crate) fn check_order(diagnostics: &[SourceDiagnostic]) {
    let placed: Vec<Span> = diagnostics.iter().filter_map(|d| d.span).collect();
    for pair in placed.windows(2) {
        let (earlier, later) = (pair[0], pair[1]);
        assert!(
            (earlier.source, earlier.start) <= (later.source, later.start),
            "diagnostics out of order: {diagnostics:?}"
        );
    }
}

/// Validation's diagnostics placed in the text through the frontend's
/// source map, as morphc places them: each has a message, and the span its
/// path finds lies in a file read.
pub(crate) fn check_placed(sources: &Sources, map: &SourceMap, diagnostics: &Diagnostics) {
    for diagnostic in diagnostics.iter() {
        assert!(!diagnostic.message.is_empty(), "{diagnostic:?}");
        if let Some(span) = map.find(&diagnostic.path) {
            check_span(sources, span, diagnostic);
        }
    }
}

/// The span is a byte range of a file read, and has a line and a column
/// there, both counted from 1.
fn check_span(sources: &Sources, span: Span, what: &dyn Debug) {
    let Some(file) = sources.get(span.source) else {
        panic!("a span in no file read: {what:?}");
    };
    assert!(
        span.start <= span.end && span.end <= file.text().len(),
        "span {}..{} of {} bytes: {what:?}",
        span.start,
        span.end,
        file.text().len()
    );
    let location = file.location(span.start);
    assert!(
        location.line >= 1 && location.column >= 1,
        "{location:?}: {what:?}"
    );
}
