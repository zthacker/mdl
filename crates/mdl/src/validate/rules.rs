//! Naming and number rules shared by the checks.

use crate::diagnostics::{Code, Place, Report};
use crate::ir::Scalar;
use crate::units::Unit;

/// The largest integer every JSON implementation reads exactly.
pub(super) const MAX_SAFE_INTEGER: i64 = (1 << 53) - 1;

/// Family, set and metadata names: lowercase letters, digits and `_`,
/// starting with a letter. They end up in subjects, topics and file names.
pub(crate) fn is_token(name: &str) -> bool {
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

/// The integer a scalar holds ([`Scalar::as_integer`]).
pub(crate) fn integer_value(scalar: &Scalar) -> Option<i128> {
    scalar.as_integer()
}

pub(crate) use crate::ir::integer_text;

/// An integer that a JSON number holds exactly must be written as one:
/// `"100"` and `100` would otherwise be two spellings of one value.
pub(crate) fn check_integer_spelling(
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
            format!(
                "{what} {text:?} is an integer within ±(2^53 - 1) written as a string; write \
                 it as {n}"
            ),
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

/// A JSON Pointer's reference tokens, unescaped (`~1` is `/`, then `~0` is
/// `~`), or `None` if it isn't one.
pub(crate) fn pointer_tokens(pointer: &str) -> Option<Vec<String>> {
    if !is_json_pointer(pointer) {
        return None;
    }
    if pointer.is_empty() {
        return Some(Vec::new());
    }
    Some(
        pointer[1..]
            .split('/')
            .map(|token| token.replace("~1", "/").replace("~0", "~"))
            .collect(),
    )
}

/// A pointer to a member of the object it's read in: the empty pointer is
/// that whole object, and the document, a struct's value and a record
/// element are objects (`decoding.md` §5.3), so `what` couldn't be read
/// there.
pub(super) fn check_member_pointer(pointer: &str, what: &str, place: &Place, report: &mut Report) {
    if pointer.is_empty() {
        report.error(
            Code::InvalidPointer,
            place,
            format!(
                "{what} can't be at the empty pointer, which is the whole JSON object it's read \
                 in; point it at a member, like \"/value\""
            ),
        );
    } else {
        check_pointer(pointer, place, report);
    }
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
pub(crate) fn check_scalar(scalar: &Scalar, what: &str, place: &Place, report: &mut Report) {
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

pub(crate) fn check_not_empty(text: &str, what: &str, place: &Place, report: &mut Report) {
    if text.is_empty() {
        report.error(Code::Empty, place, format!("{what} must not be empty"));
    }
}

/// A unit is one MDL knows (`spec/units.md`); an empty one is left out
/// instead.
pub(crate) fn check_unit_code(unit: &str, place: &Place, report: &mut Report) {
    if unit.is_empty() {
        check_not_empty(unit, "unit", place, report);
        return;
    }
    if let Err(error) = Unit::parse(unit) {
        report.error(Code::UnknownUnit, place, error.to_string());
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
