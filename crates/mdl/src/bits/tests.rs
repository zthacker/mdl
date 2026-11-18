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

fn fit(value: u64, size: u32) -> u64 {
    if size == 64 {
        value
    } else {
        value & ((1 << size) - 1)
    }
}

proptest! {
    #[test]
    fn a_write_reads_back_and_leaves_every_other_bit_alone(
        payload in proptest::collection::vec(any::<u8>(), 1..12),
        offset in 0_u64..96,
        size in 1_u32..=64,
        value in any::<u64>(),
        little in any::<bool>(),
    ) {
        // Little-endian values span whole bytes or sit inside one, so a
        // little-endian value that would do neither is moved to whole
        // bytes (rather than rejected, which runs out of rejects).
        let inside_one_byte = offset / 8 == (offset + u64::from(size) - 1) / 8;
        let (offset, size) = if little && !inside_one_byte {
            (offset / 8 * 8, size.div_ceil(8) * 8)
        } else {
            (offset, size)
        };
        let value = fit(value, size);
        let order = if little { ByteOrder::Little } else { ByteOrder::Big };
        let mut written = payload.clone();
        let result = write_bits(&mut written, offset, size, value, order);
        let payload_bits = payload.len() as u64 * 8;
        if offset + u64::from(size) > payload_bits {
            let is_past_end = matches!(result, Err(BitsError::PastEnd { .. }));
            prop_assert!(is_past_end);
            prop_assert_eq!(written, payload);
            return Ok(());
        }
        prop_assert_eq!(result, Ok(()));
        prop_assert_eq!(read_bits(&written, offset, size, order), Ok(value));
        for bit in (0..payload_bits).filter(|bit| !(offset..offset + u64::from(size)).contains(bit)) {
            prop_assert_eq!(
                reference_big(&written, bit, 1),
                reference_big(&payload, bit, 1),
                "bit {}", bit
            );
        }
    }

    #[test]
    fn little_endian_whole_bytes_write_as_to_le_bytes(
        payload in proptest::collection::vec(any::<u8>(), 8..12),
        first in 0_usize..4,
        bytes in 1_usize..=8,
        value in any::<u64>(),
    ) {
        prop_assume!(first + bytes <= payload.len());
        let value = fit(value, bytes as u32 * 8);
        let mut written = payload.clone();
        write_bits(&mut written, first as u64 * 8, bytes as u32 * 8, value, ByteOrder::Little).unwrap();
        prop_assert_eq!(&written[first..first + bytes], &value.to_le_bytes()[..bytes]);
    }
}

#[test]
fn writes_refuse_what_reads_refuse_and_values_too_wide() {
    let mut payload = [0xAA_u8; 3];
    assert_eq!(
        write_bits(&mut payload, 0, 3, 0b1000, ByteOrder::Big),
        Err(BitsError::TooWide)
    );
    assert_eq!(
        write_bits(&mut payload, 0, 0, 0, ByteOrder::Big),
        Err(BitsError::BadSize)
    );
    assert_eq!(
        write_bits(&mut payload, 4, 8, 0, ByteOrder::Little),
        Err(BitsError::Misaligned)
    );
    assert_eq!(
        write_bits(&mut payload, 20, 8, 0, ByteOrder::Big),
        Err(BitsError::PastEnd {
            end_bits: 28,
            payload_bits: 24
        })
    );
    // Nothing is written when a write fails.
    assert_eq!(payload, [0xAA; 3]);
    // A 64-bit value starting mid-byte spans 9 bytes.
    let mut wide = [0_u8; 9];
    write_bits(&mut wide, 4, 64, u64::MAX, ByteOrder::Big).unwrap();
    assert_eq!(wide, [0x0F, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xF0]);
    // Inside one byte, byte order doesn't apply.
    let mut byte = [0_u8; 1];
    write_bits(&mut byte, 1, 3, 0b101, ByteOrder::Little).unwrap();
    assert_eq!(byte, [0b0101_0000]);
}

#[test]
fn integer_forms_read_and_write_their_bit_patterns() {
    use IntForm::*;
    // 16-bit -5 in each signed form, and 5, which they share.
    for (form, bits) in [
        (TwosComplement, 0xFFFB),
        (SignMagnitude, 0x8005),
        (OnesComplement, 0xFFFA),
    ] {
        assert_eq!(form.write(-5, 16), Some(bits), "{form:?}");
        assert_eq!(form.read(bits, 16), -5, "{form:?}");
        assert_eq!(form.write(5, 16), Some(0x0005), "{form:?}");
        assert_eq!(form.write(0, 16), Some(0), "{form:?}");
    }
    // A negative zero reads as 0.
    assert_eq!(SignMagnitude.read(0x8000, 16), 0);
    assert_eq!(OnesComplement.read(0xFFFF, 16), 0);
    // Two bits: -1, 0 and 1, and the second zero.
    assert_eq!(SignMagnitude.range(2), (-1, 1));
    assert_eq!(
        [0b00, 0b01, 0b10, 0b11].map(|raw| SignMagnitude.read(raw, 2)),
        [0, 1, 0, -1]
    );
    assert_eq!(
        [0b00, 0b01, 0b10, 0b11].map(|raw| OnesComplement.read(raw, 2)),
        [0, 1, -1, 0]
    );
    // The ends at 64 bits.
    let most = i128::from(i64::MAX);
    assert_eq!(TwosComplement.range(64), (-most - 1, most));
    assert_eq!(SignMagnitude.range(64), (-most, most));
    assert_eq!(SignMagnitude.write(-most, 64), Some(u64::MAX));
    assert_eq!(OnesComplement.write(-most, 64), Some(1 << 63));
    assert_eq!(TwosComplement.write(-most - 1, 64), Some(1 << 63));
    assert_eq!(SignMagnitude.write(-most - 1, 64), None);
    assert_eq!(Unsigned.range(64), (0, i128::from(u64::MAX)));
    assert_eq!(Unsigned.read(u64::MAX, 64), i128::from(u64::MAX));
    // Outside the range, nothing is written.
    assert_eq!(Unsigned.write(-1, 8), None);
    assert_eq!(Unsigned.write(256, 8), None);
    assert_eq!(TwosComplement.write(128, 8), None);
    assert_eq!(OnesComplement.write(-128, 8), None);
    // Only the low bits are read.
    assert_eq!(SignMagnitude.read(0xFF05, 8), 5);
}

fn int_forms() -> impl Strategy<Value = IntForm> {
    prop_oneof![
        Just(IntForm::Unsigned),
        Just(IntForm::TwosComplement),
        Just(IntForm::SignMagnitude),
        Just(IntForm::OnesComplement),
    ]
}

proptest! {
    /// Every value a form holds is written and read back.
    #[test]
    fn every_value_in_range_round_trips(form in int_forms(), size in 2_u32..=64, pick in any::<u128>()) {
        let (low, high) = form.range(size);
        let span = u128::try_from(high - low).unwrap() + 1;
        let value = low + i128::try_from(pick % span).unwrap();
        let bits = form.write(value, size).unwrap();
        prop_assert!(size == 64 || bits >> size == 0);
        prop_assert_eq!(form.read(bits, size), value);
    }

    /// Every bit pattern reads as a value written back the same, but a
    /// negative zero, which is written as +0.
    #[test]
    fn every_pattern_reads_back(form in int_forms(), size in 2_u32..=64, raw in any::<u64>()) {
        let mask = u64::MAX >> (64 - size);
        let raw = raw & mask;
        let negative_zero = match form {
            IntForm::SignMagnitude => raw == 1 << (size - 1),
            IntForm::OnesComplement => raw == mask,
            _ => false,
        };
        let back = form.write(form.read(raw, size), size).unwrap();
        prop_assert_eq!(back, if negative_zero { 0 } else { raw });
    }
}
