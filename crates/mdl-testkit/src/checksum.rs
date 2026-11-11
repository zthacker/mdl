//! Checksums as `spec/decoding.md` §5.6 defines them, computed the slow way
//! its text describes (a CRC register shifted bit by bit, a Fletcher sum
//! byte by byte), for the command oracle. Pinned to the CRC catalogue's
//! check values and the ECSS test vectors, not to `mdl`.

use serde_json::{Value, json};

use crate::layout::integer_json;

/// A checksum algorithm with its parameters, as an artifact holds them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Algorithm {
    Crc {
        width: u32,
        poly: u64,
        init: u64,
        reflect_in: bool,
        reflect_out: bool,
        xor_out: u64,
    },
    Sum {
        width: u32,
    },
    Xor,
    Iso8473,
}

impl Algorithm {
    /// Bits its value takes in a packet.
    pub(crate) fn bits(&self) -> u32 {
        match self {
            Algorithm::Crc { width, .. } | Algorithm::Sum { width } => *width,
            Algorithm::Xor => 8,
            Algorithm::Iso8473 => 16,
        }
    }

    /// The value it computes over `bytes`. For `iso8473`, the first check
    /// byte is the high byte.
    pub(crate) fn compute(&self, bytes: &[u8]) -> u64 {
        match self {
            Algorithm::Crc {
                width,
                poly,
                init,
                reflect_in,
                reflect_out,
                xor_out,
            } => {
                let mask = (1_u128 << width) - 1;
                let mut register = u128::from(*init);
                for &byte in bytes {
                    let byte = if *reflect_in {
                        byte.reverse_bits()
                    } else {
                        byte
                    };
                    register ^= u128::from(byte) << (width - 8);
                    for _ in 0..8 {
                        let out = (register >> (width - 1)) & 1 == 1;
                        register = (register << 1) & mask;
                        if out {
                            register ^= u128::from(*poly);
                        }
                    }
                }
                let mut value = register as u64;
                if *reflect_out {
                    value = value.reverse_bits() >> (64 - width);
                }
                value ^ xor_out
            }
            Algorithm::Sum { width } => {
                let total: u128 = bytes.iter().map(|&byte| u128::from(byte)).sum();
                (total & ((1_u128 << width) - 1)) as u64
            }
            Algorithm::Xor => u64::from(bytes.iter().fold(0, |acc, byte| acc ^ byte)),
            Algorithm::Iso8473 => {
                let (mut c0, mut c1) = (0_u32, 0_u32);
                for &byte in bytes {
                    c0 = (c0 + u32::from(byte)) % 255;
                    c1 = (c1 + c0) % 255;
                }
                let nonzero = |byte: u32| if byte == 0 { 255 } else { byte };
                let first = nonzero((255 - (c0 + c1) % 255) % 255);
                let second = nonzero(c1);
                u64::from(first << 8 | second)
            }
        }
    }

    /// Its value as the packet stores it: a CRC or sum in the family's byte
    /// order, the ISO checksum's first byte then its second.
    pub(crate) fn stored(&self, value: u64, little: bool) -> Vec<u8> {
        let bytes = (self.bits() / 8) as usize;
        let big: Vec<u8> = (0..bytes).rev().map(|k| (value >> (8 * k)) as u8).collect();
        match self {
            Algorithm::Crc { .. } | Algorithm::Sum { .. } if little => {
                big.into_iter().rev().collect()
            }
            _ => big,
        }
    }

    /// As IR JSON.
    pub(crate) fn json(&self) -> Value {
        match self {
            Algorithm::Crc {
                width,
                poly,
                init,
                reflect_in,
                reflect_out,
                xor_out,
            } => json!({
                "kind": "crc",
                "width": width,
                "poly": integer_json(i128::from(*poly)),
                "init": integer_json(i128::from(*init)),
                "reflect_in": reflect_in,
                "reflect_out": reflect_out,
                "xor_out": integer_json(i128::from(*xor_out)),
            }),
            Algorithm::Sum { width } => json!({ "kind": "sum", "width": width }),
            Algorithm::Xor => json!({ "kind": "xor" }),
            Algorithm::Iso8473 => json!({ "kind": "iso8473" }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Algorithm;

    fn crc(width: u32, poly: u64, init: u64, reflect: bool, xor_out: u64) -> Algorithm {
        Algorithm::Crc {
            width,
            poly,
            init,
            reflect_in: reflect,
            reflect_out: reflect,
            xor_out,
        }
    }

    /// The check values of the CRC catalogue (Greg Cook's "Catalogue of
    /// parametrised CRC algorithms"): each model over the bytes
    /// `123456789`.
    #[test]
    fn crcs_give_the_catalogue_check_values() {
        let check = b"123456789";
        let models = [
            ("CRC-8/SMBUS", crc(8, 0x07, 0, false, 0), 0xF4),
            ("CRC-8/MAXIM-DOW", crc(8, 0x31, 0, true, 0), 0xA1),
            ("CRC-16/IBM-3740", crc(16, 0x1021, 0xFFFF, false, 0), 0x29B1),
            (
                "CRC-16/IBM-SDLC",
                crc(16, 0x1021, 0xFFFF, true, 0xFFFF),
                0x906E,
            ),
            ("CRC-16/ARC", crc(16, 0x8005, 0, true, 0), 0xBB3D),
            ("CRC-16/KERMIT", crc(16, 0x1021, 0, true, 0), 0x2189),
            // Reflected with an init that reads differently reflected,
            // which pins the register's init as given, not reflected.
            ("CRC-16/RIELLO", crc(16, 0x1021, 0xB2AA, true, 0), 0x63D0),
            (
                "CRC-24/BLE",
                crc(24, 0x00_065B, 0x55_5555, true, 0),
                0xC2_5A56,
            ),
            (
                "CRC-24/OPENPGP",
                crc(24, 0x86_4CFB, 0xB7_04CE, false, 0),
                0x21_CF02,
            ),
            (
                "CRC-32/ISO-HDLC",
                crc(32, 0x04C1_1DB7, 0xFFFF_FFFF, true, 0xFFFF_FFFF),
                0xCBF4_3926,
            ),
            (
                "CRC-32/ISCSI",
                crc(32, 0x1EDC_6F41, 0xFFFF_FFFF, true, 0xFFFF_FFFF),
                0xE306_9283,
            ),
            (
                "CRC-32/BZIP2",
                crc(32, 0x04C1_1DB7, 0xFFFF_FFFF, false, 0xFFFF_FFFF),
                0xFC89_1918,
            ),
            (
                "CRC-40/GSM",
                crc(40, 0x00_0482_0009, 0, false, 0xFF_FFFF_FFFF),
                0xD4_164F_C646,
            ),
            (
                "CRC-64/ECMA-182",
                crc(64, 0x42F0_E1EB_A9EA_3693, 0, false, 0),
                0x6C40_DF5F_0B49_7347,
            ),
            (
                "CRC-64/XZ",
                crc(64, 0x42F0_E1EB_A9EA_3693, u64::MAX, true, u64::MAX),
                0x995D_C9BB_DF19_39FA,
            ),
        ];
        for (name, model, value) in models {
            assert_eq!(model.compute(check), value, "{name}");
        }
    }

    #[test]
    fn sums_and_xors_take_every_byte() {
        let check = b"123456789";
        assert_eq!(Algorithm::Sum { width: 8 }.compute(check), 0xDD);
        assert_eq!(Algorithm::Sum { width: 16 }.compute(check), 0x01DD);
        assert_eq!(Algorithm::Sum { width: 64 }.compute(&[0xFF; 3]), 0x02FD);
        assert_eq!(Algorithm::Xor.compute(check), 0x31);
    }

    /// The ECSS-E-70-41A Annex A.2 vectors, and the zero case §5.6 names.
    #[test]
    fn the_iso_checksum_gives_the_ecss_test_vectors() {
        let iso = Algorithm::Iso8473;
        assert_eq!(iso.compute(&[0x00, 0x00]), 0xFFFF);
        assert_eq!(iso.compute(&[0x00, 0x00, 0x00]), 0xFFFF);
        assert_eq!(iso.compute(&[0xAB, 0xCD, 0xEF, 0x01]), 0x9CF8);
        assert_eq!(iso.compute(&[0x14, 0x56, 0xF8, 0x9A, 0x00, 0x01]), 0x24DC);
    }

    #[test]
    fn checksums_are_stored_in_their_byte_order() {
        let crc16 = crc(16, 0x1021, 0xFFFF, false, 0);
        assert_eq!(crc16.stored(0x29B1, false), [0x29, 0xB1]);
        assert_eq!(crc16.stored(0x29B1, true), [0xB1, 0x29]);
        assert_eq!(
            Algorithm::Sum { width: 24 }.stored(0x0A0B0C, true),
            [0x0C, 0x0B, 0x0A]
        );
        assert_eq!(Algorithm::Iso8473.stored(0x9CF8, true), [0x9C, 0xF8]);
        assert_eq!(Algorithm::Xor.stored(0x31, true), [0x31]);
    }
}
