//! The text an encoder writes for a JSON command (`spec/encoding.md` §5),
//! on this crate's own reading of RFC 8785: no whitespace, an object's
//! members in order of their names' UTF-16 code units, strings escaped as
//! it requires, doubles as ECMAScript writes them; and integers in exact
//! decimal digits whatever their size, MDL's one difference.

use std::cmp::Ordering;

/// A JSON value, as a command payload holds it, or as a fault planted in
/// one makes it.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Canon {
    Null,
    /// An integer field's value.
    Int(i128),
    /// A float field's value, finite.
    Float(f64),
    Bool(bool),
    Str(String),
    /// Text written exactly so: a number no field's value is written as
    /// (`1e20`), or what isn't JSON at all.
    Literal(String),
    Array(Vec<Canon>),
    /// Members as they were put; written in UTF-16 order.
    Object(Vec<(String, Canon)>),
}

/// A step into a document: an object's member, or an array's element.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Hop {
    Member(String),
    Index(usize),
}

impl Canon {
    /// Puts `value` at `tokens` inside this object, making objects along
    /// the way. Validation guarantees no two pointers of one object are
    /// equal, or one leads into another, so every member put is new.
    pub(super) fn put(&mut self, tokens: &[String], value: Canon) {
        let (Canon::Object(members), Some((first, rest))) = (self, tokens.split_first()) else {
            return;
        };
        if rest.is_empty() {
            members.push((first.clone(), value));
            return;
        }
        let at = match members.iter().position(|(name, _)| name == first) {
            Some(at) => at,
            None => {
                members.push((first.clone(), Canon::Object(Vec::new())));
                members.len() - 1
            }
        };
        members[at].1.put(rest, value);
    }

    /// The value at `hops`, if there's one.
    pub(super) fn at(&self, hops: &[Hop]) -> Option<&Canon> {
        let Some((hop, rest)) = hops.split_first() else {
            return Some(self);
        };
        let next = match (hop, self) {
            (Hop::Member(name), Canon::Object(members)) => members
                .iter()
                .find(|(member, _)| member == name)
                .map(|(_, value)| value)?,
            (Hop::Index(index), Canon::Array(items)) => items.get(*index)?,
            _ => return None,
        };
        next.at(rest)
    }

    /// The value at `hops`, if there's one, to change.
    pub(super) fn at_mut(&mut self, hops: &[Hop]) -> Option<&mut Canon> {
        let Some((hop, rest)) = hops.split_first() else {
            return Some(self);
        };
        let next = match (hop, self) {
            (Hop::Member(name), Canon::Object(members)) => members
                .iter_mut()
                .find(|(member, _)| member == name)
                .map(|(_, value)| value)?,
            (Hop::Index(index), Canon::Array(items)) => items.get_mut(*index)?,
            _ => return None,
        };
        next.at_mut(rest)
    }

    /// Takes the member at `hops` out of its object; whether it was there.
    pub(super) fn remove(&mut self, hops: &[Hop]) -> bool {
        let Some((Hop::Member(name), parent)) = hops.split_last() else {
            return false;
        };
        match self.at_mut(parent) {
            Some(Canon::Object(members)) => {
                let before = members.len();
                members.retain(|(member, _)| member != name);
                members.len() < before
            }
            _ => false,
        }
    }
}

/// The value as JSON text.
pub(super) fn write(value: &Canon) -> String {
    let mut out = String::new();
    write_into(&mut out, value);
    out
}

fn write_into(out: &mut String, value: &Canon) {
    match value {
        Canon::Null => out.push_str("null"),
        Canon::Literal(text) => out.push_str(text),
        Canon::Int(n) => out.push_str(&n.to_string()),
        Canon::Float(x) => out.push_str(&ecmascript(*x)),
        Canon::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Canon::Str(text) => write_string(out, text),
        Canon::Array(items) => {
            out.push('[');
            for (k, item) in items.iter().enumerate() {
                if k > 0 {
                    out.push(',');
                }
                write_into(out, item);
            }
            out.push(']');
        }
        Canon::Object(members) => {
            let mut sorted: Vec<&(String, Canon)> = members.iter().collect();
            sorted.sort_by(|(a, _), (b, _)| utf16_order(a, b));
            out.push('{');
            for (k, (name, member)) in sorted.into_iter().enumerate() {
                if k > 0 {
                    out.push(',');
                }
                write_string(out, name);
                out.push(':');
                write_into(out, member);
            }
            out.push('}');
        }
    }
}

/// How RFC 8785 orders member names: by their UTF-16 code units, so a
/// character past the BMP (a surrogate pair, from 0xD800) comes before
/// one from U+E000 to U+FFFF, the other way round from code points.
pub(super) fn utf16_order(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// A string as RFC 8785 writes it: `"` and `\` escaped, the controls with
/// a short escape as one (`\b \t \n \f \r`), the other controls as `\u`
/// and four lowercase hex digits, and every other character as it is.
fn write_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A finite double as ECMAScript's `Number.prototype.toString` writes it
/// (ECMA-262, Number::toString, with its Note 2): the digits [`shortest`]
/// gives, as an integer below 10^21, a decimal down to 10^-6, and with an
/// exponent past those; 0 for either zero.
pub(super) fn ecmascript(x: f64) -> String {
    if x == 0.0 {
        return "0".to_string();
    }
    if x < 0.0 {
        return format!("-{}", ecmascript(-x));
    }
    // The digits `s`, `k` of them, and `n` with x = 0.s × 10^n.
    let (digits, exponent) = shortest(x);
    let k = digits.len() as i32;
    let n = exponent + 1;
    if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        let (whole, fraction) = digits.split_at(n as usize);
        format!("{whole}.{fraction}")
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let (first, rest) = digits.split_at(1);
        let point = if rest.is_empty() { "" } else { "." };
        let sign = if n > 0 { '+' } else { '-' };
        format!("{first}{point}{rest}e{sign}{}", (n - 1).abs())
    }
}

/// The digits ECMAScript writes a double above 0 with, and the power of
/// ten of the first: the fewest that read back as it; of those, the one
/// nearest it; and of two equally near, the even one (ECMA-262's Note 2 to
/// Number::toString, which RFC 8785 requires; step 5 alone leaves the last
/// digit open). Rust's shortest round-trip digits are the fewest and the
/// nearest, but it rounds such a tie up, as with 1214.19732666015625,
/// which ECMAScript writes 1214.1973266601562. A tie shows in the double's
/// exact decimal expansion, which 800 places hold (the longest has 767
/// significant digits), as a 5 and then zeros right after the digits.
fn shortest(x: f64) -> (String, i32) {
    let split = |text: &str| -> (String, i32) {
        let (mantissa, exponent) = text.split_once('e').unwrap_or((text, "0"));
        let digits = mantissa.chars().filter(char::is_ascii_digit).collect();
        (digits, exponent.parse().unwrap_or(0))
    };
    let (digits, exponent) = split(&format!("{x:e}"));
    let (exact, exact_exponent) = split(&format!("{x:.800e}"));
    // The exponents differ only when the digits round up to a power of
    // ten, whose one digit is then the only candidate of that length: no
    // tie to break.
    if exact_exponent != exponent || exact.len() <= digits.len() {
        return (digits, exponent);
    }
    let (head, tail) = exact.split_at(digits.len());
    let tie = tail.starts_with('5') && tail[1..].bytes().all(|b| b == b'0');
    if !tie {
        return (digits, exponent);
    }
    // `head` and one more in its last place are equally near; the even one,
    // if it reads back as `x` (below a power of two the gap halves). One
    // more that carries into another digit (`999` to `1000`) isn't of the
    // length, and then only `head` can be the digits.
    let up = increment(head);
    [head.to_string(), up]
        .into_iter()
        .filter(|candidate| candidate.len() == digits.len())
        .find(|candidate| {
            let even = candidate
                .bytes()
                .last()
                .is_some_and(|b| (b - b'0').is_multiple_of(2));
            let read = format!("{}.{}e{exponent}", &candidate[..1], &candidate[1..]);
            even && read.parse::<f64>() == Ok(x)
        })
        .map_or((digits, exponent), |even| (even, exponent))
}

/// A string of decimal digits plus one in its last place.
fn increment(digits: &str) -> String {
    let mut bytes = digits.as_bytes().to_vec();
    for byte in bytes.iter_mut().rev() {
        if *byte == b'9' {
            *byte = b'0';
        } else {
            *byte += 1;
            return String::from_utf8(bytes).unwrap_or_default();
        }
    }
    format!("1{}", String::from_utf8(bytes).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{Canon, ecmascript, write};

    /// Every vector of `conformance/canonical/numbers.json`, which a real
    /// ECMAScript engine wrote (`spec/canonical-json.md` §4): a double's bits
    /// and its text, ties among them (2^-25 is written with the even digits,
    /// 2^-24 with the odd, as the even ones don't read back as it).
    #[test]
    fn doubles_are_written_as_an_ecmascript_engine_writes_them() {
        let vectors: Value = serde_json::from_str(include_str!(
            "../../../../../conformance/canonical/numbers.json"
        ))
        .unwrap();
        let vectors = vectors["vectors"].as_array().unwrap();
        assert!(vectors.len() > 2000);
        for vector in vectors {
            let bits = u64::from_str_radix(vector[0].as_str().unwrap(), 16).unwrap();
            let x = f64::from_bits(bits);
            assert_eq!(ecmascript(x), vector[1].as_str().unwrap(), "{bits:016x}");
        }
    }

    /// ECMAScript's own examples and the edges of each form.
    #[test]
    fn doubles_are_written_as_ecmascript_writes_them() {
        for (x, text) in [
            (0.0, "0"),
            (-0.0, "0"),
            (1.5, "1.5"),
            (-1.5, "-1.5"),
            (3.0, "3"),
            (123.0, "123"),
            (0.1, "0.1"),
            (1.0 / 3.0, "0.3333333333333333"),
            (1e20, "100000000000000000000"),
            (1e21, "1e+21"),
            (1.5e21, "1.5e+21"),
            (123456789012345680000.0, "123456789012345680000"),
            (1e-6, "0.000001"),
            (1.5e-6, "0.0000015"),
            (1e-7, "1e-7"),
            (1.5e-7, "1.5e-7"),
            (f64::MAX, "1.7976931348623157e+308"),
            (5e-324, "5e-324"),
            (f64::MIN_POSITIVE, "2.2250738585072014e-308"),
            (-1e-7, "-1e-7"),
            // Two strings of the fewest digits equally near: the even one.
            // 19893409 / 2^14 is 1214.19732666015625; 2^50 + 0.25 and
            // 2^50 + 0.75 are written with one decimal.
            (19_893_409.0 / 16_384.0, "1214.1973266601562"),
            (-19_893_409.0 / 16_384.0, "-1214.1973266601562"),
            (2_f64.powi(50) + 0.25, "1125899906842624.2"),
            (-(2_f64.powi(50) + 0.75), "-1125899906842624.8"),
        ] {
            assert_eq!(ecmascript(x), text, "{x:e}");
        }
    }

    /// Members in UTF-16 order, strings escaped as RFC 8785 says, integers
    /// past 2^53 exact.
    #[test]
    fn documents_are_written_canonically() {
        let mut document = Canon::Object(Vec::new());
        for (name, value) in [
            ("\u{ff5e}", Canon::Int(1)),
            ("\u{1f600}", Canon::Int(2)),
            (
                "b",
                Canon::Str("\"\\/\u{8}\t\n\u{c}\r\u{1}\u{1f}\u{7f}\u{2028}é".into()),
            ),
            ("a", Canon::Int(18_446_744_073_709_551_615)),
            ("c", Canon::Float(-0.0)),
        ] {
            document.put(&[name.to_string()], value);
        }
        document.put(&["d".into(), "f".into()], Canon::Bool(true));
        document.put(
            &["d".into(), "e".into()],
            Canon::Array(vec![Canon::Bool(false)]),
        );
        assert_eq!(
            write(&document),
            "{\"a\":18446744073709551615,\"b\":\"\\\"\\\\/\\b\\t\\n\\f\\r\\u0001\\u001f\u{7f}\u{2028}é\",\
             \"c\":0,\"d\":{\"e\":[false],\"f\":true},\"\u{1f600}\":2,\"\u{ff5e}\":1}"
        );
    }
}
