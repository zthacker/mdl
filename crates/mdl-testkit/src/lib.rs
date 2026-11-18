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

pub mod bindings;
mod checksum;
pub mod coverage;
pub mod layout;
pub mod shape;

use serde_json::Value;

/// The IR version a bundle's sets need, which it states as a document
/// does: 2.1 if a `signed` key is anywhere in them, else 2.0.
pub(crate) fn version_for(sets: &Value) -> &'static str {
    fn uses_signed(value: &Value) -> bool {
        match value {
            Value::Object(map) => map.contains_key("signed") || map.values().any(uses_signed),
            Value::Array(items) => items.iter().any(uses_signed),
            _ => false,
        }
    }
    if uses_signed(sets) { "2.1" } else { "2.0" }
}
