//! Values in reports: decoded and encoded values as JSON, and bytes as
//! hex both ways.

use serde::Serialize;

/// Why a message didn't decode or a command didn't encode: the library's
/// stable code and its message.
#[derive(Debug, Clone, Serialize)]
pub struct Failure {
    pub code: String,
    pub message: String,
}

/// Bytes from hex digits. Spaces, `_` and `:` may separate groups of
/// digits, and each group may start with `0x` (`0a1b`, `0a 1b`,
/// `0x0a1b`, `0x0a 0x1b`, `0a:1b`).
pub fn parse_hex(text: &str) -> Result<Vec<u8>, String> {
    let mut digits = String::new();
    for group in text.split(|c: char| c.is_whitespace() || c == '_' || c == ':') {
        let group = group
            .strip_prefix("0x")
            .or_else(|| group.strip_prefix("0X"))
            .unwrap_or(group);
        digits.push_str(group);
    }
    if !digits.len().is_multiple_of(2) {
        return Err(format!(
            "{text:?} has an odd number of hex digits; a byte is two"
        ));
    }
    digits
        .as_bytes()
        .chunks(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).map_err(|_| format!("{text:?} isn't hex"))?;
            u8::from_str_radix(pair, 16).map_err(|_| format!("{pair:?} isn't a hex byte"))
        })
        .collect()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_takes_separators_and_prefixes() {
        for text in [
            "0a1b",
            "0a 1b",
            "0x0a1b",
            "0x0a 0x1b",
            "0a:1b",
            "0A_1B",
            " 0a1b ",
        ] {
            assert_eq!(parse_hex(text), Ok(vec![0x0a, 0x1b]), "{text}");
        }
        assert!(parse_hex("0a1").unwrap_err().contains("odd number"));
        assert!(parse_hex("zz").unwrap_err().contains("isn't a hex byte"));
        assert_eq!(hex(&[0x0a, 0xff]), "0aff");
    }
}
