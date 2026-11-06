//! Instants: one type for every time MDL reads or compares.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

const NANOS_PER_SECOND: u32 = 1_000_000_000;

/// An instant in UTC: whole seconds since the Unix epoch
/// (1970-01-01T00:00:00Z) and nanoseconds into that second, the same shape
/// as protobuf's `Timestamp`. Leap seconds aren't counted, as in Unix time.
///
/// Every `Timestamp` lies between [`Timestamp::MIN`] and
/// [`Timestamp::MAX`] (years 0001 to 9999), the range its text form can
/// spell. Ordering is chronological.
///
/// The text form is MDL's one spelling of an instant:
/// `YYYY-MM-DDTHH:MM:SS`, then optionally `.` and 1 to 9 fraction digits
/// with no trailing zero, then `Z`. Uppercase `T` and `Z`, UTC only,
/// seconds 00 to 59. RFC 3339 allows many spellings of one instant
/// (offsets, lowercase, trailing zeros, leap seconds); one spelling keeps
/// one instant one digest, and every language can check it the same way.
///
/// ```
/// use mdl::v2::time::Timestamp;
///
/// let t: Timestamp = "2026-06-01T12:00:00.25Z".parse().unwrap();
/// assert_eq!((t.seconds(), t.nanos()), (1_780_315_200, 250_000_000));
/// assert_eq!(t.to_string(), "2026-06-01T12:00:00.25Z");
/// assert!("2026-06-01T12:00:00.250Z".parse::<Timestamp>().is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp {
    seconds: i64,
    nanos: u32,
}

impl Timestamp {
    /// 0001-01-01T00:00:00Z.
    pub const MIN: Timestamp = Timestamp {
        seconds: -62_135_596_800,
        nanos: 0,
    };
    /// 9999-12-31T23:59:59.999999999Z.
    pub const MAX: Timestamp = Timestamp {
        seconds: 253_402_300_799,
        nanos: NANOS_PER_SECOND - 1,
    };
    /// 1970-01-01T00:00:00Z.
    pub const UNIX_EPOCH: Timestamp = Timestamp {
        seconds: 0,
        nanos: 0,
    };

    /// Until this instant (2027-07-01T00:00:00Z) the leap-second table of
    /// `spec/decoding.md` §8.1 is known to be right: it's the next date a
    /// leap second could be inserted after the IERS Bulletin C the table
    /// follows. Event times on `tai`, `gps` or `tt` from then on use the
    /// table's last offset, and may differ by a second from a newer table.
    pub const LEAP_SECONDS_VALID_UNTIL: Timestamp = Timestamp {
        seconds: leap::VALID_UNTIL,
        nanos: 0,
    };

    /// `None` if `nanos` isn't below one second or the instant is outside
    /// [`MIN`](Timestamp::MIN)..=[`MAX`](Timestamp::MAX).
    pub fn new(seconds: i64, nanos: u32) -> Option<Timestamp> {
        let instant = Timestamp { seconds, nanos };
        (nanos < NANOS_PER_SECOND && (Self::MIN..=Self::MAX).contains(&instant)).then_some(instant)
    }

    /// Nanoseconds since the Unix epoch, the way most clocks and wire
    /// formats carry ground time. Every `i64` is in range (years 1677 to
    /// 2262).
    pub fn from_unix_nanos(nanos: i64) -> Timestamp {
        let per_second = i64::from(NANOS_PER_SECOND);
        Timestamp {
            seconds: nanos.div_euclid(per_second),
            // rem_euclid is in 0..per_second, so it fits.
            nanos: u32::try_from(nanos.rem_euclid(per_second)).unwrap_or(0),
        }
    }

    /// Whole seconds since the Unix epoch; negative before 1970.
    pub fn seconds(self) -> i64 {
        self.seconds
    }

    /// Nanoseconds into the second, always below 1,000,000,000 (also
    /// before 1970: one nanosecond before the epoch is second -1 plus
    /// 999,999,999 nanoseconds).
    pub fn nanos(self) -> u32 {
        self.nanos
    }

    /// Any RFC 3339 instant, the way payloads write them: any UTC offset,
    /// `T`, `t` or a space between date and time, `Z` or `z`, and any
    /// number of fraction digits (truncated to nanoseconds). A leap second
    /// (`23:59:60`) reads as the last nanosecond of the second before it,
    /// as Unix time has no leap seconds. MDL's own documents use the one
    /// spelling [`FromStr`] reads; this is for data MDL doesn't control.
    ///
    /// ```
    /// use mdl::v2::time::Timestamp;
    ///
    /// let t = Timestamp::from_rfc3339("2026-06-01T14:00:00.25+02:00").unwrap();
    /// assert_eq!(t.to_string(), "2026-06-01T12:00:00.25Z");
    /// ```
    pub fn from_rfc3339(text: &str) -> Result<Timestamp, TimestampError> {
        let error = || TimestampError {
            text: text.to_string(),
            expected: "an RFC 3339 instant with a UTC offset, between years 0001 and 9999",
        };
        let parsed = chrono::DateTime::parse_from_rfc3339(text).map_err(|_| error())?;
        let utc = parsed.to_utc();
        let nanos = utc.timestamp_subsec_nanos().min(NANOS_PER_SECOND - 1);
        Timestamp::new(utc.timestamp(), nanos).ok_or_else(error)
    }

    /// Nanoseconds since the Unix epoch, wide enough for any sum of a
    /// timestamp and an offset; `None` outside years 0001 to 9999.
    pub(crate) fn from_unix_nanos_wide(nanos: i128) -> Option<Timestamp> {
        let per_second = i128::from(NANOS_PER_SECOND);
        let seconds = i64::try_from(nanos.div_euclid(per_second)).ok()?;
        let nanos = u32::try_from(nanos.rem_euclid(per_second)).ok()?;
        Timestamp::new(seconds, nanos)
    }

    /// Nanoseconds since the Unix epoch.
    pub(crate) fn unix_nanos_wide(self) -> i128 {
        i128::from(self.seconds) * i128::from(NANOS_PER_SECOND) + i128::from(self.nanos)
    }
}

/// Text that isn't an instant in the form asked for.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{text:?} isn't {expected}")]
#[non_exhaustive]
pub struct TimestampError {
    pub text: String,
    expected: &'static str,
}

impl FromStr for Timestamp {
    type Err = TimestampError;

    fn from_str(text: &str) -> Result<Timestamp, TimestampError> {
        parse(text).ok_or_else(|| TimestampError {
            text: text.to_string(),
            expected: "an instant written YYYY-MM-DDTHH:MM:SS[.fraction]Z (UTC, no trailing \
                       zeros)",
        })
    }
}

fn parse(text: &str) -> Option<Timestamp> {
    let b = text.as_bytes();
    if b.len() < 20 || b.last() != Some(&b'Z') {
        return None;
    }
    let separators =
        b[4] == b'-' && b[7] == b'-' && b[10] == b'T' && b[13] == b':' && b[16] == b':';
    if !separators {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<u32> {
        let part = &b[range];
        if !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        Some(part.iter().fold(0, |n, d| n * 10 + u32::from(d - b'0')))
    };
    let year = number(0..4)?;
    let month = number(5..7)?;
    let day = number(8..10)?;
    let hour = number(11..13)?;
    let minute = number(14..16)?;
    let second = number(17..19)?;
    if year == 0 || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let date = chrono::NaiveDate::from_ymd_opt(i32::try_from(year).ok()?, month, day)?;
    let seconds = date
        .and_hms_opt(hour, minute, second)?
        .and_utc()
        .timestamp();
    let nanos = fraction(&b[19..b.len() - 1])?;
    Timestamp::new(seconds, nanos)
}

/// `.` and 1 to 9 digits with no trailing zero, as nanoseconds; nothing is 0.
fn fraction(text: &[u8]) -> Option<u32> {
    if text.is_empty() {
        return Some(0);
    }
    let digits = text.strip_prefix(b".")?;
    let well_formed = (1..=9).contains(&digits.len())
        && digits.iter().all(u8::is_ascii_digit)
        && digits.last() != Some(&b'0');
    if !well_formed {
        return None;
    }
    let mut nanos = 0;
    for place in 0..9 {
        let digit = digits.get(place).map_or(0, |d| u32::from(d - b'0'));
        nanos = nanos * 10 + digit;
    }
    Some(nanos)
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Every Timestamp is within years 0001 to 9999, far inside the range
        // chrono can represent, so this always finds the date.
        let Some(time) = chrono::DateTime::from_timestamp(self.seconds, 0) else {
            return write!(f, "{}s", self.seconds);
        };
        write!(f, "{}", time.format("%Y-%m-%dT%H:%M:%S"))?;
        if self.nanos > 0 {
            let digits = format!("{:09}", self.nanos);
            write!(f, ".{}", digits.trim_end_matches('0'))?;
        }
        f.write_str("Z")
    }
}

pub(crate) mod leap;

#[cfg(test)]
mod tests;
