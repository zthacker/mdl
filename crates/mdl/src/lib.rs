//! MDL, the Morpheus Definition Language: frontends, the IR, validation, the
//! compiled plan, and the interpreter that applies a plan to raw messages.
//!
//! ```text
//! TOML files ──► frontend::toml ──┐
//! (XTCE, C headers, a database:   ├──► IR ──► validate ──► Plan (JSON, SHA-256 = def_version)
//!  more frontends, later)  ───────┘                          └──► interpret (bits, conversions)
//! ```

pub mod bits;
pub mod canonical;
pub mod diagnostics;
pub mod frontend;
pub mod interpret;
pub mod ir;
pub mod plan;
pub mod validate;
