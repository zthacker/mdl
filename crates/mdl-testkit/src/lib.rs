//! Generators of MDL IR for the workspace's property tests. Never
//! published: it has no API to keep stable, and each crate's integration
//! tests (and the fuzz crate) use it as a dev-dependency.
//!
//! Generators produce IR JSON, the form every frontend reads, rather than
//! IR values: the IR's types are `#[non_exhaustive]`, so no crate but
//! `mdl` can build them, and JSON is what a round trip compares anyway.
//!
//! - [`shape`]: any bundle the IR's shape allows, valid or not, spelled as
//!   the IR writes it.
//! - [`coverage`]: the features of the IR a bundle uses, and the list of
//!   them all, which the full fixture and the generators must each reach.
//! - [`layout`]: valid layouts and messages packed for them by this
//!   crate's own reading of the decoding spec, with what each decodes to;
//!   [`layout::command`], command layouts and requests, with the payload
//!   each encodes to by the encoding spec and what it decodes back to.
//! - [`bindings`]: bindings files, with what they resolve to and the
//!   diagnostics a reader gives them.
//! - [`conformance`]: decoding conformance cases written from [`layout`]'s
//!   layouts, which the `conformance` binary writes to
//!   `conformance/decoding/oracle/` (`spec/conformance.md` §2.3).

pub mod bindings;
mod checksum;
pub mod conformance;
pub mod coverage;
pub mod layout;
pub mod shape;

use serde_json::Value;

/// The IR version a bundle's sets need, which it states as a document
/// does: 2.3 if a command packet's argument lists the states it accepts
/// or says whether it's required, 2.2 if a command packet has `arguments`
/// of its own (a map, where a catalog command's are a list), 2.1 if a
/// `signed` key is anywhere in them, else 2.0.
pub(crate) fn version_for(sets: &Value) -> &'static str {
    fn uses(value: &Value, key: &str, holds: fn(&Value) -> bool) -> bool {
        match value {
            Value::Object(map) => {
                map.get(key).is_some_and(holds) || map.values().any(|v| uses(v, key, holds))
            }
            Value::Array(items) => items.iter().any(|v| uses(v, key, holds)),
            _ => false,
        }
    }
    // A command packet's argument that lists the states it accepts, or
    // says whether it's required.
    let uses_2_3 = |arguments: &Value| {
        arguments.as_object().is_some_and(|arguments| {
            arguments
                .values()
                .any(|limits| limits.get("states").is_some() || limits.get("required").is_some())
        })
    };
    if uses(sets, "arguments", uses_2_3) {
        "2.3"
    } else if uses(sets, "arguments", Value::is_object) {
        "2.2"
    } else if uses(sets, "signed", |_| true) {
        "2.1"
    } else {
        "2.0"
    }
}
