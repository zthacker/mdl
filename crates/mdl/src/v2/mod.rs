//! MDL IR v2 and everything built on it.
//!
//! Built alongside v1 during Phase 1 so every step compiles and passes on
//! its own. v2 must never depend on v1: the v1 modules sit behind the `v1`
//! cargo feature, and `just check` builds the crate without it, so the
//! compiler rejects any crossing. When Phase 1 is done, these modules
//! replace the top-level v1 ones and this module goes away.

pub mod artifact;
pub mod bindings;
pub(crate) mod bits;
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
