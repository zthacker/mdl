//! Unsigned integers at bit positions in a binary payload, read and
//! written.
//!
//! Bits are numbered from the most significant bit of the first byte, in
//! both byte orders: bit 0 is the top bit of byte 0, bit 8 the top bit of
//! byte 1. A big-endian value may start and end anywhere. A little-endian
//! value is either whole bytes on byte boundaries, least significant byte
//! first, or fits inside one byte, where byte order doesn't apply.
//! Validation guarantees every field and locator is one of these.

use crate::ir::{ByteOrder, ScalarType, Signed};

/// Why a value can't be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BitsError {
    /// The value ends past the end of the payload.
    PastEnd { end_bits: u64, payload_bits: u64 },
    /// A little-endian value that spans bytes without being whole bytes.
    Misaligned,
    /// A size outside 1 to 64 bits.
    BadSize,
    /// A value to write that doesn't fit its size.
    TooWide,
}

/// The `size_bits`-bit unsigned value at `offset_bits`.
pub(crate) fn read_bits(
    payload: &[u8],
    offset_bits: u64,
    size_bits: u32,
    order: ByteOrder,
) -> Result<u64, BitsError> {
    let (first, last) = span(payload.len(), offset_bits, size_bits, order)?;
    let bytes = &payload[first..=last];
    match order {
        ByteOrder::Little if first != last => Ok(bytes
            .iter()
            .rev()
            .fold(0, |value, &byte| (value << 8) | u64::from(byte))),
        _ => Ok(most_significant_first(
            bytes,
            offset_bits % 8,
            u64::from(size_bits),
        )),
    }
}

/// Writes `value` as the `size_bits`-bit unsigned value at `offset_bits`,
/// so [`read_bits`] reads it back, leaving every other bit of `payload` as
/// it was. A value wider than `size_bits` is an error, never truncated.
pub(crate) fn write_bits(
    payload: &mut [u8],
    offset_bits: u64,
    size_bits: u32,
    value: u64,
    order: ByteOrder,
) -> Result<(), BitsError> {
    let (first, last) = span(payload.len(), offset_bits, size_bits, order)?;
    let size = u64::from(size_bits);
    if size < 64 && value >> size != 0 {
        return Err(BitsError::TooWide);
    }
    let bytes = &mut payload[first..=last];
    let count = bytes.len();
    match order {
        // Whole bytes, at most 8 of them.
        ByteOrder::Little if first != last => {
            bytes.copy_from_slice(&value.to_le_bytes()[..count]);
        }
        _ => {
            let window = bytes
                .iter()
                .fold(0_u128, |window, &byte| (window << 8) | u128::from(byte));
            let unused_low = 8 * count as u64 - offset_bits % 8 - size;
            let mask = ((1_u128 << size) - 1) << unused_low;
            let window = (window & !mask) | (u128::from(value) << unused_low);
            // At most 9 bytes, the low ones of the 16.
            bytes.copy_from_slice(&window.to_be_bytes()[16 - count..]);
        }
    }
    Ok(())
}

/// The first and last byte a value of `size_bits` at `offset_bits` takes,
/// once it's known to be inside a payload of `len` bytes and laid out as
/// `order` allows.
fn span(
    len: usize,
    offset_bits: u64,
    size_bits: u32,
    order: ByteOrder,
) -> Result<(usize, usize), BitsError> {
    if !(1..=64).contains(&size_bits) {
        return Err(BitsError::BadSize);
    }
    let size = u64::from(size_bits);
    // usize is at most 64 bits on every target Rust supports.
    let payload_bits = (len as u64).saturating_mul(8);
    let end_bits = offset_bits.saturating_add(size);
    let past_end = BitsError::PastEnd {
        end_bits,
        payload_bits,
    };
    if end_bits > payload_bits {
        return Err(past_end);
    }
    // Both are below len, so they fit a usize.
    let first = usize::try_from(offset_bits / 8).map_err(|_| past_end)?;
    let last = usize::try_from((end_bits - 1) / 8).map_err(|_| past_end)?;
    if order == ByteOrder::Little
        && first != last
        && (!offset_bits.is_multiple_of(8) || !size.is_multiple_of(8))
    {
        return Err(BitsError::Misaligned);
    }
    Ok((first, last))
}

/// How an integer's bits are its value (`decoding.md` §2.1): unsigned, or
/// one of the signed forms. Every reading of an integer, and every bound on
/// one, goes through here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum IntForm {
    Unsigned,
    TwosComplement,
    SignMagnitude,
    OnesComplement,
}

impl IntForm {
    /// The form of a `uint`, `int` or `bool` value; `None` for any other.
    pub(crate) fn of(ty: ScalarType, signed: Option<Signed>) -> Option<IntForm> {
        match (ty, signed) {
            (ScalarType::Uint | ScalarType::Bool, _) => Some(IntForm::Unsigned),
            (ScalarType::Int, None | Some(Signed::TwosComplement)) => Some(IntForm::TwosComplement),
            (ScalarType::Int, Some(Signed::SignMagnitude)) => Some(IntForm::SignMagnitude),
            (ScalarType::Int, Some(Signed::OnesComplement)) => Some(IntForm::OnesComplement),
            _ => None,
        }
    }

    /// The values `size_bits` bits hold: two's complement one more negative
    /// than positive, sign-magnitude and ones' complement as many of each,
    /// having two zeros.
    pub(crate) fn range(self, size_bits: u32) -> (i128, i128) {
        let size = size_bits.clamp(1, 64);
        let half = 1_i128 << (size - 1);
        match self {
            IntForm::Unsigned => (0, (1_i128 << size) - 1),
            IntForm::TwosComplement => (-half, half - 1),
            IntForm::SignMagnitude | IntForm::OnesComplement => (-(half - 1), half - 1),
        }
    }

    /// The value of the low `size_bits` bits of `raw`. A negative zero is
    /// 0.
    pub(crate) fn read(self, raw: u64, size_bits: u32) -> i128 {
        let size = size_bits.clamp(1, 64);
        let mask = u64::MAX >> (64 - size);
        let raw = raw & mask;
        let negative = raw >> (size - 1) == 1;
        match self {
            IntForm::Unsigned => i128::from(raw),
            // Move the value's top bit to bit 63, reinterpret the bits as
            // signed, then shift back: `>>` on an i64 copies the sign in.
            IntForm::TwosComplement => {
                let unused = 64 - size;
                i128::from(((raw << unused) as i64) >> unused)
            }
            IntForm::SignMagnitude if negative => -i128::from(raw & (mask >> 1)),
            IntForm::OnesComplement if negative => -i128::from(!raw & mask),
            IntForm::SignMagnitude | IntForm::OnesComplement => i128::from(raw),
        }
    }

    /// The `size_bits` bits that hold `value`, if they can: 0 as +0.
    pub(crate) fn write(self, value: i128, size_bits: u32) -> Option<u64> {
        let (low, high) = self.range(size_bits);
        if value < low || value > high {
            return None;
        }
        let size = size_bits.clamp(1, 64);
        let mask = u64::MAX >> (64 - size);
        let magnitude = u64::try_from(value.unsigned_abs()).ok()?;
        Some(match self {
            _ if value >= 0 => magnitude,
            IntForm::TwosComplement => magnitude.wrapping_neg() & mask,
            IntForm::SignMagnitude => (1 << (size - 1)) | magnitude,
            IntForm::OnesComplement => !magnitude & mask,
            // A negative value is outside an unsigned range.
            IntForm::Unsigned => return None,
        })
    }
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
