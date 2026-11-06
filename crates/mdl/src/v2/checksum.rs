//! Checksums: computing them, and the named CRCs of the CRC catalogue.

use crate::canonical::MAX_SAFE_INTEGER;
use crate::v2::ir::{ByteOrder, Checksum, Scalar};
use crate::v2::validate::rules::integer_value;

/// A checksum's parameters, resolved to integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Algorithm {
    Crc(Crc),
    Sum { width: u32 },
    Xor,
    Iso8473,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Crc {
    width: u32,
    poly: u64,
    init: u64,
    reflect_in: bool,
    reflect_out: bool,
    xor_out: u64,
}

/// The largest value `width` bits hold.
pub(crate) fn mask(width: u32) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1 << width) - 1
    }
}

/// Widths a CRC or sum may have: whole bytes, 8 to 64 bits.
pub(crate) fn is_width(width: u8) -> bool {
    (8..=64).contains(&width) && width.is_multiple_of(8)
}

impl Algorithm {
    /// `None` if the width isn't whole bytes from 8 to 64, or a parameter
    /// doesn't fit it (validation reports both).
    pub(crate) fn resolve(checksum: &Checksum) -> Option<Algorithm> {
        match checksum {
            Checksum::Crc {
                width,
                poly,
                init,
                reflect_in,
                reflect_out,
                xor_out,
            } => {
                if !is_width(*width) {
                    return None;
                }
                let width = u32::from(*width);
                let value = |scalar: &Scalar| {
                    integer_value(scalar)
                        .and_then(|n| u64::try_from(n).ok())
                        .filter(|n| *n <= mask(width))
                };
                Some(Algorithm::Crc(Crc {
                    width,
                    poly: value(poly)?,
                    init: value(init)?,
                    reflect_in: *reflect_in,
                    reflect_out: *reflect_out,
                    xor_out: value(xor_out)?,
                }))
            }
            Checksum::Sum { width } => is_width(*width).then(|| Algorithm::Sum {
                width: u32::from(*width),
            }),
            Checksum::Xor {} => Some(Algorithm::Xor),
            Checksum::Iso8473 {} => Some(Algorithm::Iso8473),
        }
    }

    /// The checksum of `bytes`.
    pub(crate) fn compute(&self, bytes: &[u8]) -> u64 {
        match self {
            Algorithm::Crc(crc) => crc.compute(bytes),
            Algorithm::Sum { width } => {
                bytes
                    .iter()
                    .fold(0_u64, |sum, byte| sum.wrapping_add(u64::from(*byte)))
                    & mask(*width)
            }
            Algorithm::Xor => u64::from(bytes.iter().fold(0_u8, |acc, byte| acc ^ byte)),
            Algorithm::Iso8473 => iso8473(bytes),
        }
    }

    /// Whether a stored checksum is right for bytes whose checksum is
    /// `computed`. Only the ISO checksum accepts a value it doesn't
    /// compute: its decoding procedure also takes check bytes of 0 where
    /// both sums are 0, which encoding writes as 255.
    pub(crate) fn matches(&self, stored: u64, computed: u64) -> bool {
        stored == computed || (*self == Algorithm::Iso8473 && computed == 0xFFFF && stored == 0)
    }

    /// The byte order a stored checksum is read and written in, for a
    /// family's byte order.
    pub(crate) fn order(&self, family: ByteOrder) -> ByteOrder {
        match self {
            Algorithm::Iso8473 => ByteOrder::Big,
            Algorithm::Crc(_) | Algorithm::Sum { .. } | Algorithm::Xor => family,
        }
    }
}

/// The ISO checksum's check bytes for `bytes`, as ECSS-E-70-41A Annex A.2
/// gives them: C0 and C1 start at 0; for each byte, C0 += byte and C1 +=
/// C0, modulo 255; then the first check byte is -(C0 + C1) and the second
/// C1, each 255 where it would be 0. The first is the high byte.
fn iso8473(bytes: &[u8]) -> u64 {
    let mut c0 = 0_u64;
    let mut c1 = 0_u64;
    // Reducing after every 4096 bytes instead of every byte keeps both
    // sums below 2^32, far from overflowing.
    for chunk in bytes.chunks(4096) {
        for &byte in chunk {
            c0 += u64::from(byte);
            c1 += c0;
        }
        c0 %= 255;
        c1 %= 255;
    }
    let first = (255 - (c0 + c1) % 255) % 255;
    let second = c1;
    let check = |byte: u64| if byte == 0 { 255 } else { byte };
    (check(first) << 8) | check(second)
}

impl Crc {
    /// The CRC catalogue's model, one bit at a time: the register starts
    /// at `init`; each byte (bit-reversed if `reflect_in`) goes into its
    /// top, and each bit shifted out of the top folds in `poly`; the result
    /// is bit-reversed if `reflect_out`, then exclusive-ored with
    /// `xor_out`.
    fn compute(&self, bytes: &[u8]) -> u64 {
        let width = self.width;
        let top = 1_u64 << (width - 1);
        let mut register = self.init;
        for &byte in bytes {
            let byte = if self.reflect_in {
                byte.reverse_bits()
            } else {
                byte
            };
            register ^= u64::from(byte) << (width - 8);
            for _ in 0..8 {
                register = if register & top != 0 {
                    (register << 1) ^ self.poly
                } else {
                    register << 1
                };
            }
            register &= mask(width);
        }
        if self.reflect_out {
            register = register.reverse_bits() >> (64 - width);
        }
        (register ^ self.xor_out) & mask(width)
    }
}

/// A checksum ready to compute quickly: a CRC carries a 256-entry table,
/// built from the bit-at-a-time model, so it gives the same results eight
/// bits at a time.
#[derive(Debug, Clone)]
pub(crate) struct Compiled {
    algorithm: Algorithm,
    table: Option<Box<[u64; 256]>>,
}

impl Compiled {
    pub(crate) fn new(algorithm: Algorithm) -> Compiled {
        let table = match algorithm {
            Algorithm::Crc(crc) => Some(Box::new(crc.table())),
            Algorithm::Sum { .. } | Algorithm::Xor | Algorithm::Iso8473 => None,
        };
        Compiled { algorithm, table }
    }

    pub(crate) fn algorithm(&self) -> &Algorithm {
        &self.algorithm
    }

    pub(crate) fn compute(&self, bytes: &[u8]) -> u64 {
        match (&self.algorithm, &self.table) {
            (Algorithm::Crc(crc), Some(table)) => crc.compute_with(table, bytes),
            (algorithm, _) => algorithm.compute(bytes),
        }
    }
}

impl Crc {
    /// What the register becomes for each value of its top byte, after
    /// eight shifts.
    fn table(&self) -> [u64; 256] {
        let top = 1_u64 << (self.width - 1);
        let mut table = [0_u64; 256];
        for (byte, entry) in (0_u64..).zip(table.iter_mut()) {
            let mut register = byte << (self.width - 8);
            for _ in 0..8 {
                register = if register & top != 0 {
                    (register << 1) ^ self.poly
                } else {
                    register << 1
                };
            }
            *entry = register & mask(self.width);
        }
        table
    }

    /// The same model as `compute`, a byte at a time through `table`.
    fn compute_with(&self, table: &[u64; 256], bytes: &[u8]) -> u64 {
        let shift = self.width - 8;
        let mut register = self.init;
        for &byte in bytes {
            let byte = if self.reflect_in {
                byte.reverse_bits()
            } else {
                byte
            };
            // The register's top byte and the input byte pick the entry; the
            // mask keeps the index within 0..=255.
            let index = (((register >> shift) ^ u64::from(byte)) & 0xFF) as usize;
            register = ((register << 8) ^ table[index]) & mask(self.width);
        }
        if self.reflect_out {
            register = register.reverse_bits() >> (64 - self.width);
        }
        (register ^ self.xor_out) & mask(self.width)
    }
}

/// A named CRC from the CRC catalogue (reveng.sourceforge.io).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Preset {
    /// The catalogue's name, like `CRC-16/IBM-3740`.
    pub name: &'static str,
    /// Other names it goes by, like `CRC-16/CCITT-FALSE`.
    pub aliases: &'static [&'static str],
    pub width: u8,
    pub poly: u64,
    pub init: u64,
    pub reflect_in: bool,
    pub reflect_out: bool,
    pub xor_out: u64,
    /// The CRC of the nine ASCII bytes `123456789`, as the catalogue gives
    /// it.
    pub check: u64,
}

impl Preset {
    /// The preset as an IR checksum.
    pub fn checksum(&self) -> Checksum {
        Checksum::crc(
            self.width,
            scalar(self.poly),
            scalar(self.init),
            self.reflect_in,
            self.reflect_out,
            scalar(self.xor_out),
        )
    }
}

/// An integer as the IR writes it: a number within ±(2^53 − 1), a decimal
/// string beyond.
fn scalar(value: u64) -> Scalar {
    match i64::try_from(value) {
        Ok(n) if value <= MAX_SAFE_INTEGER => Scalar::Int(n),
        _ => Scalar::Str(value.to_string()),
    }
}

/// The named CRCs MDL knows: common in space, avionics, industrial and
/// drone links.
pub const PRESETS: &[Preset] = &[
    Preset {
        name: "CRC-8/SMBUS",
        aliases: &[],
        width: 8,
        poly: 0x07,
        init: 0x00,
        reflect_in: false,
        reflect_out: false,
        xor_out: 0x00,
        check: 0xF4,
    },
    Preset {
        name: "CRC-16/ARC",
        aliases: &[],
        width: 16,
        poly: 0x8005,
        init: 0x0000,
        reflect_in: true,
        reflect_out: true,
        xor_out: 0x0000,
        check: 0xBB3D,
    },
    Preset {
        name: "CRC-16/IBM-3740",
        aliases: &["CRC-16/CCITT-FALSE"],
        width: 16,
        poly: 0x1021,
        init: 0xFFFF,
        reflect_in: false,
        reflect_out: false,
        xor_out: 0x0000,
        check: 0x29B1,
    },
    Preset {
        name: "CRC-16/XMODEM",
        aliases: &[],
        width: 16,
        poly: 0x1021,
        init: 0x0000,
        reflect_in: false,
        reflect_out: false,
        xor_out: 0x0000,
        check: 0x31C3,
    },
    Preset {
        name: "CRC-16/KERMIT",
        aliases: &[],
        width: 16,
        poly: 0x1021,
        init: 0x0000,
        reflect_in: true,
        reflect_out: true,
        xor_out: 0x0000,
        check: 0x2189,
    },
    Preset {
        name: "CRC-16/MODBUS",
        aliases: &[],
        width: 16,
        poly: 0x8005,
        init: 0xFFFF,
        reflect_in: true,
        reflect_out: true,
        xor_out: 0x0000,
        check: 0x4B37,
    },
    Preset {
        name: "CRC-16/MCRF4XX",
        aliases: &[],
        width: 16,
        poly: 0x1021,
        init: 0xFFFF,
        reflect_in: true,
        reflect_out: true,
        xor_out: 0x0000,
        check: 0x6F91,
    },
    Preset {
        name: "CRC-24/OPENPGP",
        aliases: &[],
        width: 24,
        poly: 0x86_4CFB,
        init: 0xB7_04CE,
        reflect_in: false,
        reflect_out: false,
        xor_out: 0x00_0000,
        check: 0x21_CF02,
    },
    Preset {
        name: "CRC-32/ISO-HDLC",
        aliases: &["CRC-32"],
        width: 32,
        poly: 0x04C1_1DB7,
        init: 0xFFFF_FFFF,
        reflect_in: true,
        reflect_out: true,
        xor_out: 0xFFFF_FFFF,
        check: 0xCBF4_3926,
    },
    Preset {
        name: "CRC-32/ISCSI",
        aliases: &["CRC-32C"],
        width: 32,
        poly: 0x1EDC_6F41,
        init: 0xFFFF_FFFF,
        reflect_in: true,
        reflect_out: true,
        xor_out: 0xFFFF_FFFF,
        check: 0xE306_9283,
    },
    Preset {
        name: "CRC-32/MPEG-2",
        aliases: &[],
        width: 32,
        poly: 0x04C1_1DB7,
        init: 0xFFFF_FFFF,
        reflect_in: false,
        reflect_out: false,
        xor_out: 0x0000_0000,
        check: 0x0376_E6E7,
    },
    Preset {
        name: "CRC-64/ECMA-182",
        aliases: &[],
        width: 64,
        poly: 0x42F0_E1EB_A9EA_3693,
        init: 0,
        reflect_in: false,
        reflect_out: false,
        xor_out: 0,
        check: 0x6C40_DF5F_0B49_7347,
    },
    Preset {
        name: "CRC-64/XZ",
        aliases: &[],
        width: 64,
        poly: 0x42F0_E1EB_A9EA_3693,
        init: u64::MAX,
        reflect_in: true,
        reflect_out: true,
        xor_out: u64::MAX,
        check: 0x995D_C9BB_DF19_39FA,
    },
];

/// The preset with this name or alias, ignoring case.
pub fn preset(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|preset| {
        preset.name.eq_ignore_ascii_case(name)
            || preset
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(name))
    })
}

#[cfg(test)]
mod tests;
