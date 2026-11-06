use proptest::prelude::*;

use super::*;

/// The obvious definition, one bit at a time.
fn reference_big(payload: &[u8], offset: u64, size: u32) -> u64 {
    let mut value = 0;
    for bit in offset..offset + u64::from(size) {
        let byte = payload[(bit / 8) as usize];
        value = (value << 1) | u64::from((byte >> (7 - bit % 8)) & 1);
    }
    value
}

#[test]
fn bit_numbering_starts_at_the_top_of_byte_zero() {
    let payload = [0b1010_0000, 0x12, 0x34];
    assert_eq!(read_bits(&payload, 0, 1, ByteOrder::Big), Ok(1));
    assert_eq!(read_bits(&payload, 1, 1, ByteOrder::Big), Ok(0));
    assert_eq!(read_bits(&payload, 0, 3, ByteOrder::Big), Ok(0b101));
    assert_eq!(read_bits(&payload, 8, 16, ByteOrder::Big), Ok(0x1234));
    assert_eq!(read_bits(&payload, 8, 16, ByteOrder::Little), Ok(0x3412));
    // Inside one byte there's no byte order to apply.
    assert_eq!(read_bits(&payload, 0, 3, ByteOrder::Little), Ok(0b101));
    assert_eq!(read_bits(&payload, 12, 8, ByteOrder::Big), Ok(0x23));
}

#[test]
fn reads_stay_inside_the_payload() {
    let payload = [0xFF; 9];
    assert_eq!(read_bits(&payload, 8, 64, ByteOrder::Big), Ok(u64::MAX));
    assert_eq!(read_bits(&payload, 7, 64, ByteOrder::Big), Ok(u64::MAX));
    assert_eq!(
        read_bits(&payload, 9, 64, ByteOrder::Big),
        Err(BitsError::PastEnd {
            end_bits: 73,
            payload_bits: 72
        })
    );
    assert!(matches!(
        read_bits(&payload, u64::MAX, 8, ByteOrder::Big),
        Err(BitsError::PastEnd { .. })
    ));
    assert!(matches!(
        read_bits(&[], 0, 1, ByteOrder::Big),
        Err(BitsError::PastEnd { .. })
    ));
    assert_eq!(
        read_bits(&payload, 0, 0, ByteOrder::Big),
        Err(BitsError::BadSize)
    );
    assert_eq!(
        read_bits(&payload, 0, 65, ByteOrder::Big),
        Err(BitsError::BadSize)
    );
    assert_eq!(
        read_bits(&payload, 4, 8, ByteOrder::Little),
        Err(BitsError::Misaligned)
    );
}

proptest! {
    #[test]
    fn big_endian_matches_the_reference(
        payload in proptest::collection::vec(any::<u8>(), 1..12),
        offset in 0_u64..96,
        size in 1_u32..=64,
    ) {
        let fits = offset + u64::from(size) <= payload.len() as u64 * 8;
        let read = read_bits(&payload, offset, size, ByteOrder::Big);
        if fits {
            prop_assert_eq!(read, Ok(reference_big(&payload, offset, size)));
        } else {
            let is_past_end = matches!(read, Err(BitsError::PastEnd { .. }));
            prop_assert!(is_past_end);
        }
    }

    #[test]
    fn little_endian_whole_bytes_match_from_le_bytes(
        payload in proptest::collection::vec(any::<u8>(), 8..12),
        first in 0_usize..4,
        bytes in 1_usize..=8,
    ) {
        prop_assume!(first + bytes <= payload.len());
        let mut padded = [0_u8; 8];
        padded[..bytes].copy_from_slice(&payload[first..first + bytes]);
        let read = read_bits(&payload, first as u64 * 8, bytes as u32 * 8, ByteOrder::Little);
        prop_assert_eq!(read, Ok(u64::from_le_bytes(padded)));
    }
}
