//! Positions of binary entries that are known without decoding anything.
//!
//! Part 2 of validation grows this into the full layout check; for now it
//! gives identification the static bit offset of top-level fields, so two
//! packets' keys can be compared by the bits they read rather than by name.

use crate::v2::ir::{ByteLength, Count, Element, Entry, ScalarType};

/// The bit offset of each entry, where it is known: an explicit
/// `offset_bits`, or a sequential entry that follows only entries of known
/// size.
pub(super) fn static_offsets(entries: &[Entry]) -> Vec<Option<u64>> {
    let mut cursor = Some(0_u64);
    entries
        .iter()
        .map(|entry| {
            let offset = explicit_offset(entry).or(cursor);
            // An offset that doesn't fit in 64 bits is unknown, not wrapped.
            cursor = offset
                .zip(fixed_size(entry))
                .and_then(|(o, size)| o.checked_add(size));
            offset
        })
        .collect()
}

fn explicit_offset(entry: &Entry) -> Option<u64> {
    match entry {
        Entry::Field(field) => field.offset_bits,
        Entry::Array(array) => array.offset_bits,
        Entry::Struct(entry) => entry.offset_bits,
        Entry::Padding(padding) => padding.offset_bits,
    }
    .map(u64::from)
}

/// Size in bits when it can't vary between messages. A conditional entry
/// may be absent, so its size varies.
fn fixed_size(entry: &Entry) -> Option<u64> {
    match entry {
        Entry::Field(field) if field.when.is_none() => {
            scalar_size(field.ty, field.size_bits, field.length.as_ref())
        }
        Entry::Padding(padding) if padding.when.is_none() => Some(u64::from(padding.size_bits)),
        Entry::Array(array) if array.when.is_none() => match (&array.count, &array.element) {
            (Some(Count::Fixed { count }), Element::Scalar(element)) => {
                scalar_size(element.ty, element.size_bits, element.length.as_ref())
                    .and_then(|size| size.checked_mul(u64::from(*count)))
            }
            _ => None,
        },
        _ => None,
    }
}

fn scalar_size(ty: ScalarType, size_bits: Option<u32>, length: Option<&ByteLength>) -> Option<u64> {
    match ty {
        ScalarType::String | ScalarType::Bytes => match length? {
            ByteLength::Fixed { bytes } => Some(u64::from(*bytes) * 8),
            _ => None,
        },
        _ => size_bits.map(u64::from),
    }
}
