//! The leap-second table, and moving between a time scale and UTC, as
//! `spec/decoding.md` §8.1 defines them.
//!
//! A *label* on a scale is nanoseconds since 1970-01-01T00:00:00 on that
//! scale, counting every day as 86,400 s: for `utc` that's Unix time; for
//! `tai`, `gps` and `tt`, which count every second, labels run ahead of UTC
//! by the leap seconds (less 19 s for `gps`, plus 32.184 s for `tt`).

use crate::v2::ir::TimeScale;

const NANOS: i128 = 1_000_000_000;

/// GPS time is TAI 19 s behind.
const GPS_BEHIND_TAI: i128 = 19 * NANOS;

/// Terrestrial Time is TAI 32.184 s ahead.
const TT_AHEAD_OF_TAI: i128 = 32_184_000_000;

/// The table is known to be right until this instant (Unix seconds,
/// 2027-07-01T00:00:00Z): the next date a leap second could be inserted
/// after the Bulletin C it follows.
pub(crate) const VALID_UNTIL: i64 = 1_814_400_000;

/// TAI − UTC in seconds, from each UTC instant (Unix seconds) on, as the
/// IERS has published it through Bulletin C 72 (July 2026), which announced
/// no leap second at the end of 2026: 10 s in 1972, then the 27 leap
/// seconds since. Before the first entry, its offset applies; after the
/// last, until a new table, the last.
pub(crate) const LEAP_SECONDS: &[(i64, i64)] = &[
    (63_072_000, 10),    // 1972-01-01
    (78_796_800, 11),    // 1972-07-01
    (94_694_400, 12),    // 1973-01-01
    (126_230_400, 13),   // 1974-01-01
    (157_766_400, 14),   // 1975-01-01
    (189_302_400, 15),   // 1976-01-01
    (220_924_800, 16),   // 1977-01-01
    (252_460_800, 17),   // 1978-01-01
    (283_996_800, 18),   // 1979-01-01
    (315_532_800, 19),   // 1980-01-01
    (362_793_600, 20),   // 1981-07-01
    (394_329_600, 21),   // 1982-07-01
    (425_865_600, 22),   // 1983-07-01
    (489_024_000, 23),   // 1985-07-01
    (567_993_600, 24),   // 1988-01-01
    (631_152_000, 25),   // 1990-01-01
    (662_688_000, 26),   // 1991-01-01
    (709_948_800, 27),   // 1992-07-01
    (741_484_800, 28),   // 1993-07-01
    (773_020_800, 29),   // 1994-07-01
    (820_454_400, 30),   // 1996-01-01
    (867_715_200, 31),   // 1997-07-01
    (915_148_800, 32),   // 1999-01-01
    (1_136_073_600, 33), // 2006-01-01
    (1_230_768_000, 34), // 2009-01-01
    (1_341_100_800, 35), // 2012-07-01
    (1_435_708_800, 36), // 2015-07-01
    (1_483_228_800, 37), // 2017-01-01
];

/// The table's entries in nanoseconds: (UTC start, TAI − UTC from then).
fn entries() -> impl Iterator<Item = (i128, i128)> {
    LEAP_SECONDS
        .iter()
        .map(|&(start, offset)| (i128::from(start) * NANOS, i128::from(offset) * NANOS))
}

/// The table's first offset, which applies before it.
fn first_offset() -> i128 {
    LEAP_SECONDS
        .first()
        .map_or(0, |&(_, offset)| i128::from(offset) * NANOS)
}

/// A label on `scale` as Unix nanoseconds. A TAI or GPS label inside a
/// leap second (UTC 23:59:60) is held to 23:59:59.999999999, as an RFC 3339
/// leap second is.
pub(crate) fn to_utc(scale: TimeScale, label: i128) -> i128 {
    let tai = match scale {
        TimeScale::Utc => return label,
        TimeScale::Tai => label,
        TimeScale::Gps => label + GPS_BEHIND_TAI,
        TimeScale::Tt => label - TT_AHEAD_OF_TAI,
    };
    let mut offset = first_offset();
    for (start, after) in entries() {
        // From `start` on in UTC, which is `start + after` in TAI.
        if tai >= start + after {
            offset = after;
            continue;
        }
        // The leap second before `start`: TAI from `start + offset`.
        if tai >= start + offset {
            return start - 1;
        }
        break;
    }
    tai - offset
}

/// A Unix instant (nanoseconds) as a label on `scale`.
pub(crate) fn from_utc(scale: TimeScale, utc: i128) -> i128 {
    let offset = entries()
        .take_while(|(start, _)| *start <= utc)
        .last()
        .map_or_else(first_offset, |(_, offset)| offset);
    match scale {
        TimeScale::Utc => utc,
        TimeScale::Tai => utc + offset,
        TimeScale::Gps => utc + offset - GPS_BEHIND_TAI,
        TimeScale::Tt => utc + offset + TT_AHEAD_OF_TAI,
    }
}

#[cfg(test)]
mod tests;
