//! Naming and number rules shared by the checks.

use crate::v2::diagnostics::{Code, Place, Report};
use crate::v2::ir::Scalar;

/// The largest integer every JSON implementation reads exactly.
pub(super) const MAX_SAFE_INTEGER: i64 = (1 << 53) - 1;

/// Family, set and metadata names: lowercase letters, digits and `_`,
/// starting with a letter. They end up in subjects, topics and file names.
pub(super) fn is_token(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Packet, entry and struct type names: letters, digits and `_`, not
/// starting with a digit. No dots or brackets, because output paths join
/// names with them (`pos.x`, `cells[2]`).
pub(super) fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Canonical param names: two or more parts joined by dots, like
/// `eps.bus_voltage`. A part is lowercase letters, digits and `_`, starting
/// with a letter or digit (`eps.3v3_current`).
pub(super) fn is_canonical_name(name: &str) -> bool {
    let mut parts = 0;
    for part in name.split('.') {
        let mut chars = part.chars();
        let first = chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        if !first || !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            return false;
        }
        parts += 1;
    }
    parts >= 2
}

/// The one spelling MDL accepts for an instant: `YYYY-MM-DDTHH:MM:SS`,
/// optionally `.` and 1 to 9 fraction digits, then `Z`. Uppercase `T` and
/// `Z`, UTC only, seconds 00 to 59, years 0001 to 9999. RFC 3339 allows
/// many spellings of one instant (offsets, lowercase, leap seconds); a
/// single spelling keeps one instant one digest, and every language can
/// check it the same way.
pub(super) fn is_strict_instant(text: &str) -> bool {
    let b = text.as_bytes();
    if b.len() < 20 || b[b.len() - 1] != b'Z' {
        return false;
    }
    let digits = |range: std::ops::Range<usize>| -> Option<u32> {
        let part = b.get(range)?;
        if part.iter().all(u8::is_ascii_digit) {
            std::str::from_utf8(part).ok()?.parse().ok()
        } else {
            None
        }
    };
    let separators =
        b[4] == b'-' && b[7] == b'-' && b[10] == b'T' && b[13] == b':' && b[16] == b':';
    let fraction = &b[19..b.len() - 1];
    let fraction_ok = fraction.is_empty()
        || (fraction[0] == b'.'
            && (2..=10).contains(&fraction.len())
            && fraction[1..].iter().all(u8::is_ascii_digit));
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        digits(0..4),
        digits(5..7),
        digits(8..10),
        digits(11..13),
        digits(14..16),
        digits(17..19),
    ) else {
        return false;
    };
    separators
        && fraction_ok
        && year >= 1
        && hour <= 23
        && minute <= 59
        && second <= 59
        && i32::try_from(year)
            .ok()
            .and_then(|y| chrono::NaiveDate::from_ymd_opt(y, month, day))
            .is_some()
}

/// The integer a scalar holds: a JSON number, or a decimal string (how
/// integers beyond ±(2^53 - 1) are written). `None` for other strings.
pub(super) fn integer_value(scalar: &Scalar) -> Option<i128> {
    match scalar {
        Scalar::Int(n) => Some(i128::from(*n)),
        Scalar::Str(text) => {
            let digits = text.strip_prefix('-').unwrap_or(text);
            let canonical = !digits.is_empty()
                && digits.len() <= 20
                && digits.bytes().all(|b| b.is_ascii_digit())
                && (digits == "0" || !digits.starts_with('0'))
                && text != "-0";
            if canonical { text.parse().ok() } else { None }
        }
    }
}

/// An integer that a JSON number holds exactly must be written as one:
/// `"100"` and `100` would otherwise be two spellings of one value.
pub(super) fn check_integer_spelling(
    scalar: &Scalar,
    what: &str,
    place: &Place,
    report: &mut Report,
) {
    if let (Scalar::Str(text), Some(n)) = (scalar, integer_value(scalar))
        && n.unsigned_abs() <= MAX_SAFE_INTEGER as u128
    {
        report.error(
            Code::IntegerAsString,
            place,
            format!("{what} {text:?} fits a JSON number; write it as {n}"),
        );
    }
}

/// RFC 6901: empty (the whole document), or `/`-separated tokens where `~`
/// appears only as `~0` or `~1`.
pub(super) fn is_json_pointer(pointer: &str) -> bool {
    if pointer.is_empty() {
        return true;
    }
    if !pointer.starts_with('/') {
        return false;
    }
    let mut chars = pointer.chars();
    while let Some(c) = chars.next() {
        if c == '~' && !matches!(chars.next(), Some('0' | '1')) {
            return false;
        }
    }
    true
}

pub(super) fn check_pointer(pointer: &str, place: &Place, report: &mut Report) {
    if !is_json_pointer(pointer) {
        report.error(
            Code::InvalidPointer,
            place,
            format!(
                "{pointer:?} is not a JSON Pointer (RFC 6901): it starts with \"/\", and \"~\" \
                 is written \"~0\" and \"/\" inside a key \"~1\""
            ),
        );
    }
}

/// Integers in definitions must survive every JSON implementation exactly.
pub(super) fn check_scalar(scalar: &Scalar, what: &str, place: &Place, report: &mut Report) {
    if let Scalar::Int(n) = scalar
        && n.unsigned_abs() > MAX_SAFE_INTEGER as u64
    {
        report.error(
            Code::UnsafeInteger,
            place,
            format!(
                "{what} {n} is outside ±(2^53 - 1), so some JSON readers would change it; write \
                 it as the string \"{n}\""
            ),
        );
    }
}

pub(super) fn check_int(n: i64, what: &str, place: &Place, report: &mut Report) {
    check_scalar(&Scalar::Int(n), what, place, report);
}

pub(super) fn check_finite(n: f64, what: &str, place: &Place, report: &mut Report) {
    if !n.is_finite() {
        report.error(
            Code::NonFinite,
            place,
            format!("{what} must be a finite number, not {n}"),
        );
    }
}

pub(super) fn check_not_empty(text: &str, what: &str, place: &Place, report: &mut Report) {
    if text.is_empty() {
        report.error(Code::Empty, place, format!("{what} must not be empty"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(is_token("vendor_x_v2"));
        for bad in ["", "Vendor", "2fast", "a-b", "a.b", "a b"] {
            assert!(!is_token(bad), "{bad:?}");
        }
        assert!(is_identifier("EPS_HK") && is_identifier("_raw") && is_identifier("q_w"));
        for bad in ["", "9lives", "pos.x", "cells[2]", "a-b"] {
            assert!(!is_identifier(bad), "{bad:?}");
        }
        assert!(is_canonical_name("eps.bus_voltage") && is_canonical_name("a.b.c"));
        for bad in [
            "eps",
            "Eps.Mode",
            "eps.",
            ".eps",
            "eps..mode",
            "eps.bus-voltage",
        ] {
            assert!(!is_canonical_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn digits_may_start_canonical_name_parts() {
        assert!(is_canonical_name("eps.3v3_current") && is_canonical_name("3v3.rail"));
    }

    #[test]
    fn instants_have_one_spelling() {
        for good in [
            "2000-01-01T00:00:00Z",
            "2026-11-05T23:59:59.5Z",
            "2024-02-29T12:00:00.123456789Z",
            "0001-01-01T00:00:00Z",
        ] {
            assert!(is_strict_instant(good), "{good}");
        }
        for bad in [
            "2000-01-01 00:00:00Z",
            "2000-01-01t00:00:00z",
            "2000-01-01T23:59:60Z",
            "2000-01-01T00:00:00.1234567891Z",
            "0000-01-01T00:00:00Z",
            "2000-01-01T00:00:00+00:00",
            "2023-02-29T00:00:00Z",
            "2000-01-01T00:00:00.Z",
            "2000-01-01",
            "2000-13-01T00:00:00Z",
        ] {
            assert!(!is_strict_instant(bad), "{bad}");
        }
    }

    #[test]
    fn integer_values() {
        assert_eq!(integer_value(&Scalar::Int(-7)), Some(-7));
        assert_eq!(
            integer_value(&Scalar::Str("18446744073709551615".into())),
            Some(18446744073709551615)
        );
        for bad in [
            "",
            "01",
            "-0",
            "+1",
            "1.0",
            "1e3",
            "x",
            "123456789012345678901",
        ] {
            assert_eq!(integer_value(&Scalar::Str(bad.into())), None, "{bad}");
        }
    }

    #[test]
    fn json_pointers() {
        for good in ["", "/", "/v", "/v/0", "/a~0b", "/a~1b", "/~01"] {
            assert!(is_json_pointer(good), "{good:?}");
        }
        for bad in ["v", "#/v", "/a~", "/a~2", "/~x"] {
            assert!(!is_json_pointer(bad), "{bad:?}");
        }
    }
}
