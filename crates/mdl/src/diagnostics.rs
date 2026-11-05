//! Diagnostics: problems found in definitions, collected rather than
//! returned one at a time, so one `morphc build` lists everything wrong.
//! Shared by every frontend (format errors) and by validation (meaning
//! errors).

use std::fmt;

/// One problem: where it came from, where in it, and what's wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// A file name (frontend errors) or a set name (validation errors).
    pub source: String,
    /// "packet EPS_HK, field mode", or empty for the source as a whole.
    pub location: String,
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.location.is_empty() {
            write!(f, "{}: {}", self.source, self.message)
        } else {
            write!(f, "{}: {}: {}", self.source, self.location, self.message)
        }
    }
}

/// Everything wrong with a set of definitions, one diagnostic per line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Errors(pub Vec<Diagnostic>);

impl fmt::Display for Errors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for diagnostic in &self.0 {
            writeln!(f, "{diagnostic}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Errors {}

/// Collects diagnostics while definitions are walked. `source` is the file
/// or set being looked at right now, so call sites only say where in it.
// Only v1 builds a Report until v2 validation (Phase 1, step 1b) does.
#[cfg_attr(not(feature = "v1"), allow(dead_code))]
#[derive(Debug, Default)]
pub(crate) struct Report {
    pub(crate) source: String,
    diagnostics: Vec<Diagnostic>,
}

// Only v1 builds a Report until v2 validation (Phase 1, step 1b) does.
#[cfg_attr(not(feature = "v1"), allow(dead_code))]
impl Report {
    pub(crate) fn new(source: impl Into<String>) -> Self {
        Report {
            source: source.into(),
            diagnostics: Vec::new(),
        }
    }

    pub(crate) fn error(&mut self, location: impl Into<String>, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic {
            source: self.source.clone(),
            location: location.into(),
            message: message.into(),
        });
    }

    pub(crate) fn count(&self) -> usize {
        self.diagnostics.len()
    }

    /// The value if nothing went wrong, otherwise every diagnostic.
    pub(crate) fn finish<T>(self, value: T) -> Result<T, Errors> {
        if self.diagnostics.is_empty() {
            Ok(value)
        } else {
            Err(Errors(self.diagnostics))
        }
    }
}
