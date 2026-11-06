use proptest::prelude::*;

use super::*;

const CHECK_INPUT: &[u8] = b"123456789";

#[test]
fn every_preset_gives_its_catalogue_check_value() {
    for preset in PRESETS {
        let algorithm = Algorithm::resolve(&preset.checksum()).unwrap();
        assert_eq!(
            algorithm.compute(CHECK_INPUT),
            preset.check,
            "{}",
            preset.name
        );
    }
}

#[test]
fn presets_are_found_by_name_or_alias_in_any_case() {
    assert_eq!(preset("CRC-32").map(|p| p.name), Some("CRC-32/ISO-HDLC"));
    assert_eq!(
        preset("crc-16/ccitt-false").map(|p| p.name),
        Some("CRC-16/IBM-3740")
    );
    assert_eq!(preset("CRC-17/NOPE"), None);
    // A CRC-64 polynomial is past 2^53, so the IR writes it as a string.
    let Checksum::Crc { poly, .. } = preset("CRC-64/XZ").unwrap().checksum() else {
        panic!("a CRC preset");
    };
    assert_eq!(poly, Scalar::Str("4823603603198064275".to_string()));
}

#[test]
fn sums_and_xors() {
    let sum8 = Algorithm::resolve(&Checksum::sum(8)).unwrap();
    assert_eq!(sum8.compute(&[0xFF, 0x02]), 0x01);
    let sum16 = Algorithm::resolve(&Checksum::sum(16)).unwrap();
    assert_eq!(sum16.compute(&[0xFF, 0x02]), 0x0101);
    let xor = Algorithm::resolve(&Checksum::xor()).unwrap();
    assert_eq!(xor.compute(&[0x0F, 0xF0, 0x01]), 0xFE);
    assert_eq!(xor.compute(&[]), 0);
}

#[test]
fn parameters_must_fit_their_width() {
    assert_eq!(
        Algorithm::resolve(&Checksum::crc(16, 0x1_0000, 0, false, false, 0)),
        None
    );
    assert_eq!(
        Algorithm::resolve(&Checksum::crc(12, 0x80F, 0, false, false, 0)),
        None
    );
    assert_eq!(Algorithm::resolve(&Checksum::sum(4)), None);
}

/// The CRC of `bytes` the long way: the message, with `width` zero bits
/// appended, divided by the polynomial over GF(2), for a CRC with no
/// reflection, zero init and zero xor.
fn polynomial_remainder(bytes: &[u8], width: u32, poly: u64) -> u64 {
    let mut bits: Vec<u8> = bytes
        .iter()
        .flat_map(|byte| (0..8).rev().map(move |i| (byte >> i) & 1))
        .collect();
    bits.extend(std::iter::repeat_n(0, width as usize));
    let mut divisor = vec![1_u8];
    divisor.extend((0..width).rev().map(|i| ((poly >> i) & 1) as u8));
    for start in 0..bits.len() - width as usize {
        if bits[start] == 1 {
            for (offset, bit) in divisor.iter().enumerate() {
                bits[start + offset] ^= bit;
            }
        }
    }
    bits[bits.len() - width as usize..]
        .iter()
        .fold(0, |value, bit| (value << 1) | u64::from(*bit))
}

proptest! {
    #[test]
    fn the_register_matches_polynomial_division(
        bytes in proptest::collection::vec(any::<u8>(), 0..24),
        width in prop_oneof![Just(8_u8), Just(16), Just(24), Just(32), Just(64)],
        poly in any::<u64>(),
    ) {
        let poly = poly & mask(u32::from(width));
        let crc = Algorithm::resolve(&Checksum::crc(width, scalar(poly), 0, false, false, 0)).unwrap();
        prop_assert_eq!(
            crc.compute(&bytes),
            polynomial_remainder(&bytes, u32::from(width), poly)
        );
    }
}

proptest! {
    #[test]
    fn the_table_gives_what_the_bits_give(
        bytes in proptest::collection::vec(any::<u8>(), 0..64),
        width in (1_u8..=8).prop_map(|bytes| bytes * 8),
        poly in any::<u64>(),
        init in any::<u64>(),
        xor_out in any::<u64>(),
        reflect_in in any::<bool>(),
        reflect_out in any::<bool>(),
    ) {
        let fit = |value: u64| scalar(value & mask(u32::from(width)));
        let checksum = Checksum::crc(width, fit(poly), fit(init), reflect_in, reflect_out, fit(xor_out));
        let algorithm = Algorithm::resolve(&checksum).unwrap();
        prop_assert_eq!(Compiled::new(algorithm).compute(&bytes), algorithm.compute(&bytes));
    }
}

#[test]
fn compiled_presets_give_their_check_values() {
    for preset in PRESETS {
        let compiled = Compiled::new(Algorithm::resolve(&preset.checksum()).unwrap());
        assert_eq!(
            compiled.compute(CHECK_INPUT),
            preset.check,
            "{}",
            preset.name
        );
    }
}

/// The ISO checksum's encoding procedure, a byte at a time, as
/// ECSS-E-70-41A Annex A.2 gives it.
fn iso8473_by_the_book(bytes: &[u8]) -> u64 {
    let (mut c0, mut c1) = (0_u64, 0_u64);
    for &byte in bytes {
        c0 = (c0 + u64::from(byte)) % 255;
        c1 = (c1 + c0) % 255;
    }
    // c0 + c1 is at most 508, so this is -(c0 + c1) modulo 255.
    let first = match (2 * 255 - c0 - c1) % 255 {
        0 => 255,
        n => n,
    };
    let second = if c1 == 0 { 255 } else { c1 };
    (first << 8) | second
}

/// The ISO checksum's decoding procedure, as Annex A.2 gives it: an error
/// if exactly one check byte is 0; otherwise the sums over the bytes and
/// both check bytes must come to 0.
fn iso8473_decoder_accepts(bytes: &[u8], stored: u16) -> bool {
    let [first, second] = stored.to_be_bytes();
    if (first == 0) != (second == 0) {
        return false;
    }
    let (mut c0, mut c1) = (0_u64, 0_u64);
    for &byte in bytes.iter().chain(&[first, second]) {
        c0 = (c0 + u64::from(byte)) % 255;
        c1 = (c1 + c0) % 255;
    }
    c0 == 0 && c1 == 0
}

fn iso8473() -> Algorithm {
    Algorithm::resolve(&Checksum::iso8473()).unwrap()
}

#[test]
fn the_iso_checksum_gives_the_ecss_test_vectors() {
    // ECSS-E-70-41A, A.2.5.
    let vectors: [(&[u8], u64); 4] = [
        (&[0x00, 0x00], 0xFFFF),
        (&[0x00, 0x00, 0x00], 0xFFFF),
        (&[0xAB, 0xCD, 0xEF, 0x01], 0x9CF8),
        (&[0x14, 0x56, 0xF8, 0x9A, 0x00, 0x01], 0x24DC),
    ];
    let compiled = Compiled::new(iso8473());
    for (bytes, check) in vectors {
        assert_eq!(iso8473().compute(bytes), check, "{bytes:02X?}");
        assert_eq!(compiled.compute(bytes), check, "{bytes:02X?}");
    }
}

#[test]
fn the_iso_checksum_accepts_what_the_ecss_decoder_accepts() {
    let algorithm = iso8473();
    // Both sums come to 0 for the empty and all-zero inputs, and for the
    // last; the decoder then takes check bytes of 0 as well as 255.
    let inputs: [&[u8]; 7] = [
        &[],
        &[0x00, 0x00],
        &[0xFF],
        &[0xFF, 0xFF],
        &[0xAB, 0xCD, 0xEF, 0x01],
        &[0x14, 0x56, 0xF8, 0x9A, 0x00, 0x01],
        &[0x14, 0xD7, 0x14],
    ];
    for bytes in inputs {
        let computed = algorithm.compute(bytes);
        for stored in 0..=u16::MAX {
            assert_eq!(
                algorithm.matches(u64::from(stored), computed),
                iso8473_decoder_accepts(bytes, stored),
                "{bytes:02X?} stored {stored:04X}"
            );
        }
    }
}

proptest! {
    #[test]
    fn the_iso_checksum_is_the_procedure_of_the_standard(
        // Past 4096 bytes, where the sums are first reduced.
        bytes in proptest::collection::vec(any::<u8>(), 0..9000),
        stored in any::<u16>(),
    ) {
        let algorithm = iso8473();
        let computed = algorithm.compute(&bytes);
        prop_assert_eq!(computed, iso8473_by_the_book(&bytes));
        prop_assert!(iso8473_decoder_accepts(&bytes, computed as u16));
        prop_assert_eq!(
            algorithm.matches(u64::from(stored), computed),
            iso8473_decoder_accepts(&bytes, stored)
        );
    }
}

#[test]
fn the_iso_checksum_sums_long_inputs_without_overflow() {
    // All ones grow both sums fastest.
    let bytes = vec![0xFF; 100_000];
    assert_eq!(iso8473().compute(&bytes), iso8473_by_the_book(&bytes));
}

#[test]
fn the_iso_checksum_keeps_its_own_byte_order() {
    assert_eq!(iso8473().order(ByteOrder::Little), ByteOrder::Big);
    assert_eq!(
        Algorithm::resolve(&Checksum::sum(16))
            .unwrap()
            .order(ByteOrder::Little),
        ByteOrder::Little
    );
}

#[test]
fn other_checksums_accept_only_what_they_compute() {
    let xor = Algorithm::resolve(&Checksum::xor()).unwrap();
    assert!(xor.matches(0, 0));
    assert!(!xor.matches(0xFF, 0));
    let sum = Algorithm::resolve(&Checksum::sum(16)).unwrap();
    assert!(!sum.matches(0, 0xFFFF));
}
