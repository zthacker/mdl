//! JSON documents as the testkit writes them (`spec/decoding.md` §2.3):
//! a value tree, and a writer that spells each value any way JSON allows
//! while keeping what a reader takes it for. An integer keeps being one
//! (`42`, `42.0`, `4.2e1`, `420E-1`, each within ±(2^53 − 1)); a float
//! reads back to the same double; a string's characters come raw or
//! escaped, a character beyond the BMP as a surrogate pair; members come
//! in any order, with any whitespace between tokens.
//!
//! The choices come from [`Spell`], a small generator seeded once per
//! message, so the spellings needn't be drawn one by one as strategies.

/// A JSON value, before it's written.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::layout) enum Doc {
    Null,
    Bool(bool),
    /// An integer, in any spelling that keeps it one.
    Int(i128),
    /// A finite double, in any spelling that reads back as it.
    Float(f64),
    /// A number written exactly so.
    Literal(String),
    Str(String),
    Array(Vec<Doc>),
    /// Members in the order they're written.
    Object(Vec<(String, Doc)>),
}

/// The largest integer every spelling keeps exact: 2^53 − 1 (§2.3).
const SAFE: i128 = (1 << 53) - 1;

/// Where a document's spellings come from: SplitMix64, seeded once.
#[derive(Debug, Clone)]
pub(in crate::layout) struct Spell {
    state: u64,
}

impl Spell {
    pub(in crate::layout) fn new(seed: u64) -> Spell {
        Spell { state: seed }
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// One of `n` choices, 0 for none.
    pub(in crate::layout) fn pick(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next() % n as u64) as usize
    }

    /// Whether a one-in-`n` chance comes up.
    fn chance(&mut self, n: usize) -> bool {
        self.pick(n) == 0
    }

    /// `items` in an order this picks.
    pub(in crate::layout) fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, self.pick(i + 1));
        }
    }
}

/// The document as text.
pub(in crate::layout) fn write(doc: &Doc, spell: &mut Spell) -> String {
    let mut out = String::new();
    space(spell, &mut out);
    value(doc, spell, &mut out);
    space(spell, &mut out);
    out
}

/// Whitespace between tokens: mostly none.
fn space(spell: &mut Spell, out: &mut String) {
    const SPACES: [&str; 5] = [" ", "\n", "\t", "\r\n", "  "];
    if spell.chance(3) {
        out.push_str(SPACES[spell.pick(SPACES.len())]);
    }
}

fn value(doc: &Doc, spell: &mut Spell, out: &mut String) {
    match doc {
        Doc::Null => out.push_str("null"),
        Doc::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Doc::Int(n) => out.push_str(&integer(*n, spell)),
        Doc::Float(x) => out.push_str(&float(*x, spell)),
        Doc::Literal(text) => out.push_str(text),
        Doc::Str(text) => string(text, spell, out),
        Doc::Array(items) => {
            out.push('[');
            for (k, item) in items.iter().enumerate() {
                if k > 0 {
                    space(spell, out);
                    out.push(',');
                }
                space(spell, out);
                value(item, spell, out);
            }
            space(spell, out);
            out.push(']');
        }
        Doc::Object(members) => {
            out.push('{');
            for (k, (name, member)) in members.iter().enumerate() {
                if k > 0 {
                    space(spell, out);
                    out.push(',');
                }
                space(spell, out);
                string(name, spell, out);
                space(spell, out);
                out.push(':');
                space(spell, out);
                value(member, spell, out);
            }
            space(spell, out);
            out.push('}');
        }
    }
}

/// An integer as JSON: its digits, or within ±(2^53 − 1) also with a
/// fraction of zeros or an exponent, which keep it an integer (§2.3). 0
/// may be `-0`.
pub(in crate::layout) fn integer(n: i128, spell: &mut Spell) -> String {
    let plain = n.to_string();
    if n.abs() > SAFE {
        return plain;
    }
    let sign = if n < 0 { "-" } else { "" };
    let digits = n.unsigned_abs().to_string();
    let e = if spell.chance(2) { "e" } else { "E" };
    let plus = if spell.chance(2) { "+" } else { "" };
    match spell.pick(8) {
        0..=2 => plain,
        3 => format!("{plain}.0"),
        4 => format!("{plain}.{}", "0".repeat(1 + spell.pick(3))),
        // One digit before the point: 4.2e1, 4e3.
        5 => {
            let (first, rest) = digits.split_at(1);
            let rest = rest.trim_end_matches('0');
            let exponent = digits.len() - 1;
            if rest.is_empty() {
                format!("{sign}{first}{e}{plus}{exponent}")
            } else {
                format!("{sign}{first}.{rest}{e}{plus}{exponent}")
            }
        }
        _ if n == 0 => {
            const ZEROS: [&str; 4] = ["-0", "0e0", "-0.0", "0.0E+5"];
            ZEROS[spell.pick(ZEROS.len())].to_string()
        }
        // A zero too many and a negative exponent: 4200e-1.
        6 => format!("{sign}{digits}0{e}-1"),
        _ => plain,
    }
}

/// A finite double as JSON: Rust's shortest round-trip digits, plain or
/// with an exponent. Negative zero is `-0`, `-0e0` or `-0E0`, which a float
/// field reads as −0.0 (§5.3).
pub(in crate::layout) fn float(x: f64, spell: &mut Spell) -> String {
    match spell.pick(4) {
        0 | 1 => format!("{x}"),
        2 => format!("{x:e}"),
        _ => format!("{x:E}"),
    }
}

/// A string as JSON, each character raw or escaped: the ones JSON
/// requires escaped (quote, backslash, controls) as a short escape where
/// one exists or as `\u`, others raw, or as `\u` (a character beyond the
/// BMP as a surrogate pair), and `/` sometimes as `\/`.
pub(in crate::layout) fn string(text: &str, spell: &mut Spell, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        let short = match c {
            '"' => Some("\\\""),
            '\\' => Some("\\\\"),
            '\u{8}' => Some("\\b"),
            '\u{c}' => Some("\\f"),
            '\n' => Some("\\n"),
            '\r' => Some("\\r"),
            '\t' => Some("\\t"),
            '/' => Some("\\/"),
            _ => None,
        };
        let required = c == '"' || c == '\\' || u32::from(c) < 0x20;
        let escaped = required || spell.chance(4);
        if !escaped {
            out.push(c);
        } else if let Some(short) = short.filter(|_| !spell.chance(4)) {
            out.push_str(short);
        } else {
            unicode(c, spell, out);
        }
    }
    out.push('"');
}

/// A character as `\u` escapes: one, or a surrogate pair beyond the BMP,
/// in either case of hex digit.
fn unicode(c: char, spell: &mut Spell, out: &mut String) {
    let upper = spell.chance(2);
    let mut units = [0_u16; 2];
    for unit in c.encode_utf16(&mut units) {
        if upper {
            out.push_str(&format!("\\u{unit:04X}"));
        } else {
            out.push_str(&format!("\\u{unit:04x}"));
        }
    }
}
