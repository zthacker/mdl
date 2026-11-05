//! Reading fields out of binary payloads.
//!
//! MDL's bit numbering is the same as CCSDS: bit 0 is the most significant
//! bit of byte 0, bit 7 its least significant, bit 8 the most significant
//! bit of byte 1, and so on.
//!
//! ```text
//!   byte 0                    byte 1
//!   bit 0 1 2 3 4 5 6 7       bit 8 9 10 11 12 13 14 15
//!       ^ MSB         LSB ^
//! ```
//!
//! `Endian` decides how the bytes of a multi-byte field combine. Big-endian
//! fields may start and end anywhere. Little-endian fields must be whole
//! bytes on byte boundaries (as MAVLink's are) or fit inside one byte.

use thiserror::Error;

use crate::ir::Endian;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BitsError {
    #[error("size_bits must be 1-64, got {size_bits}")]
    BadSize { size_bits: u32 },
    #[error("{size_bits} bits at bit {offset_bits} run past the {payload_bits}-bit payload")]
    OutOfRange {
        offset_bits: u32,
        size_bits: u32,
        payload_bits: usize,
    },
    #[error(
        "a little-endian field must be whole bytes on byte boundaries or fit in one byte, \
         got {size_bits} bits at bit {offset_bits}"
    )]
    Misaligned { offset_bits: u32, size_bits: u32 },
}

/// Read `size_bits` bits starting at `offset_bits`, as an unsigned number.
/// Signed and float fields start from this value (see `sign_extend`).
pub fn extract_bits(
    payload: &[u8],
    offset_bits: u32,
    size_bits: u32,
    endian: Endian,
) -> Result<u64, BitsError> {
    if !(1..=64).contains(&size_bits) {
        return Err(BitsError::BadSize { size_bits });
    }

    let offset = offset_bits as usize;
    let size = size_bits as usize;
    let end = offset + size; // two u32s can't overflow a 64-bit usize
    let payload_bits = payload.len() * 8;
    if end > payload_bits {
        return Err(BitsError::OutOfRange {
            offset_bits,
            size_bits,
            payload_bits,
        });
    }

    let first_byte = offset / 8;
    let last_byte = (end - 1) / 8;
    match endian {
        Endian::Big => Ok(read_msb_first(payload, offset, end)),
        // Inside one byte there's no byte order to apply
        Endian::Little if first_byte == last_byte => Ok(read_msb_first(payload, offset, end)),
        Endian::Little if offset.is_multiple_of(8) && size.is_multiple_of(8) => {
            Ok(read_little_endian(&payload[first_byte..=last_byte]))
        }
        Endian::Little => Err(BitsError::Misaligned {
            offset_bits,
            size_bits,
        }),
    }
}

/// Can `extract_bits` read a field at these bits in this byte order?
/// Big-endian fields can be anywhere; little-endian ones must be whole bytes
/// on byte boundaries or fit inside one byte. `validate` asks the same
/// question, so `morphc` rejects at build time what a decoder would reject
/// at runtime.
pub fn byte_order_fits(offset_bits: u32, size_bits: u32, endian: Endian) -> bool {
    let start = u64::from(offset_bits);
    let end = start + u64::from(size_bits.max(1));
    match endian {
        Endian::Big => true,
        Endian::Little => {
            start / 8 == (end - 1) / 8
                || (offset_bits.is_multiple_of(8) && size_bits.is_multiple_of(8))
        }
    }
}

/// Bits `start..end`, most significant first. The caller checked the bounds
fn read_msb_first(payload: &[u8], start: usize, end: usize) -> u64 {
    let mut value = 0u64;
    for bit in start..end {
        let byte = payload[bit / 8];
        let shift = 7 - (bit % 8);
        value = (value << 1) | u64::from((byte >> shift) & 1);
    }
    value
}

/// Whole bytes, least significant first
fn read_little_endian(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .rev()
        .fold(0u64, |value, &byte| (value << 8) | u64::from(byte))
}

/// Treat the low `size_bits` bits of `raw` as a two's-complement number:
/// `sign_extend(0b11, 2)` is -1, `sign_extend(0xFFFF, 16)` is -1.
pub fn sign_extend(raw: u64, size_bits: u32) -> i64 {
    let unused = 64 - size_bits.clamp(1, 64);
    // Move the field's top bit to bit 63, reinterpret as signed, then shift
    // back: `>>` on i64 copies the sign bit in from the left
    ((raw << unused) as i64) >> unused
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SAT-1's EPS_HK payload (definitions/sets/vendor_a.toml): spacecraft
    /// id 101 at bits 48-63, bus voltage 742 counts at 64-79, mode 1 in
    /// byte 14's low two bits (118-119).
    fn eps_hk() -> [u8; 15] {
        let mut payload = [0u8; 15];
        payload[6..8].copy_from_slice(&101u16.to_be_bytes());
        payload[8..10].copy_from_slice(&742u16.to_be_bytes());
        payload[14] = 0b1111_1101; // other bits set, to prove they're ignored
        payload
    }

    #[test]
    fn reads_big_endian_fields_from_a_real_layout() {
        let payload = eps_hk();
        assert_eq!(extract_bits(&payload, 48, 16, Endian::Big), Ok(101));
        assert_eq!(extract_bits(&payload, 64, 16, Endian::Big), Ok(742));
        assert_eq!(extract_bits(&payload, 118, 2, Endian::Big), Ok(1));
    }

    #[test]
    fn reads_little_endian_fields_from_a_real_layout() {
        // UAV-1's SYS_STATUS: voltage_battery, 12600 mV, at bytes 14-15.
        let mut payload = [0u8; 43];
        payload[14..16].copy_from_slice(&12600u16.to_le_bytes());
        assert_eq!(extract_bits(&payload, 112, 16, Endian::Little), Ok(12600));
        // Inside one byte, byte order doesn't matter.
        assert_eq!(
            extract_bits(&[0b0001_0100], 3, 3, Endian::Little),
            Ok(0b101)
        );
    }

    #[test]
    fn sign_extend_reads_twos_complement() {
        assert_eq!(sign_extend(0xFFFF, 16), -1);
        assert_eq!(sign_extend(0x7FFF, 16), 32767);
        assert_eq!(sign_extend(0b10, 2), -2);
        assert_eq!(sign_extend(u64::MAX, 64), -1);
    }

    #[test]
    fn bad_requests_are_errors_not_panics() {
        let payload = [0u8; 4];
        assert_eq!(
            extract_bits(&payload, 0, 0, Endian::Big),
            Err(BitsError::BadSize { size_bits: 0 })
        );
        assert_eq!(
            extract_bits(&payload, 0, 65, Endian::Big),
            Err(BitsError::BadSize { size_bits: 65 })
        );
        assert_eq!(
            extract_bits(&payload, 24, 16, Endian::Big),
            Err(BitsError::OutOfRange {
                offset_bits: 24,
                size_bits: 16,
                payload_bits: 32
            })
        );
        assert_eq!(
            extract_bits(&payload, u32::MAX, 64, Endian::Big),
            Err(BitsError::OutOfRange {
                offset_bits: u32::MAX,
                size_bits: 64,
                payload_bits: 32
            })
        );
        assert_eq!(
            extract_bits(&payload, 4, 16, Endian::Little),
            Err(BitsError::Misaligned {
                offset_bits: 4,
                size_bits: 16
            })
        );
    }

    mod properties {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            /// Any big-endian field of a 16-byte payload, checked against
            /// std: the payload as one u128, shifted and masked.
            #[test]
            fn big_endian_matches_a_u128_shift(
                payload in any::<[u8; 16]>(),
                offset in 0u32..128,
                size in 1u32..=64,
            ) {
                prop_assume!(offset + size <= 128);
                let whole = u128::from_be_bytes(payload);
                let expected = (whole >> (128 - offset - size)) & ((1u128 << size) - 1);
                prop_assert_eq!(
                    extract_bits(&payload, offset, size, Endian::Big),
                    Ok(expected as u64)
                );
            }

            /// Any whole-byte little-endian field, checked against
            /// `u64::from_le_bytes`.
            #[test]
            fn little_endian_matches_from_le_bytes(
                payload in any::<[u8; 16]>(),
                start in 0usize..16,
                len in 1usize..=8,
            ) {
                prop_assume!(start + len <= 16);
                let mut padded = [0u8; 8];
                padded[..len].copy_from_slice(&payload[start..start + len]);
                prop_assert_eq!(
                    extract_bits(&payload, (start * 8) as u32, (len * 8) as u32, Endian::Little),
                    Ok(u64::from_le_bytes(padded))
                );
            }

            /// `validate` and `extract_bits` agree on which little-endian
            /// layouts are allowed.
            #[test]
            fn byte_order_fits_agrees_with_extract_bits(
                offset in 0u32..192,
                size in 1u32..=64,
            ) {
                let payload = [0u8; 32];
                let misaligned = matches!(
                    extract_bits(&payload, offset, size, Endian::Little),
                    Err(BitsError::Misaligned { .. })
                );
                prop_assert_eq!(misaligned, !byte_order_fits(offset, size, Endian::Little));
                prop_assert!(byte_order_fits(offset, size, Endian::Big));
            }

            /// Narrow signed integers survive the round trip through raw bits.
            #[test]
            fn sign_extend_undoes_truncation(value in any::<i32>()) {
                prop_assert_eq!(sign_extend(value as u32 as u64, 32), i64::from(value));
                prop_assert_eq!(sign_extend(value as u16 as u64, 16), i64::from(value as i16));
                prop_assert_eq!(sign_extend(value as u8 as u64, 8), i64::from(value as i8));
            }
        }
    }
}
