//! What MDL's fuzz targets check. Each target takes the fuzzer's bytes and
//! panics when something the spec or the API promises doesn't hold, which
//! is how libFuzzer learns of it; a panic inside MDL is a bug on its own.
//!
//! `fuzz/` (cargo-fuzz, nightly) wraps each target in a `fuzz_target!`.
//! This crate's replay test runs every committed input through them on
//! stable, in `just check`: the seeds (`fuzz/seeds/<target>`), the corpus
//! grown from them (`fuzz/corpus/<target>`) and the crashes found and fixed
//! (`fuzz/regressions/<target>`).

mod documents;
mod frontends;
mod generated;
mod input;
mod kitchen;
mod messages;
mod pipeline;
mod values;

pub use documents::{artifact, canonical, read};
pub use frontends::{bindings, toml, xtce};
pub use generated::{layouts, shapes};
pub use messages::{decode, decode_command, encode};
pub use values::{timestamp, units};

/// A target: takes the fuzzer's bytes, panics on what it finds.
pub type Target = fn(&[u8]);

/// Every target, by the name its binary in `fuzz/` and its directories
/// under `fuzz/seeds`, `fuzz/corpus` and `fuzz/regressions` go by.
pub const TARGETS: [(&str, Target); 13] = [
    ("artifact", artifact),
    ("bindings", bindings),
    ("canonical", canonical),
    ("decode", decode),
    ("decode_command", decode_command),
    ("encode", encode),
    ("layouts", layouts),
    ("read", read),
    ("shapes", shapes),
    ("timestamp", timestamp),
    ("toml", toml),
    ("units", units),
    ("xtce", xtce),
];

/// Runs every target on an input of its own before libFuzzer runs any, so
/// what's built on first use and kept (the kitchen sink, and whatever MDL
/// and its dependencies build once) isn't credited to the first input run.
/// Without it, minimizing a corpus depends on the order its inputs run in,
/// and minimizing it again removes a few more.
pub fn warm_up() {
    artifact(b"{}");
    bindings(include_bytes!(
        "../../../fuzz/seeds/bindings/kitchen_sink.toml"
    ));
    canonical(br#"{"b":[1,2.5,"x\n"],"a":null}"#);
    read(include_bytes!("../../mdl/tests/fixtures/full_bundle.json"));
    timestamp(b"2026-06-01T14:00:00.25+02:00");
    toml(include_bytes!(
        "../../mdl-toml/tests/fixtures/full_bundle.toml"
    ));
    units(b"km/h\nm/s");
    xtce(include_bytes!("../../../examples/xtce/imager.xml"));
    decode(&[0; 8]);
    decode_command(&[0; 8]);
    encode(&[0, 1, 1, 0, 5, 0x80]);
    shapes(&[0; 32]);
    for kind in 0..4 {
        layouts(&[kind]);
    }
}
