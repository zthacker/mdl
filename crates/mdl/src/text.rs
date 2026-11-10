//! Text in a field's encoding, both ways: UTF-8 (the default), ASCII and
//! Latin-1. Decoding and encoding share these, so they agree on what each
//! encoding allows.

use crate::ir::TextEncoding;

/// The encoding's name, for messages.
pub(crate) fn name(encoding: Option<TextEncoding>) -> &'static str {
    match encoding.unwrap_or(TextEncoding::Utf8) {
        TextEncoding::Utf8 => "UTF-8",
        TextEncoding::Ascii => "ASCII",
        TextEncoding::Latin1 => "Latin-1",
    }
}

/// Bytes as text, strictly: UTF-8 must be valid, ASCII below 0x80, and
/// Latin-1 maps each byte to the code point of the same value. `None` if
/// the bytes aren't text in the encoding.
pub(crate) fn decode(bytes: &[u8], encoding: Option<TextEncoding>) -> Option<String> {
    match encoding.unwrap_or(TextEncoding::Utf8) {
        TextEncoding::Utf8 => std::str::from_utf8(bytes).ok().map(str::to_string),
        TextEncoding::Ascii => bytes
            .is_ascii()
            .then(|| bytes.iter().map(|&b| char::from(b)).collect()),
        TextEncoding::Latin1 => Some(bytes.iter().map(|&b| char::from(b)).collect()),
    }
}

/// Text as bytes: UTF-8 as it is, ASCII only below 0x80, Latin-1 only up to
/// U+00FF. `None` if the text can't be written in the encoding.
pub(crate) fn encode(text: &str, encoding: Option<TextEncoding>) -> Option<Vec<u8>> {
    match encoding.unwrap_or(TextEncoding::Utf8) {
        TextEncoding::Utf8 => Some(text.as_bytes().to_vec()),
        TextEncoding::Ascii => text.is_ascii().then(|| text.as_bytes().to_vec()),
        TextEncoding::Latin1 => text
            .chars()
            .map(|c| u8::try_from(u32::from(c)).ok())
            .collect(),
    }
}
