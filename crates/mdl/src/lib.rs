//! MDL, the Morpheus Definition Language: definitions of what a fleet's
//! messages mean, checked against a canonical catalog, built into
//! content-addressed artifacts, and used to decode telemetry and encode
//! commands.
//!
//! ```text
//! TOML (mdl-toml) ─┐                                           ┌─► route, decode ─► samples
//! XTCE (mdl-xtce) ─┼─► source::merge ─► validate ─► artifact ─┤
//! IR JSON (json) ──┘    one bundle       Validated   artifacts └─► encode ─► payloads
//! ```
//!
//! - [`ir`]: the IR: the catalog, families and sets, and the artifacts
//!   they're written as.
//! - [`json`], [`read`] and [`source`]: reading bundles and artifacts,
//!   with where each value came from; [`source::merge`] makes bundles from
//!   several frontends one.
//! - [`validate`]: every rule, with every problem reported at once;
//!   [`diagnostics`]: their stable codes.
//! - [`artifact`]: content-addressed artifacts, rebuilds that keep sets'
//!   identities, and assembly from storage; [`canonical`]: the JSON form
//!   identities are taken from (RFC 8785).
//! - [`route`] and [`decode`]: which asset sent a message, which set
//!   decodes it, and its catalog samples; [`bindings`]: which asset each
//!   native id is; [`message`]: what a decoder is given; [`time`]:
//!   instants and event times; [`units`]: unit conversion.
//! - [`encode`]: catalog commands as command packets; [`checksum`]: the
//!   checksums both directions compute.
//! - [`layout`]: where each entry of a set sits, for tools.
//!
//! The package is `morpheus-mdl` (`mdl` is taken on crates.io); the crate
//! is imported as `mdl`. The specifications in the repository's `spec`
//! directory are normative; these docs describe this implementation of
//! them.

pub mod artifact;
pub mod bindings;
pub(crate) mod bits;
pub mod canonical;
pub mod checksum;
pub(crate) mod convert;
pub mod decode;
pub mod diagnostics;
pub mod encode;
pub mod ir;
pub mod json;
pub mod layout;
pub mod message;
pub mod read;
pub mod route;
pub mod source;
pub(crate) mod text;
pub mod time;
pub mod units;
pub mod validate;
