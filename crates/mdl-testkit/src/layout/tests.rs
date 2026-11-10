//! The packer against hand-worked bytes, so the oracle doesn't rest only
//! on agreeing with `mdl`. The whole-byte vectors are what Python's
//! `struct.pack` gives (`<H`, `>H`, `<Q`, `>f`, `<f`).

use super::{fit, put_bits, sign_extend};

fn packed(length: usize, offset: u64, size: u32, value: u64, little: bool) -> Vec<u8> {
    let mut payload = vec![0; length];
    put_bits(&mut payload, offset, size, value, little);
    payload
}

#[test]
fn whole_bytes_are_written_in_their_order() {
    // struct.pack('<H', 0x1234) and ('>H', 0x1234), from byte 1.
    assert_eq!(packed(3, 8, 16, 0x1234, true), [0x00, 0x34, 0x12]);
    assert_eq!(packed(3, 8, 16, 0x1234, false), [0x00, 0x12, 0x34]);
    // struct.pack('<Q', 0x0102030405060708).
    assert_eq!(
        packed(8, 0, 64, 0x0102_0304_0506_0708, true),
        [8, 7, 6, 5, 4, 3, 2, 1]
    );
    // struct.pack('>f', 1.5) and ('<f', 1.5).
    let bits = u64::from(1.5_f32.to_bits());
    assert_eq!(packed(4, 0, 32, bits, false), [0x3f, 0xc0, 0x00, 0x00]);
    assert_eq!(packed(4, 0, 32, bits, true), [0x00, 0x00, 0xc0, 0x3f]);
}

#[test]
fn bits_are_numbered_from_the_top_of_byte_0() {
    // 0xABC in 12 bits from bit 4: 0000 1010 | 1011 1100.
    assert_eq!(packed(2, 4, 12, 0xabc, false), [0x0a, 0xbc]);
    // Inside one byte a little-endian value is written as big-endian:
    // 0b101 at bit 2 is 0010 1000.
    assert_eq!(packed(1, 2, 3, 0b101, true), [0x28]);
    assert_eq!(packed(1, 2, 3, 0b101, false), [0x28]);
}

#[test]
fn a_signed_value_is_twos_complement_in_its_size() {
    // -3 in 5 bits is 11101, from bit 0: 1110 1000.
    let raw = fit(-3_i64 as u64, 5);
    assert_eq!(raw, 0b11101);
    assert_eq!(packed(1, 0, 5, raw, false), [0xe8]);
    assert_eq!(sign_extend(raw, 5), -3);
    assert_eq!(sign_extend(fit(i64::MIN as u64, 64), 64), i64::MIN);
}

#[test]
fn writing_leaves_every_other_bit_as_it_was() {
    let mut payload = vec![0xff; 2];
    put_bits(&mut payload, 4, 8, 0, false);
    assert_eq!(payload, [0xf0, 0x0f]);
}
