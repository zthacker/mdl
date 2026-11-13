//! Payloads that aren't JSON MDL reads (`spec/decoding.md` §2.3), made
//! from a valid message: its text cut, wrapped or added to, or its
//! document given a name twice. Each is `invalid_json`, met before
//! anything but the encoding check (§3.3).

use proptest::sample::Index;

use super::{Doc, Edit};
use crate::layout::Malformed;

/// A change to a message's written text, whose document is an object.
#[derive(Debug, Clone)]
pub(super) enum TextEdit {
    /// These bytes before the text.
    Before(Vec<u8>),
    /// These bytes after it.
    After(Vec<u8>),
    /// The text cut before the document's closing brace, this many
    /// thousandths of the way to it.
    Cut(usize),
    /// The text replaced by these bytes.
    Whole(Vec<u8>),
    /// These bytes just inside the document's opening brace.
    Inside(Vec<u8>),
    /// A member, these bytes, first in the document.
    Member(Vec<u8>),
}

impl TextEdit {
    /// The text changed; `empty` when the document has no members, so a
    /// member put first needs no comma after it.
    pub(super) fn apply(&self, mut text: Vec<u8>, empty: bool) -> Vec<u8> {
        let inside = text
            .iter()
            .position(|&b| b == b'{')
            .map_or(0, |open| open + 1);
        match self {
            TextEdit::Before(bytes) => {
                let mut changed = bytes.clone();
                changed.extend(text);
                changed
            }
            TextEdit::After(bytes) => {
                text.extend_from_slice(bytes);
                text
            }
            TextEdit::Cut(thousandths) => {
                let close = text.iter().rposition(|&b| b == b'}').unwrap_or(0);
                text.truncate(close * thousandths / 1000);
                text
            }
            TextEdit::Whole(bytes) => bytes.clone(),
            TextEdit::Inside(bytes) => {
                text.splice(inside..inside, bytes.iter().copied());
                text
            }
            TextEdit::Member(bytes) => {
                let mut member = bytes.clone();
                if !empty {
                    member.push(b',');
                }
                text.splice(inside..inside, member);
                text
            }
        }
    }
}

/// How deep arrays and objects nest before MDL refuses them (§2.3): the
/// document is the first level.
const REFUSED_DEPTH: usize = 128;

/// The change that plants `malformed`, one of its spellings by `pick`,
/// and what it plants, for a failing case's report.
pub(super) fn plant(malformed: Malformed, pick: &Index) -> (Edit, String) {
    let text = |edit: TextEdit| {
        let what = match &edit {
            TextEdit::Before(bytes) => format!("{} before the document", shown(bytes)),
            TextEdit::After(bytes) => format!("{} after the document", shown(bytes)),
            TextEdit::Cut(thousandths) => format!("cut {thousandths}/1000 of the way"),
            TextEdit::Whole(bytes) => format!("the payload {}", shown(bytes)),
            TextEdit::Inside(bytes) => format!("{} inside the document", shown(bytes)),
            TextEdit::Member(bytes) => format!("{} first in the document", shown(bytes)),
        };
        (Edit::Text(edit), what)
    };
    let one = |spellings: &[&[u8]]| spellings[pick.index(spellings.len())].to_vec();
    match malformed {
        Malformed::Bom => text(TextEdit::Before(vec![0xEF, 0xBB, 0xBF])),
        Malformed::Cut => text(TextEdit::Cut(pick.index(1000))),
        Malformed::Deep => {
            // Inside the document, a member nested so the deepest value is
            // one level past what's read.
            let levels = REFUSED_DEPTH - 1;
            let value = if pick.index(2) == 0 {
                format!("{}1{}", r#"{"n":"#.repeat(levels), "}".repeat(levels))
            } else {
                format!("{}{}", "[".repeat(levels), "]".repeat(levels))
            };
            text(TextEdit::Member(format!(r#""bad":{value}"#).into_bytes()))
        }
        Malformed::Duplicate => match pick.index(2) {
            // The writer may spell the two names differently: they're one
            // name once unescaped.
            0 => (
                Edit::Twice(pick.index(64)),
                "a member of the document twice".to_string(),
            ),
            _ => {
                let twice = Doc::Object(vec![("y".into(), Doc::Int(1)), ("y".into(), Doc::Int(2))]);
                let deeper = Doc::Array(vec![Doc::Object(vec![("x".into(), twice)])]);
                (
                    Edit::Add("bad".into(), deeper),
                    "a name twice in an object two levels in".to_string(),
                )
            }
        },
        Malformed::Empty => text(TextEdit::Whole(one(&[b"", b" ", b"\n", b"\t\r\n "]))),
        Malformed::Escape => {
            let inside = one(&[
                b"\x00", b"\x01", b"\x1f", b"\t", b"\n", br"\x41", br"\u12", br"\U0041", br"\'",
                br"\a", br"\u00G0",
            ]);
            // `z` after it, so `\u12` is short whatever follows.
            let mut member = br#""bad":"a"#.to_vec();
            member.extend(inside);
            member.extend_from_slice(br#"z""#);
            text(TextEdit::Member(member))
        }
        Malformed::Huge => {
            let huge = format!("1{}", "0".repeat(309));
            let number = one(&[b"1e309", b"-1e400", b"2e308", b"1E+999", huge.as_bytes()]);
            let mut member = br#""bad":"#.to_vec();
            member.extend(number);
            text(TextEdit::Member(member))
        }
        Malformed::Number => {
            let number = one(&[
                b"01",
                b"-01",
                b"00",
                b"+1",
                b".5",
                b"1.",
                b"1.e5",
                b"0x10",
                b"1e",
                b"1e+",
                b"-",
                b"NaN",
                b"Infinity",
                b"-Infinity",
            ]);
            let mut member = br#""bad":"#.to_vec();
            member.extend(number);
            text(TextEdit::Member(member))
        }
        Malformed::Respelled => text(TextEdit::Member(one(&[
            br#""\u0061":1,"a":2"#,
            "\"\\u00e9\":1,\"\u{e9}\":2".as_bytes(),
            "\"\\ud83d\\ude00\":1,\"\u{1f600}\":2".as_bytes(),
            br#""\/":1,"/":2"#,
            br#""\u000a":1,"\n":2"#,
            br#""\"":1,"\u0022":2"#,
        ]))),
        Malformed::Surrogate => text(TextEdit::Member(one(&[
            br#""bad":"\ud800""#,
            br#""bad":"\udc00""#,
            br#""bad":"\ud800\u0041""#,
            br#""bad":"\ud800\ud800""#,
            br#""bad":"x\udfffy""#,
            br#""\ud800":1"#,
        ]))),
        Malformed::Token => match pick.index(3) {
            // Whitespace JSON doesn't allow, or a comment, between tokens.
            0 => text(TextEdit::Inside(one(&[
                b"\x0c",
                b"\x0b",
                "\u{a0}".as_bytes(),
                "\u{2028}".as_bytes(),
                b"/* c */",
                b"// c\n",
            ]))),
            _ => text(TextEdit::Member(one(&[
                br#""bad":[1,]"#,
                br#""bad":[1,,2]"#,
                br#""bad":[1 2]"#,
                br#""bad":{"a" 1}"#,
                br#""bad":{'a':1}"#,
                br#""bad":{a:1}"#,
                br#""bad":'x'"#,
                br#""bad":tru"#,
                br#""bad":True"#,
                br#""b1":1 "b2":2"#,
            ]))),
        },
        Malformed::Trailing => text(TextEdit::After(one(&[
            b"x", b" {}", b"1", b",", b"]", b"}", br#" "s""#, b"\nnull",
        ]))),
        Malformed::Utf8 => {
            let bytes = one(&[
                b"\xff",
                b"\xfe",
                b"\xc0\xaf",
                b"\xe0\x80\xaf",
                b"\xed\xa0\x80",
                b"\xf4\x90\x80\x80",
                b"\xc3",
                b"\x80",
            ]);
            match pick.index(3) {
                0 => text(TextEdit::Inside(bytes)),
                1 => {
                    let mut member = b"\"".to_vec();
                    member.extend(bytes);
                    member.extend_from_slice(b"\":1");
                    text(TextEdit::Member(member))
                }
                _ => {
                    let mut member = br#""bad":""#.to_vec();
                    member.extend(bytes);
                    member.push(b'"');
                    text(TextEdit::Member(member))
                }
            }
        }
    }
}

/// Bytes for a report: text, with what isn't UTF-8 replaced.
fn shown(bytes: &[u8]) -> String {
    format!("{:?}", String::from_utf8_lossy(bytes))
}
