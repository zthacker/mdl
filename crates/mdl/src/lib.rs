//! MDL, the Morpheus Definition Language: frontends, the IR, validation, the
//! compiled plan, and the interpreter that applies a plan to raw messages.
//!
//! ```text
//! TOML files ──► frontend::toml ──┐
//! (XTCE, C headers, a database:   ├──► IR ──► validate ──► Plan (JSON, SHA-256 = def_version)
//!  more frontends, later)  ───────┘                          └──► interpret (bits, conversions)
//! ```

pub mod canonical;
pub mod v2;

// v1, removed at the end of Phase 1 (see the `v1` feature in Cargo.toml).
#[cfg(feature = "v1")]
pub mod bits;
#[cfg(feature = "v1")]
pub mod diagnostics;
#[cfg(feature = "v1")]
pub mod frontend;
#[cfg(feature = "v1")]
pub mod interpret;
#[cfg(feature = "v1")]
pub mod ir;
#[cfg(feature = "v1")]
pub mod plan;
#[cfg(feature = "v1")]
pub mod validate;
