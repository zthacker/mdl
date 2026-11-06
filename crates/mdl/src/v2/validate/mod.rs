//! Validation: whether a bundle means something.
//!
//! The reader already guarantees a bundle has the IR's shape. Validation
//! checks everything else and reports every problem at once, each with a
//! stable [`Code`](crate::v2::diagnostics::Code). The only way to get a
//! [`Validated`] bundle is to pass.
//!
//! Checks: names and references, families and their declared metadata,
//! set versions, time sources, identification, nesting depth, numeric
//! ranges, layout (positions, sizes for each type, alignment, overlaps,
//! exact lengths, references to earlier entries) and canonical mapping
//! (types, units, enum states, composites and frames).
//!
//! Validation reports every problem but not their echoes: a mistake that
//! another check depends on (an unknown family, an undefined struct type,
//! an enum param without states) is reported once, and the checks that
//! depend on it stay quiet.
//!
//! # Identification
//!
//! Each identify key resolves to what it reads: a JSON Pointer, a metadata
//! name the family declares, or a top-level integer or bool field of the
//! packet (compared by static bit position; a field whose position isn't
//! static counts as distinct in every packet). Values compare as their key
//! reads them: pointers compare JSON values exactly (`"2"` is not `2`);
//! fields and metadata compare integers by value, and an integer a JSON
//! number holds is written as one. A message matches a packet when every
//! key matches, and is decoded with the most specific matching packet:
//! validation guarantees that any two packets one message can match have
//! strictly nested keys, so the matches form a chain with one largest
//! member. An empty identify is the set's catch-all.
//!
//! A decoder that follows this: tries packets in descending order of key
//! count and takes the first full match (packets with equally many keys
//! never match one message, so their order doesn't matter); resolves keys
//! exactly as validation does; and treats a key it can't evaluate (missing
//! metadata, a payload too short, an earlier entry that fails to decode) as
//! "no match", never as an error, so a more specific packet's failure falls
//! back to the less specific one.

mod catalog;
mod family;
mod layout;
mod mapping;
pub(crate) mod rules;
mod set;

#[cfg(test)]
mod tests;

use crate::v2::diagnostics::{Diagnostics, Place, Report};
use crate::v2::ir::Bundle;

/// How deep struct types and conditions may nest. A fixed limit every
/// implementation shares, so decoding depth is bounded by the definition
/// language itself, not by whatever stack a decoder happens to have.
pub const MAX_NESTING: usize = 32;

/// A bundle that passed validation.
#[derive(Debug, Clone, PartialEq)]
pub struct Validated {
    bundle: Bundle,
}

impl Validated {
    pub fn bundle(&self) -> &Bundle {
        &self.bundle
    }

    pub fn into_bundle(self) -> Bundle {
        self.bundle
    }
}

/// Check everything; a `Validated` bundle only if nothing is wrong.
pub fn validate(bundle: Bundle) -> Result<Validated, Diagnostics> {
    let mut report = Report::default();
    let root = Place::root("");
    catalog::check(&bundle.catalog, &root.section("catalog"), &mut report);
    let families = family::check_all(&bundle.families, &root, &mut report);
    set::check_all(&bundle, &families, &root, &mut report);
    report.finish(Validated { bundle })
}
