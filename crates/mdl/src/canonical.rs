//! Canonical JSON (RFC 8785, the JSON Canonicalization Scheme).
//!
//! Every MDL artifact is hashed over its canonical form, so any
//! implementation, in any language, that parses an artifact and
//! re-canonicalizes it gets the same bytes and the same hash. The rules:
//!
//! - no whitespace;
//! - object members sorted by their keys' UTF-16 code units (not UTF-8
//!   bytes: the two orders differ above U+FFFF);
//! - strings escaped minimally: `"` and `\`, the five short escapes
//!   (`\b \t \n \f \r`), `\u00xx` (lowercase) for other control characters,
//!   everything else written as is;
//! - numbers written the way ECMAScript's `Number.prototype.toString` writes
//!   them (`1.0` is `1`, `1e30` is `1e+30`, `-0` is `0`).
//!
//! MDL adds two input rules on top of RFC 8785, both normative for every
//! implementation (`spec/canonical-json.md`):
//!
//! - **No duplicate keys** (I-JSON, RFC 7493). Most JSON parsers silently
//!   keep the last duplicate and some keep both, so a document with
//!   duplicates has no single canonical form. Parse artifacts with
//!   [`parse_strict`], which refuses them.
//! - **Integers must already be their own canonical text.** RFC 8785 treats
//!   every number as a double, so `9007199254740993` would be written
//!   `9007199254740992` and two different documents would share one hash.
//!   An integer literal in the 64-bit range (−2^63 to 2^64 − 1) is refused
//!   unless its nearest double prints exactly its digits. Integer literals
//!   beyond that range are read as doubles, as every double-only parser
//!   reads them.
//!
//! IR v2 validation is stricter still for numbers the IR means as integers
//! (identify values, sentinels, states): they must be within ±(2^53 − 1),
//! with wider values written as strings (see `v2::ir::Scalar`).

use std::collections::BTreeSet;
use std::fmt::{self, Write};

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::error::Category;
use serde_json::{Map, Number, Value};
use thiserror::Error;

/// Above this, not every integer has an exact double: 2^53 − 1.
pub const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CanonicalError {
    #[error(
        "integer {value} at {path} changes in canonical JSON (as a double it's written \
         {canonical}); write it as a string"
    )]
    InexactInteger {
        path: String,
        value: String,
        canonical: String,
    },
    #[error("duplicate key: {detail}")]
    DuplicateKey { detail: String },
    #[error("invalid JSON: {detail}")]
    InvalidJson { detail: String },
    /// Only reachable if serde_json's `arbitrary_precision` feature is on
    /// somewhere in the dependency graph (features unify across crates),
    /// letting a number exceed what an f64 can hold.
    #[error("number {value} at {path} is outside the range of an IEEE 754 double")]
    OutOfRange { path: String, value: String },
}

impl CanonicalError {
    /// The code conformance vectors use (`spec/canonical-json.md`, §1).
    pub fn code(&self) -> &'static str {
        match self {
            CanonicalError::InexactInteger { .. } => "inexact_integer",
            CanonicalError::DuplicateKey { .. } => "duplicate_key",
            CanonicalError::InvalidJson { .. } | CanonicalError::OutOfRange { .. } => {
                "invalid_json"
            }
        }
    }
}

/// Parse an artifact's bytes, refusing input that has no single canonical
/// form: invalid UTF-8, unpaired surrogates, numbers too large for a double,
/// a leading byte order mark, an object with a duplicate key (at any depth),
/// or an integer that isn't its own canonical text. These are input rules
/// 1 to 7 of `spec/canonical-json.md`. serde_json on its own silently keeps
/// the last of two duplicates.
///
/// Two passes: `NoDuplicates` only checks keys, then serde_json builds
/// the value. Building the value ourselves would be one pass, but it would
/// break if serde_json's `arbitrary_precision` feature were enabled anywhere
/// in a downstream build (features unify), because that feature smuggles
/// numbers through the deserializer as private one-key maps that only
/// serde_json's own `Value` understands. Artifacts are parsed once per load,
/// so the second pass is cheap.
///
/// Nesting is capped at serde_json's default depth of 128 in both passes,
/// so hostile input can't exhaust the stack.
pub fn parse_strict(bytes: &[u8]) -> Result<Value, CanonicalError> {
    serde_json::from_slice::<NoDuplicates>(bytes).map_err(|err| {
        let detail = err.to_string();
        // The checker's only error is a duplicate key, and serde_json files
        // errors raised by visitors under `Category::Data`. Everything else
        // (syntax, UTF-8, surrogates, number range, depth) is `Syntax`/`Eof`.
        match err.classify() {
            Category::Data => CanonicalError::DuplicateKey { detail },
            _ => CanonicalError::InvalidJson { detail },
        }
    })?;
    let value = serde_json::from_slice(bytes).map_err(|err| CanonicalError::InvalidJson {
        detail: err.to_string(),
    })?;
    // Rule 7 (integers must be their own canonical text) and rule 5
    // (numbers must be finite doubles). Under default features serde_json
    // already refuses `1e400`; with `arbitrary_precision` it doesn't, and
    // this check catches it.
    check_numbers(&value, &mut String::new()).map_err(|err| match err {
        CanonicalError::OutOfRange { .. } => CanonicalError::InvalidJson {
            detail: err.to_string(),
        },
        other => other,
    })?;
    Ok(value)
}

/// Walks a document and fails on the first duplicate object key. Builds
/// nothing.
struct NoDuplicates;

impl<'de> Deserialize<'de> for NoDuplicates {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(NoDuplicatesVisitor)
    }
}

struct NoDuplicatesVisitor;

impl<'de> Visitor<'de> for NoDuplicatesVisitor {
    type Value = NoDuplicates;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_unit<E>(self) -> Result<NoDuplicates, E> {
        Ok(NoDuplicates)
    }

    fn visit_bool<E>(self, _: bool) -> Result<NoDuplicates, E> {
        Ok(NoDuplicates)
    }

    fn visit_i64<E>(self, _: i64) -> Result<NoDuplicates, E> {
        Ok(NoDuplicates)
    }

    fn visit_u64<E>(self, _: u64) -> Result<NoDuplicates, E> {
        Ok(NoDuplicates)
    }

    fn visit_f64<E>(self, _: f64) -> Result<NoDuplicates, E> {
        Ok(NoDuplicates)
    }

    fn visit_str<E>(self, _: &str) -> Result<NoDuplicates, E> {
        Ok(NoDuplicates)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<NoDuplicates, A::Error> {
        while seq.next_element::<NoDuplicates>()?.is_some() {}
        Ok(NoDuplicates)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<NoDuplicates, A::Error> {
        let mut keys = BTreeSet::new();
        while let Some(key) = access.next_key::<String>()? {
            if keys.contains(&key) {
                return Err(de::Error::custom(format!(
                    "{key:?} appears twice in one object"
                )));
            }
            access.next_value::<NoDuplicates>()?;
            keys.insert(key);
        }
        Ok(NoDuplicates)
    }
}

/// The canonical JSON text of `value`.
pub fn to_canonical_string(value: &Value) -> Result<String, CanonicalError> {
    let mut out = String::new();
    write_value(&mut out, value, &mut String::new())?;
    Ok(out)
}

/// `path` is the JSON Pointer of `value`, for error messages only.
fn write_value(out: &mut String, value: &Value, path: &mut String) -> Result<(), CanonicalError> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => write_number(out, number, path)?,
        Value::String(text) => write_string(out, text),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                with_segment(path, &index.to_string(), |path| {
                    write_value(out, item, path)
                })?;
            }
            out.push(']');
        }
        Value::Object(members) => write_object(out, members, path)?,
    }
    Ok(())
}

fn write_object(
    out: &mut String,
    members: &Map<String, Value>,
    path: &mut String,
) -> Result<(), CanonicalError> {
    let mut sorted: Vec<(&String, &Value)> = members.iter().collect();
    sorted.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
    out.push('{');
    for (index, (key, value)) in sorted.into_iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        write_string(out, key);
        out.push(':');
        with_segment(path, key, |path| write_value(out, value, path))?;
    }
    out.push('}');
    Ok(())
}

fn write_number(out: &mut String, number: &Number, path: &str) -> Result<(), CanonicalError> {
    out.push_str(&canonical_number(number, path)?);
    Ok(())
}

/// A number's canonical text, or why it has none. Integers are canonical
/// only if their nearest double is written with exactly their digits.
fn canonical_number(number: &Number, path: &str) -> Result<String, CanonicalError> {
    let integer = match (number.as_u64(), number.as_i64()) {
        (Some(n), _) => Some((n.to_string(), n as f64)),
        (None, Some(n)) => Some((n.to_string(), n as f64)),
        (None, None) => None,
    };
    match integer {
        Some((digits, double)) => {
            let canonical = format_double(double);
            if canonical == digits {
                Ok(digits)
            } else {
                Err(CanonicalError::InexactInteger {
                    path: display_path(path),
                    value: digits,
                    canonical,
                })
            }
        }
        None => match number.as_f64() {
            Some(n) if n.is_finite() => Ok(format_double(n)),
            _ => Err(CanonicalError::OutOfRange {
                path: display_path(path),
                value: number.to_string(),
            }),
        },
    }
}

/// Apply the number rules to every number in `value`. `parse_strict` uses
/// this so a reader never builds typed values from input the spec rejects.
fn check_numbers(value: &Value, path: &mut String) -> Result<(), CanonicalError> {
    match value {
        Value::Number(number) => canonical_number(number, path).map(|_| ()),
        Value::Array(items) => items.iter().enumerate().try_for_each(|(index, item)| {
            with_segment(path, &index.to_string(), |path| check_numbers(item, path))
        }),
        Value::Object(members) => members
            .iter()
            .try_for_each(|(key, item)| with_segment(path, key, |path| check_numbers(item, path))),
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
    }
}

/// ECMAScript `Number.prototype.toString` for a finite double. serde_json
/// never holds NaN or infinity, so every double here is finite.
fn format_double(n: f64) -> String {
    if n == 0.0 {
        // Covers -0, which ECMAScript writes as "0".
        return "0".to_string();
    }
    ryu_js::Buffer::new().format_finite(n).to_string()
}

fn write_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{0C}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if c < '\u{20}' => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Run `f` with `segment` appended to the pointer `path`, then restore it.
fn with_segment<T>(path: &mut String, segment: &str, f: impl FnOnce(&mut String) -> T) -> T {
    let len = path.len();
    path.push('/');
    // JSON Pointer escaping (RFC 6901): "~" is "~0", "/" is "~1".
    path.push_str(&segment.replace('~', "~0").replace('/', "~1"));
    let result = f(path);
    path.truncate(len);
    result
}

fn display_path(path: &str) -> String {
    if path.is_empty() {
        "the top level".to_string()
    } else {
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn canonical(text: &str) -> String {
        to_canonical_string(&serde_json::from_str(text).unwrap()).unwrap()
    }

    /// RFC 8785 §3.2.2's example: whitespace, number forms and escapes.
    #[test]
    fn rfc_8785_example() {
        let input = r#"{
            "numbers": [333333333.33333329, 1E30, 4.50, 2e-3, 0.000000000000000000000000001],
            "string": "\u20ac$\u000F\u000aA'\u0042\u0022\u005c\\\"\/",
            "literals": [null, true, false]
        }"#;
        assert_eq!(
            canonical(input),
            r#"{"literals":[null,true,false],"numbers":[333333333.3333333,1e+30,4.5,0.002,1e-27],"string":"€$\u000f\nA'B\"\\\\\"/"}"#
        );
    }

    /// RFC 8785 §3.2.3's sorting example. In UTF-8 byte order U+FB33 would
    /// sort before the emoji (U+1F600); in UTF-16 code units the emoji's
    /// surrogate pair (0xD83D ...) comes first.
    #[test]
    fn keys_sort_by_utf16_code_units() {
        let input = r#"{
            "€": "Euro Sign",
            "\r": "Carriage Return",
            "דּ": "Hebrew Letter Dalet With Dagesh",
            "1": "One",
            "😀": "Emoji: Grinning Face",
            "\u0080": "Control",
            "ö": "Latin Small Letter O With Diaeresis"
        }"#;
        let text = canonical(input);
        let positions: Vec<usize> = [
            "Carriage Return",
            "One",
            "Control",
            "Latin Small Letter O With Diaeresis",
            "Euro Sign",
            "Emoji: Grinning Face",
            "Hebrew Letter Dalet With Dagesh",
        ]
        .iter()
        .map(|name| text.find(name).unwrap())
        .collect();
        // serde_json's own map sorts by UTF-8, so check the order in the text.
        assert!(positions.windows(2).all(|w| w[0] < w[1]), "{text}");
    }

    /// RFC 8785 Appendix B: IEEE 754 bit patterns and their canonical form.
    #[test]
    fn rfc_8785_number_vectors() {
        let vectors: &[(u64, &str)] = &[
            (0x0000000000000000, "0"),
            (0x8000000000000000, "0"),
            (0x0000000000000001, "5e-324"),
            (0x8000000000000001, "-5e-324"),
            (0x7fefffffffffffff, "1.7976931348623157e+308"),
            (0xffefffffffffffff, "-1.7976931348623157e+308"),
            (0x4340000000000000, "9007199254740992"),
            (0xc340000000000000, "-9007199254740992"),
            (0x4430000000000000, "295147905179352830000"),
            (0x44b52d02c7e14af5, "9.999999999999997e+22"),
            (0x44b52d02c7e14af6, "1e+23"),
            (0x44b52d02c7e14af7, "1.0000000000000001e+23"),
            (0x444b1ae4d6e2ef4e, "999999999999999700000"),
            (0x444b1ae4d6e2ef4f, "999999999999999900000"),
            (0x444b1ae4d6e2ef50, "1e+21"),
            (0x3eb0c6f7a0b5ed8c, "9.999999999999997e-7"),
            (0x3eb0c6f7a0b5ed8d, "0.000001"),
            (0x41b3de4355555553, "333333333.3333332"),
            (0x41b3de4355555554, "333333333.33333325"),
            (0x41b3de4355555555, "333333333.3333333"),
            (0x41b3de4355555556, "333333333.3333334"),
            (0x41b3de4355555557, "333333333.33333343"),
            (0xbecbf647612f3696, "-0.0000033333333333333333"),
            (0x43143ff3c1cb0959, "1424953923781206.2"),
        ];
        for (bits, expected) in vectors {
            assert_eq!(
                format_double(f64::from_bits(*bits)),
                *expected,
                "{bits:#018x}"
            );
        }
    }

    #[test]
    fn integers_and_doubles() {
        assert_eq!(canonical("[1, 1.0, -0.0, 10.5, -3]"), "[1,1,0,10.5,-3]");
        let edge = json!([MAX_SAFE_INTEGER, -(MAX_SAFE_INTEGER as i64)]);
        assert_eq!(
            to_canonical_string(&edge).unwrap(),
            "[9007199254740991,-9007199254740991]"
        );
    }

    #[test]
    fn big_integers_pass_only_if_they_are_their_own_canonical_text() {
        // 2000-01-01 in ns: above 2^53, but its double prints the same digits.
        let exact = json!({"epoch_ns": 946_684_800_000_000_000_i64});
        assert_eq!(
            to_canonical_string(&exact).unwrap(),
            r#"{"epoch_ns":946684800000000000}"#
        );

        // 2^53 + 1 would hash like 2^53: refused.
        let value = json!({"time": {"epoch_ns": (1_i64 << 53) + 1}});
        assert_eq!(
            to_canonical_string(&value).unwrap_err().to_string(),
            "integer 9007199254740993 at /time/epoch_ns changes in canonical JSON (as a double \
             it's written 9007199254740992); write it as a string"
        );
        let value = json!([{"a/b": [u64::MAX]}]);
        let error = to_canonical_string(&value).unwrap_err();
        assert!(error.to_string().contains("at /0/a~1b/0"), "{error}");
        // Exact doubles can still fail: 2^63 is exact, but ECMAScript prints
        // its shortest round-trip digits, 9223372036854776000.
        assert!(to_canonical_string(&json!(i64::MIN)).is_err());
        assert!(to_canonical_string(&json!(i64::MAX)).is_err());
    }

    #[test]
    fn integer_text_beyond_64_bits_is_read_as_a_double() {
        // Pinned behavior (spec/canonical-json.md): outside the 64-bit range,
        // integer text is a double, exactly as Go and JavaScript read it.
        assert_eq!(canonical("18446744073709551617"), "18446744073709552000");
        assert_eq!(canonical("100000000000000000000000"), "1e+23");
        assert_eq!(canonical("-9223372036854775809"), "-9223372036854776000");
    }

    #[test]
    fn parse_strict_refuses_duplicate_keys_at_any_depth() {
        for text in [r#"{"a":1,"a":2}"#, r#"[{"x":{"k":1,"k":1}}]"#] {
            let error = parse_strict(text.as_bytes()).unwrap_err();
            assert!(
                matches!(error, CanonicalError::DuplicateKey { .. }),
                "{text}: {error}"
            );
        }
        let error = parse_strict(br#"{"a":1,"a":2}"#).unwrap_err().to_string();
        assert_eq!(
            error,
            "duplicate key: \"a\" appears twice in one object at line 1 column 10"
        );
        // Same key in sibling objects is fine.
        assert!(parse_strict(br#"[{"a":1},{"a":2}]"#).is_ok());
    }

    #[test]
    fn parse_strict_reports_bad_json_and_survives_deep_nesting() {
        let error = parse_strict(b"{ not json").unwrap_err();
        assert!(
            matches!(error, CanonicalError::InvalidJson { .. }),
            "{error}"
        );

        // Far past serde_json's depth limit: an error, not a stack overflow.
        let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
        let error = parse_strict(deep.as_bytes()).unwrap_err();
        assert!(error.to_string().contains("recursion limit"), "{error}");
    }

    #[test]
    fn parse_strict_rejects_input_with_no_single_canonical_form() {
        let backslash = char::from(92);
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("invalid UTF-8", b"\"\xff\"".to_vec()),
            ("overlong UTF-8", b"[\"\xc0\xaf\"]".to_vec()),
            ("raw surrogate bytes", b"\"\xed\xa0\x80\"".to_vec()),
            (
                "escaped lone surrogate",
                format!("\"{backslash}ud800\"").into_bytes(),
            ),
            ("number beyond a double", b"[1e400]".to_vec()),
            ("byte order mark", b"\xef\xbb\xbf{}".to_vec()),
        ];
        for (what, bytes) in cases {
            let error = parse_strict(&bytes).unwrap_err();
            assert!(
                matches!(error, CanonicalError::InvalidJson { .. }),
                "{what}: {error}"
            );
        }
    }

    #[test]
    fn noncharacters_are_accepted_and_written_as_is() {
        // A stated deviation from I-JSON (spec/canonical-json.md): every
        // parser passes noncharacters through unchanged, so they can't make
        // implementations disagree.
        let text = "[\"\u{FFFF}\u{FDD0}\u{1FFFE}\"]";
        let value = parse_strict(text.as_bytes()).unwrap();
        assert_eq!(to_canonical_string(&value).unwrap(), text);
    }

    #[test]
    fn parse_strict_enforces_the_integer_rule_itself() {
        // A reader that parses and then types values must be refused here,
        // not only when it later canonicalizes.
        for text in [
            "[9007199254740993]",
            "18446744073709551615",
            "-9223372036854775808",
        ] {
            let error = parse_strict(text.as_bytes()).unwrap_err();
            assert!(
                matches!(error, CanonicalError::InexactInteger { .. }),
                "{text}: {error}"
            );
        }
    }

    #[test]
    fn parse_strict_builds_the_same_values_as_serde_json() {
        let text = r#"{"n":[0,-0,1.5,9007199254740991,946684800000000000,-1e300],"s":"x","b":[true,null]}"#;
        let ours = parse_strict(text.as_bytes()).unwrap();
        let theirs: Value = serde_json::from_str(text).unwrap();
        assert_eq!(ours, theirs);
    }

    #[test]
    fn characters_outside_the_bmp_are_written_as_is() {
        let value = json!({"emoji": "\u{1F600}"});
        assert_eq!(
            to_canonical_string(&value).unwrap(),
            "{\"emoji\":\"\u{1F600}\"}"
        );
    }

    #[test]
    fn control_characters_use_lowercase_unicode_escapes() {
        let value = json!("\u{0001}\u{001f}\u{007f}\u{2028}");
        assert_eq!(
            to_canonical_string(&value).unwrap(),
            "\"\\u0001\\u001f\u{007f}\u{2028}\""
        );
    }

    mod properties {
        use super::*;
        use proptest::prelude::*;

        fn json_value() -> impl Strategy<Value = Value> {
            let leaf = prop_oneof![
                Just(Value::Null),
                any::<bool>().prop_map(Value::Bool),
                (-(MAX_SAFE_INTEGER as i64)..=(MAX_SAFE_INTEGER as i64)).prop_map(|n| json!(n)),
                any::<f64>()
                    .prop_filter("finite", |n| n.is_finite())
                    .prop_map(|n| json!(n)),
                ".*".prop_map(Value::String),
            ];
            leaf.prop_recursive(4, 64, 8, |inner| {
                prop_oneof![
                    prop::collection::vec(inner.clone(), 0..8).prop_map(Value::Array),
                    prop::collection::btree_map(".*", inner, 0..8)
                        .prop_map(|m| Value::Object(m.into_iter().collect())),
                ]
            })
        }

        /// Equal as JSON values, comparing numbers by their double value
        /// (serde_json alone calls integer `5` and float `5.0` different).
        fn same_json(a: &Value, b: &Value) -> bool {
            match (a, b) {
                (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
                (Value::Array(x), Value::Array(y)) => {
                    x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same_json(x, y))
                }
                (Value::Object(x), Value::Object(y)) => {
                    x.len() == y.len()
                        && x.iter()
                            .all(|(k, v)| y.get(k).is_some_and(|w| same_json(v, w)))
                }
                _ => a == b,
            }
        }

        proptest! {
            /// Canonical text parses back to the same value, and
            /// canonicalizing it again changes nothing.
            #[test]
            fn canonical_form_is_a_fixed_point(value in json_value()) {
                let once = to_canonical_string(&value).unwrap();
                let reparsed = parse_strict(once.as_bytes()).unwrap();
                prop_assert!(same_json(&reparsed, &value), "{} vs {:?}", once, value);
                prop_assert_eq!(to_canonical_string(&reparsed).unwrap(), once);
            }

            /// Any 64-bit integer is accepted exactly when its nearest double
            /// prints its digits, and then it's written as those digits.
            #[test]
            fn integers_follow_the_own_canonical_text_rule(n in any::<i64>(), m in any::<u64>()) {
                for (digits, double, value) in [
                    (n.to_string(), n as f64, json!(n)),
                    (m.to_string(), m as f64, json!(m)),
                ] {
                    match to_canonical_string(&value) {
                        Ok(text) => prop_assert_eq!(&text, &digits),
                        Err(error) => {
                            prop_assert!(format_double(double) != digits, "{}", error);
                            let is_inexact = matches!(error, CanonicalError::InexactInteger { .. });
                            prop_assert!(is_inexact);
                        }
                    }
                }
            }

            /// ECMAScript number text parses back to the same double.
            #[test]
            fn doubles_round_trip(n in any::<f64>().prop_filter("finite", |n| n.is_finite())) {
                let text = format_double(n);
                let back: f64 = text.parse().unwrap();
                prop_assert!(back == n || (back == 0.0 && n == 0.0), "{} -> {}", n, text);
            }
        }
    }
}
