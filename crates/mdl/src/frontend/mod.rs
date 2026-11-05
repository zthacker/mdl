//! Frontends: one per definition syntax, each translating its format into the
//! MDL IR (`crate::ir`). A frontend reports only *format* errors (bad TOML, a
//! field with neither bits nor a path). What the definitions mean is checked
//! by `crate::validate`, the same way for every frontend.
//!
//! TOML is the first syntax. XTCE, C headers or a database would each be
//! another module here.

pub mod toml;
