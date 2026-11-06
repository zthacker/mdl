//! Unsigned integers at bit positions in a binary payload.
//!
//! Bits are numbered from the most significant bit of the first byte, in
//! both byte orders: bit 0 is the top bit of byte 0, bit 8 the top bit of
//! byte 1. A big-endian value may start and end anywhere. A little-endian
//! value is either whole bytes on byte boundaries, least significant byte
//! first, or fits inside one byte, where byte order doesn't apply.
//! Validation guarantees every field and locator is one of these.

use crate::v2::ir::ByteOrder;

/// Why a value can't be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BitsError {
    /// The value ends past the end of the payload.
    PastEnd { end_bits: u64, payload_bits: u64 },
    /// A little-endian value that spans bytes without being whole bytes.
    Misaligned,
    /// A size outside 1 to 64 bits.
    BadSize,
}

/// The `size_bits`-bit unsigned value at `offset_bits`.
pub(crate) fn read_bits(
    payload: &[u8],
    offset_bits: u64,
    size_bits: u32,
    order: ByteOrder,
) -> Result<u64, BitsError> {
    if !(1..=64).contains(&size_bits) {
        return Err(BitsError::BadSize);
    }
    let size = u64::from(size_bits);
    // usize is at most 64 bits on every target Rust supports.
    let payload_bits = (payload.len() as u64).saturating_mul(8);
    let end_bits = offset_bits.saturating_add(size);
    let past_end = BitsError::PastEnd {
        end_bits,
        payload_bits,
    };
    if end_bits > payload_bits {
        return Err(past_end);
    }
    // Both are below payload.len(), so they fit a usize.
    let first = usize::try_from(offset_bits / 8).map_err(|_| past_end)?;
    let last = usize::try_from((end_bits - 1) / 8).map_err(|_| past_end)?;
    let bytes = &payload[first..=last];
    match order {
        ByteOrder::Little if first != last => {
            if !offset_bits.is_multiple_of(8) || !size.is_multiple_of(8) {
                return Err(BitsError::Misaligned);
            }
            Ok(bytes
                .iter()
                .rev()
                .fold(0, |value, &byte| (value << 8) | u64::from(byte)))
        }
        _ => Ok(most_significant_first(bytes, offset_bits % 8, size)),
    }
}

/// The low `size_bits` bits of `raw` as a two's-complement number:
/// `sign_extend(0b11, 2)` is -1.
pub(crate) fn sign_extend(raw: u64, size_bits: u32) -> i64 {
    let unused = 64 - size_bits.clamp(1, 64);
    // Move the value's top bit to bit 63, reinterpret the bits as signed,
    // then shift back: `>>` on an i64 copies the sign bit in.
    ((raw << unused) as i64) >> unused
}

/// `size` bits of `bytes`, starting `skip` bits into the first byte. A
/// 64-bit value that starts mid-byte spans 9 bytes, so a u128 holds them.
fn most_significant_first(bytes: &[u8], skip: u64, size: u64) -> u64 {
    let window = bytes
        .iter()
        .fold(0_u128, |window, &byte| (window << 8) | u128::from(byte));
    let unused_low = 8 * bytes.len() as u64 - skip - size;
    let mask = (1_u128 << size) - 1;
    // Masked to at most 64 bits, so the truncation keeps every bit.
    ((window >> unused_low) & mask) as u64
}

#[cfg(test)]
mod tests;
