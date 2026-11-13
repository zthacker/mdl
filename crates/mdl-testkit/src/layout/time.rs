//! Event time (`spec/decoding.md` §8): time sources and their fields,
//! epochs, scales and leap seconds, and the instant a message's fields
//! give.

use std::collections::BTreeSet;

use proptest::collection::btree_set;
use proptest::prelude::*;
use proptest::sample::select;
use serde_json::{Value, json};

use super::{Raw, fit, sign_extend};

/// What a message's event time is (§8): ground time only, or a UTC
/// instant in nanoseconds since 1970-01-01T00:00:00Z (Unix time, which has
/// no leap seconds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventTime {
    Ground,
    At(i128),
    /// It can't be read: a count that isn't a number, or an instant
    /// outside years 0001 to 9999 (`invalid_time`).
    Invalid,
    /// Time since the asset booted, in the source's unit.
    SinceBoot {
        value: u64,
        unit: &'static str,
    },
}

pub(super) const SECOND: i128 = 1_000_000_000;
pub(super) const WEEK: i128 = 604_800 * SECOND;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Scale {
    Utc,
    Tai,
    Gps,
    Tt,
}

impl Scale {
    pub(super) fn name(self) -> &'static str {
        match self {
            Scale::Utc => "utc",
            Scale::Tai => "tai",
            Scale::Gps => "gps",
            Scale::Tt => "tt",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Unit {
    S,
    Ms,
    Us,
    Ns,
}

impl Unit {
    pub(super) fn name(self) -> &'static str {
        match self {
            Unit::S => "s",
            Unit::Ms => "ms",
            Unit::Us => "us",
            Unit::Ns => "ns",
        }
    }

    pub(super) fn nanos(self) -> i128 {
        match self {
            Unit::S => SECOND,
            Unit::Ms => 1_000_000,
            Unit::Us => 1_000,
            Unit::Ns => 1,
        }
    }

    /// The widest unsigned count, in whole bytes, that keeps an event time
    /// within years 0001 to 9999 from any epoch generated here.
    pub(super) fn count_bits(self) -> u32 {
        match self {
            Unit::S => 32,
            Unit::Ms => 40,
            Unit::Us => 48,
            Unit::Ns => 64,
        }
    }
}

/// An epoch: its text in MDL's one spelling, and its label (nanoseconds
/// since 1970-01-01T00:00:00 on its scale, every day 86,400 s).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Epoch {
    pub(super) text: String,
    pub(super) label: i128,
}

/// A set's time source.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum TimeSource {
    None,
    Cuc {
        coarse: u32,
        fine: u32,
        epoch: Epoch,
        scale: Scale,
    },
    Elapsed {
        unit: Unit,
        epoch: Epoch,
        scale: Scale,
    },
    Gps {
        unit: Unit,
        week_bits: Option<u32>,
    },
    /// Time since boot in each timed packet's `t`, which a correlation
    /// packet's absolute clock (in `clock_unit` from `epoch` on `scale`)
    /// and boot clock tie to an instant (§8.2).
    Boot {
        unit: Unit,
        clock_unit: Unit,
        epoch: Epoch,
        scale: Scale,
    },
}

/// A packet's count field: an elapsed count, or a GPS time of week.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Count {
    Uint(u32),
    Int(u32),
    Float(u32),
}

impl Count {
    pub(super) fn bits(self) -> u32 {
        match self {
            Count::Uint(bits) | Count::Int(bits) | Count::Float(bits) => bits,
        }
    }
}

/// The days from 1970-01-01 to a date in the proleptic Gregorian
/// calendar (Howard Hinnant's `days_from_civil`).
pub(super) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// A date and time as a label: nanoseconds since 1970-01-01T00:00:00.
pub(crate) fn label(date: (i64, i64, i64), time: (i64, i64, i64), nanos: i128) -> i128 {
    let (year, month, day) = date;
    let (hour, minute, second) = time;
    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
    i128::from(seconds) * SECOND + nanos
}

/// TAI − UTC from each date on (UTC), as `spec/decoding.md` §8.1 lists it
/// after IERS Bulletin C.
pub(super) const LEAPS: &[((i64, i64, i64), i128)] = &[
    ((1972, 1, 1), 10),
    ((1972, 7, 1), 11),
    ((1973, 1, 1), 12),
    ((1974, 1, 1), 13),
    ((1975, 1, 1), 14),
    ((1976, 1, 1), 15),
    ((1977, 1, 1), 16),
    ((1978, 1, 1), 17),
    ((1979, 1, 1), 18),
    ((1980, 1, 1), 19),
    ((1981, 7, 1), 20),
    ((1982, 7, 1), 21),
    ((1983, 7, 1), 22),
    ((1985, 7, 1), 23),
    ((1988, 1, 1), 24),
    ((1990, 1, 1), 25),
    ((1991, 1, 1), 26),
    ((1992, 7, 1), 27),
    ((1993, 7, 1), 28),
    ((1994, 7, 1), 29),
    ((1996, 1, 1), 30),
    ((1997, 7, 1), 31),
    ((1999, 1, 1), 32),
    ((2006, 1, 1), 33),
    ((2009, 1, 1), 34),
    ((2012, 7, 1), 35),
    ((2015, 7, 1), 36),
    ((2017, 1, 1), 37),
];

/// Each entry's start, as a UTC instant, and its offset, in nanoseconds.
pub(super) fn leaps() -> impl Iterator<Item = (i128, i128)> {
    LEAPS
        .iter()
        .map(|&(date, offset)| (label(date, (0, 0, 0), 0), offset * SECOND))
}

/// The UTC instant of a TAI label (§8.1), a label inside a leap second
/// being the last nanosecond before it.
pub(super) fn tai_to_utc(tai: i128) -> i128 {
    let mut instant = tai - 10 * SECOND;
    let mut previous: Option<i128> = None;
    for (start, offset) in leaps() {
        let before = previous.unwrap_or(offset);
        if tai >= start + before && tai < start + offset {
            return start - 1;
        }
        if start + offset <= tai {
            instant = tai - offset;
        }
        previous = Some(offset);
    }
    instant
}

/// The UTC instant of a label on `scale`.
pub(super) fn to_utc(label: i128, scale: Scale) -> i128 {
    match scale {
        Scale::Utc => label,
        Scale::Tai => tai_to_utc(label),
        Scale::Gps => tai_to_utc(label + 19 * SECOND),
        Scale::Tt => tai_to_utc(label - 32_184_000_000),
    }
}

/// TAI − UTC in effect at a UTC instant.
pub(super) fn tai_minus_utc(instant: i128) -> i128 {
    leaps()
        .filter(|(start, _)| *start <= instant)
        .last()
        .map_or(10 * SECOND, |(_, offset)| offset)
}

/// The GPS epoch, 1980-01-06T00:00:00, as a label on the gps scale.
pub(super) fn gps_epoch() -> i128 {
    label((1980, 1, 6), (0, 0, 0), 0)
}

/// The full GPS week (§8): the field's value, or with `week_bits` the week
/// congruent to it nearest the ground time's.
pub(super) fn gps_week(value: u64, week_bits: Option<u32>, ground: i128) -> i128 {
    let Some(bits) = week_bits else {
        return i128::from(value);
    };
    let modulus = 1_i128 << bits;
    let w = i128::from(value) % modulus;
    let ground_label = ground + tai_minus_utc(ground) - 19 * SECOND;
    let g = (ground_label - gps_epoch()).div_euclid(WEEK);
    let c = (g - w + modulus / 2).div_euclid(modulus).max(0);
    w + c * modulus
}

/// A count of `unit`s in nanoseconds: an integer exactly, a float times the
/// unit in binary64, rounded down. `None` for a float that isn't a number,
/// or is so large no instant in range is that far from any epoch.
pub(super) fn count_nanos(raw: &Raw, unit: Unit) -> Option<i128> {
    match raw {
        Raw::Uint(n) => Some(i128::from(*n) * unit.nanos()),
        Raw::Int(n) => Some(i128::from(*n) * unit.nanos()),
        Raw::Float(x) => {
            let nanos = (x * unit.nanos() as f64).floor();
            (nanos.is_finite() && nanos.abs() < 1e30).then_some(nanos as i128)
        }
        _ => None,
    }
}

/// The UTC instant of a label, if it can be read and lies within years
/// 0001 to 9999.
pub(super) fn instant(on_scale: Option<i128>, scale: Scale) -> EventTime {
    let first = label((1, 1, 1), (0, 0, 0), 0);
    let last = label((9999, 12, 31), (23, 59, 59), SECOND - 1);
    match on_scale.map(|on_scale| to_utc(on_scale, scale)) {
        Some(at) if (first..=last).contains(&at) => EventTime::At(at),
        _ => EventTime::Invalid,
    }
}

pub(super) fn time_scale() -> BoxedStrategy<Scale> {
    select(&[Scale::Utc, Scale::Tai, Scale::Gps, Scale::Tt][..]).boxed()
}

pub(super) fn unit() -> BoxedStrategy<Unit> {
    select(&[Unit::S, Unit::Ms, Unit::Us, Unit::Ns][..]).boxed()
}

/// An epoch: one of the well-known ones, or any instant from 1960 to 2030
/// to the millisecond, spelled as MDL spells instants.
pub(super) fn epoch() -> BoxedStrategy<Epoch> {
    let known = select(
        &[
            ("1958-01-01T00:00:00Z", (1958, 1, 1), (0, 0, 0), 0),
            ("1970-01-01T00:00:00Z", (1970, 1, 1), (0, 0, 0), 0),
            ("1980-01-06T00:00:00Z", (1980, 1, 6), (0, 0, 0), 0),
            ("2000-01-01T11:58:55.816Z", (2000, 1, 1), (11, 58, 55), 816),
            ("2000-01-01T12:00:00Z", (2000, 1, 1), (12, 0, 0), 0),
        ][..],
    )
    .prop_map(|(text, date, time, millis)| Epoch {
        text: text.to_string(),
        label: label(date, time, i128::from(millis) * 1_000_000),
    });
    // Any day of the month, leap days included (2000 is a leap year).
    let date = (1960_i64..=2030, 1_i64..=12).prop_flat_map(|(year, month)| {
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let days = match month {
            2 if leap => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        (Just(year), Just(month), 1_i64..=days)
    });
    let any = (date, (0_i64..24, 0_i64..60, 0_i64..60), 0_i128..1000).prop_map(
        |((year, month, day), (hour, minute, second), millis)| {
            let fraction = if millis == 0 {
                String::new()
            } else {
                format!(".{millis:03}").trim_end_matches('0').to_string()
            };
            Epoch {
                text: format!(
                    "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{fraction}Z"
                ),
                label: label(
                    (year, month, day),
                    (hour, minute, second),
                    millis * 1_000_000,
                ),
            }
        },
    );
    prop_oneof![known, any].boxed()
}

pub(super) fn time_source() -> BoxedStrategy<TimeSource> {
    prop_oneof![
        1 => Just(TimeSource::None),
        2 => (1_u32..=4, 0_u32..=3, epoch(), time_scale()).prop_map(|(coarse, fine, epoch, scale)| {
            TimeSource::Cuc { coarse, fine, epoch, scale }
        }),
        2 => (unit(), epoch(), time_scale())
            .prop_map(|(unit, epoch, scale)| TimeSource::Elapsed { unit, epoch, scale }),
        2 => (unit(), proptest::option::of(select(&[10_u32, 13, 16][..])))
            .prop_map(|(unit, week_bits)| TimeSource::Gps { unit, week_bits }),
    ]
    .boxed()
}

/// A packet's time fields for the source: its count field, if it has one,
/// and up to one sentinel each for the count and a GPS week.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct TimeFields {
    pub(super) count: Option<Count>,
    pub(super) count_invalid: Vec<i128>,
    pub(super) week_invalid: Vec<u64>,
    /// Whether the count and week restate the unit their source counts
    /// them in, which validation allows (§8).
    pub(super) units: bool,
}

pub(super) fn time_fields(time: &TimeSource) -> BoxedStrategy<TimeFields> {
    let gps = matches!(time, TimeSource::Gps { .. });
    count(time)
        .prop_flat_map(move |count| {
            let range = match count {
                Some(Count::Uint(bits)) => 0..=(1_i128 << bits) - 1,
                Some(Count::Int(bits)) => -(1_i128 << (bits - 1))..=(1_i128 << (bits - 1)) - 1,
                _ => -1000..=1000,
            };
            let count_invalid = match count {
                Some(_) => btree_set(range, 0..=1).boxed(),
                None => Just(BTreeSet::new()).boxed(),
            };
            let week_invalid = if gps && count.is_some() {
                btree_set(0_u64..=u64::from(u16::MAX), 0..=1).boxed()
            } else {
                Just(BTreeSet::new()).boxed()
            };
            (Just(count), count_invalid, week_invalid, any::<bool>())
        })
        .prop_map(|(count, count_invalid, week_invalid, units)| TimeFields {
            count,
            count_invalid: count_invalid.into_iter().collect(),
            week_invalid: week_invalid.into_iter().collect(),
            units,
        })
        .boxed()
}

/// A packet's count field for the source, if it has one: whole bytes, wide
/// enough for the source's counts and no wider than keeps them in range.
pub(super) fn count(time: &TimeSource) -> BoxedStrategy<Option<Count>> {
    let widths = |least: u32, most: u32| -> Vec<u32> { (least..=most).step_by(8).collect() };
    match time {
        TimeSource::None | TimeSource::Cuc { .. } => Just(None).boxed(),
        TimeSource::Boot { .. } => {
            proptest::option::of(select(widths(8, 32)).prop_map(Count::Uint)).boxed()
        }
        // Now and then 64 bits, whose counts can run past year 9999.
        TimeSource::Elapsed { unit, .. } => {
            let bits = widths(8, unit.count_bits());
            proptest::option::of(prop_oneof![
                3 => select(bits.clone()).prop_map(Count::Uint),
                3 => select(bits).prop_map(Count::Int),
                1 => select(&[Count::Uint(64), Count::Int(64)][..]),
                2 => select(&[32_u32, 64][..]).prop_map(Count::Float),
            ])
            .boxed()
        }
        // A time of week up to 604,800 s in the unit.
        TimeSource::Gps { unit, .. } => {
            let least = match unit {
                Unit::S => 24,
                Unit::Ms => 32,
                Unit::Us => 40,
                Unit::Ns => 56,
            };
            proptest::option::of(prop_oneof![
                select(widths(least, 64)).prop_map(Count::Uint),
                select(&[32_u32, 64][..]).prop_map(Count::Float),
            ])
            .boxed()
        }
    }
}

/// A count field's value: mostly one that keeps the event time in range
/// (up to 2^31 s from the epoch, or a time of week up to two weeks, which
/// the spec adds as it is), and now and then a float that isn't a number
/// or is far too large.
pub(super) fn count_value(time: &TimeSource, count: Count) -> BoxedStrategy<Raw> {
    let (unit, gps) = match time {
        TimeSource::Elapsed { unit, .. } => (*unit, false),
        TimeSource::Gps { unit, .. } => (*unit, true),
        _ => (Unit::S, false),
    };
    let span = if gps {
        (2 * WEEK / unit.nanos()) as f64
    } else {
        2_147_483_648.0 * (SECOND / unit.nanos()) as f64
    };
    let unreadable = select(&[f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e300, -1e300][..])
        .prop_map(Raw::Float)
        .boxed();
    let low = if gps { 0.0 } else { -span };
    // An elapsed integer count lands near a leap second often: the count
    // from the epoch to it, where the field holds that, else any count.
    let near = |signed: bool, bits: u32| -> BoxedStrategy<Raw> {
        let TimeSource::Elapsed { epoch, scale, .. } = time else {
            return Just(Raw::Uint(0)).boxed();
        };
        let start = epoch.label;
        (near_leap(*scale), any::<u64>())
            .prop_map(move |(label, fallback)| {
                let n = (label - start).div_euclid(unit.nanos());
                let (low, high) = if signed {
                    (-(1_i128 << (bits - 1)), (1_i128 << (bits - 1)) - 1)
                } else {
                    (0, (1_i128 << bits) - 1)
                };
                match (signed, (low..=high).contains(&n)) {
                    (true, true) => Raw::Int(i64::try_from(n).unwrap_or(0)),
                    (false, true) => Raw::Uint(u64::try_from(n).unwrap_or(0)),
                    (true, false) => Raw::Int(sign_extend(fit(fallback, bits), bits)),
                    (false, false) => Raw::Uint(fit(fallback, bits)),
                }
            })
            .boxed()
    };
    match count {
        Count::Uint(bits) if gps => (0..(2 * WEEK / unit.nanos()) as u64)
            .prop_map(move |n| Raw::Uint(fit(n, bits)))
            .boxed(),
        Count::Uint(bits) => prop_oneof![
            2 => any::<u64>().prop_map(move |n| Raw::Uint(fit(n, bits))),
            1 => near(false, bits),
        ]
        .boxed(),
        Count::Int(bits) => prop_oneof![
            2 => any::<u64>().prop_map(move |n| Raw::Int(sign_extend(fit(n, bits), bits))),
            1 => near(true, bits),
        ]
        .boxed(),
        // A binary32 count is the double it widens to, whatever decimal
        // it looks like.
        Count::Float(32) => prop_oneof![
            8 => (low..span)
                .prop_map(|x| Raw::Float(f64::from(x as f32)))
                .prop_filter("within the span", move |raw| match raw {
                    Raw::Float(x) => *x >= low && *x < span,
                    _ => true,
                }),
            // ±1e300 is an infinity in binary32.
            1 => unreadable.prop_map(|raw| match raw {
                Raw::Float(x) => Raw::Float(f64::from(x as f32)),
                other => other,
            }),
        ]
        .boxed(),
        Count::Float(_) => prop_oneof![
            8 => (low..span).prop_map(Raw::Float),
            1 => unreadable,
        ]
        .boxed(),
    }
}

/// A message's ground time: from 1990 to 2035, near the GPS week rollovers
/// of 1999-08-22 and 2019-04-07, and within two seconds of a leap step
/// (where the ground's TAI − UTC changes), more often.
pub(super) fn received() -> BoxedStrategy<i128> {
    let rollover = |date| {
        let at = label(date, (0, 0, 0), 0);
        (at - 30 * 86_400 * SECOND..at + 30 * 86_400 * SECOND).boxed()
    };
    let steps: Vec<i128> = leaps()
        .map(|(start, _)| start)
        .filter(|start| *start >= label((1990, 1, 1), (0, 0, 0), 0))
        .collect();
    prop_oneof![
        3 => label((1990, 1, 1), (0, 0, 0), 0)..label((2035, 1, 1), (0, 0, 0), 0),
        1 => rollover((1999, 8, 22)),
        1 => rollover((2019, 4, 7)),
        1 => (select(steps), -2 * SECOND..2 * SECOND).prop_map(|(start, delta)| start + delta),
    ]
    .boxed()
}

/// A GPS week field's value for a message received at `ground`: any, or
/// with `week_bits` one exactly half a cycle from the ground's week either
/// way, where "nearest" ties.
pub(super) fn week_value(time: &TimeSource, ground: i128) -> BoxedStrategy<u16> {
    let TimeSource::Gps {
        week_bits: Some(bits),
        ..
    } = time
    else {
        return any::<u16>().boxed();
    };
    let modulus = 1_i128 << bits;
    let g = (ground + tai_minus_utc(ground) - 19 * SECOND - gps_epoch()).div_euclid(WEEK);
    let ties: Vec<u16> = [g + modulus / 2, g - modulus / 2]
        .iter()
        .map(|w| u16::try_from(w.rem_euclid(modulus)).unwrap_or(0))
        .collect();
    prop_oneof![2 => any::<u16>(), 1 => select(ties)].boxed()
}

/// A label on `scale` within two seconds of a leap second: of the moment
/// TAI − UTC steps, on `tai`, `gps` and `tt`, or of an entry's start, on
/// `utc`.
pub(super) fn near_leap(scale: Scale) -> BoxedStrategy<i128> {
    let mut entries = Vec::new();
    let mut previous: Option<i128> = None;
    for (start, offset) in leaps() {
        entries.push((start, previous.unwrap_or(offset)));
        previous = Some(offset);
    }
    (select(entries), -2 * SECOND..2 * SECOND)
        .prop_map(move |((start, before), delta)| {
            let tai = start + before + delta;
            match scale {
                Scale::Utc => start + delta,
                Scale::Tai => tai,
                Scale::Gps => tai - 19 * SECOND,
                Scale::Tt => tai + 32_184_000_000,
            }
        })
        .boxed()
}

/// The set's time source as IR JSON, a CUC code at `cuc_offset`.
pub(super) fn time_json(time: &TimeSource, cuc_offset: u64) -> Option<Value> {
    Some(match time {
        TimeSource::None => return None,
        TimeSource::Cuc {
            coarse,
            fine,
            epoch,
            scale,
        } => json!({
            "kind": "cuc", "offset_bits": cuc_offset, "coarse_bytes": coarse, "fine_bytes": fine,
            "epoch": epoch.text, "scale": scale.name()
        }),
        TimeSource::Elapsed { unit, epoch, scale } => json!({
            "kind": "elapsed", "unit": unit.name(), "epoch": epoch.text, "scale": scale.name()
        }),
        TimeSource::Gps { unit, week_bits } => {
            let mut gps = json!({ "kind": "gps", "unit": unit.name(), "week_field": "week" });
            if let Some(bits) = week_bits {
                gps["week_bits"] = json!(bits);
            }
            gps
        }
        TimeSource::Boot {
            unit,
            clock_unit,
            epoch,
            scale,
        } => json!({
            "kind": "boot", "unit": unit.name(),
            "correlate": {
                "packet": "CORR", "clock_field": "clock", "clock_unit": clock_unit.name(),
                "epoch": epoch.text, "scale": scale.name(), "boot_field": "boot"
            }
        }),
    })
}

/// Whether a packet's time fields hold a sentinel (§7.2): its count, or a
/// GPS week beside a count. Then it has no reading, and ground time.
pub(super) fn no_reading(
    time: &TimeSource,
    count: Option<&Raw>,
    week: u16,
    count_invalid: &[i128],
    week_invalid: &[u64],
) -> bool {
    let count_sentinel = count.is_some_and(|raw| {
        count_invalid.iter().any(|n| match raw {
            Raw::Uint(m) => i128::from(*m) == *n,
            Raw::Int(m) => i128::from(*m) == *n,
            Raw::Float(x) => *x == *n as f64,
            _ => false,
        })
    });
    let week_sentinel = matches!(time, TimeSource::Gps { .. })
        && count.is_some()
        && week_invalid.contains(&u64::from(week));
    count_sentinel || week_sentinel
}

/// How an RFC 3339 instant is written (§8).
#[derive(Debug, Clone)]
pub(super) struct Rfc3339 {
    /// The UTC offset, in minutes: −23:59 to +23:59.
    pub(super) offset: i32,
    /// `T`, `t` or a space.
    pub(super) separator: char,
    /// For an offset of 0: `Z`, `z`, or `+00:00`.
    pub(super) zone: usize,
    /// Fraction digits, 0 to 12; past nine, digits reading drops.
    pub(super) digits: usize,
    /// Whether the minute's second 59 is written as 60, which reads as its
    /// last nanosecond.
    pub(super) leap: bool,
}

/// Any way §8 lets an RFC 3339 instant be written.
pub(super) fn rfc3339_form() -> BoxedStrategy<Rfc3339> {
    (
        prop_oneof![
            3 => Just(0_i32),
            2 => -1439_i32..=1439,
            1 => select(&[-1439_i32, 1439, -60, 60, 330, -570][..]),
        ],
        select(&['T', 't', ' '][..]),
        0_usize..3,
        prop_oneof![3 => Just(0_usize), 3 => 1_usize..=9, 1 => 10_usize..=12],
        proptest::bool::weighted(0.1),
    )
        .prop_map(|(offset, separator, zone, digits, leap)| Rfc3339 {
            offset,
            separator,
            zone,
            digits,
            leap,
        })
        .boxed()
}

/// `unix` (nanoseconds) written as `form` says, and the instant the text
/// reads as (§8): its fraction cut to the digits written, and a second of
/// 60 the last nanosecond of second 59, whatever its fraction. `None`
/// where the local date isn't in years 0000 to 9999, which four digits
/// write.
pub(super) fn rfc3339(unix: i128, form: &Rfc3339) -> Option<(String, i128)> {
    let offset = i128::from(form.offset) * 60 * SECOND;
    let local = unix + offset;
    let seconds = local.div_euclid(SECOND);
    let nanos = local.rem_euclid(SECOND);
    let (year, month, day) = civil_from_days(i64::try_from(seconds.div_euclid(86_400)).ok()?);
    if !(0..=9999).contains(&year) {
        return None;
    }
    let of_day = seconds.rem_euclid(86_400);
    let (hour, minute, second) = (of_day / 3600, of_day % 3600 / 60, of_day % 60);
    let minute_start = local - second * SECOND - nanos;
    let (second, read) = if form.leap {
        (60, minute_start + 60 * SECOND - 1 - offset)
    } else {
        let cut = 10_i128.pow(9_u32.saturating_sub(form.digits as u32));
        (second, local - nanos % cut - offset)
    };
    let mut fraction = String::new();
    if form.digits > 0 {
        let digits = format!("{nanos:09}");
        fraction.push('.');
        fraction.push_str(&digits[..form.digits.min(9)]);
        for k in 9..form.digits {
            fraction.push(char::from(b'0' + ((nanos + k as i128) % 10) as u8));
        }
    }
    let zone = match (form.offset, form.zone) {
        (0, 0) => "Z".to_string(),
        (0, 1) => "z".to_string(),
        (offset, _) => {
            let sign = if offset < 0 { '-' } else { '+' };
            format!("{sign}{:02}:{:02}", offset.abs() / 60, offset.abs() % 60)
        }
    };
    let separator = form.separator;
    let text = format!(
        "{year:04}-{month:02}-{day:02}{separator}{hour:02}:{minute:02}:{second:02}{fraction}{zone}"
    );
    Some((text, read))
}

/// A packet's event time (§8, §8.1); `payload` starts where its time
/// fields do.
pub(super) fn event_time(
    time: &TimeSource,
    payload: &[u8],
    count: Option<&Raw>,
    week: u64,
    ground: i128,
) -> EventTime {
    match time {
        TimeSource::None => EventTime::Ground,
        TimeSource::Cuc {
            coarse,
            fine,
            epoch,
            scale,
        } => {
            let big = |bytes: &[u8]| bytes.iter().fold(0_i128, |n, b| (n << 8) | i128::from(*b));
            let coarse_end = *coarse as usize;
            let seconds = big(&payload[..coarse_end]);
            let fraction = big(&payload[coarse_end..coarse_end + *fine as usize]);
            let nanos = fraction * SECOND / (1_i128 << (8 * fine));
            instant(Some(epoch.label + seconds * SECOND + nanos), *scale)
        }
        TimeSource::Elapsed { unit, epoch, scale } => match count {
            Some(raw) => instant(
                count_nanos(raw, *unit).map(|nanos| epoch.label + nanos),
                *scale,
            ),
            None => EventTime::Ground,
        },
        TimeSource::Gps { unit, week_bits } => match count {
            Some(raw) => {
                let week = gps_week(week, *week_bits, ground);
                let label = count_nanos(raw, *unit).map(|nanos| gps_epoch() + week * WEEK + nanos);
                instant(label, Scale::Gps)
            }
            None => EventTime::Ground,
        },
        TimeSource::Boot { unit, .. } => match count {
            Some(Raw::Uint(value)) => EventTime::SinceBoot {
                value: *value,
                unit: unit.name(),
            },
            _ => EventTime::Ground,
        },
    }
}

/// A Unix instant in MDL's spelling, its fraction without trailing zeros.
pub(crate) fn instant_text(unix: i128) -> String {
    let seconds = unix.div_euclid(SECOND);
    let nanos = unix.rem_euclid(SECOND);
    let days = seconds.div_euclid(86_400);
    let of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(i64::try_from(days).unwrap_or(0));
    let fraction = if nanos == 0 {
        String::new()
    } else {
        format!(".{nanos:09}").trim_end_matches('0').to_string()
    };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}{fraction}Z",
        of_day / 3600,
        of_day % 3600 / 60,
        of_day % 60
    )
}

/// The date `days` after 1970-01-01 (Howard Hinnant's `civil_from_days`).
pub(crate) fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}
