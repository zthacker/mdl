//! The JSON frontend: a bundle written as the IR itself, read with the
//! position of every value, so problems point at the text.
//!
//! The text goes through the reader's strict parse and typed reading, the
//! same as [`read_bundle`](crate::v2::read::read_bundle); this module adds
//! the [`SourceMap`], from a scan of text that already parsed.

use std::collections::BTreeMap;

use crate::canonical::{self, CanonicalError};
use crate::v2::diagnostics::escape_pointer_token;
use crate::v2::read::{ReadError, read_bundle_value};
use crate::v2::source::{Parsed, SourceDiagnostic, SourceFile, SourceMap, Span};

/// Read a bundle from one JSON file. The file must hold a whole bundle;
/// several files aren't merged, as they are in TOML.
pub fn read(source: SourceFile<'_>) -> Result<Parsed, Vec<SourceDiagnostic>> {
    let text = source.text();
    if text.starts_with('\u{feff}') {
        // Refused by the input rules, but serde_json's "expected value" at
        // an invisible character doesn't say why.
        return Err(vec![
            SourceDiagnostic::error(
                "invalid_json",
                "the file starts with a byte order mark; save it as UTF-8 without one",
            )
            .with_span(source.span(0, '\u{feff}'.len_utf8())),
        ]);
    }
    let value = match canonical::parse_strict(text.as_bytes()) {
        Ok(value) => value,
        Err(err) => return Err(vec![parse_error(source, err)]),
    };
    let map = match scan(source) {
        Ok(map) => map,
        // Text that parsed has no duplicate, but the bundle still reads
        // without positions if the scan disagrees.
        Err(_) => SourceMap::new(),
    };
    match read_bundle_value(&value) {
        Ok(loaded) => Ok(Parsed::new(loaded.value, map)),
        Err(err) => Err(vec![map.locate_read_error(&err)]),
    }
}

/// A strict-parse error, at the text it's about.
fn parse_error(source: SourceFile<'_>, err: CanonicalError) -> SourceDiagnostic {
    let read = ReadError::Canonical(err);
    let diagnostic = SourceDiagnostic::error(read.code(), read.detail());
    match read {
        ReadError::Canonical(CanonicalError::DuplicateKey { line, column, .. }) => {
            match scan(source) {
                Err(Stop::Duplicate(duplicate)) => duplicate_key(duplicate),
                _ => at_position(diagnostic, source, line, column),
            }
        }
        ReadError::Canonical(CanonicalError::InvalidJson { line, column, .. }) => {
            at_position(diagnostic, source, line, column)
        }
        ReadError::Canonical(
            CanonicalError::InexactInteger { ref path, .. }
            | CanonicalError::OutOfRange { ref path, .. },
        ) => {
            let span = scan(source).ok().and_then(|map| map.find(path));
            let diagnostic = diagnostic.with_path(path.clone());
            match span {
                Some(span) => diagnostic.with_span(span),
                None => diagnostic,
            }
        }
        _ => diagnostic,
    }
}

fn duplicate_key(duplicate: Duplicate) -> SourceDiagnostic {
    SourceDiagnostic::error(
        "duplicate_key",
        format!("duplicate key {:?} in one object", duplicate.key),
    )
    .with_path(duplicate.pointer)
    .with_span(duplicate.second)
    .with_first(duplicate.first)
}

/// A diagnostic at a line and byte column as serde_json reports them.
fn at_position(
    diagnostic: SourceDiagnostic,
    source: SourceFile<'_>,
    line: usize,
    column: usize,
) -> SourceDiagnostic {
    let Some(start) = offset_of(source, line, column) else {
        return diagnostic;
    };
    let end = source
        .text()
        .get(start..)
        .and_then(|rest| rest.chars().next())
        .map_or(start, |c| start + c.len_utf8());
    diagnostic.with_span(source.span(start, end))
}

/// The byte offset of a line and byte column counted from 1, as serde_json
/// reports them; `None` when there's no position (line 0) or the line isn't
/// in the text. Column 0 (serde_json's for an end right after a line
/// break) is the line's start.
fn offset_of(source: SourceFile<'_>, line: usize, column: usize) -> Option<usize> {
    let text = source.text();
    let line_start = source.line_start(line)?;
    let mut offset = (line_start + column.saturating_sub(1)).min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    Some(offset)
}

/// Nesting deeper than serde_json allows; text the strict parse accepted
/// never gets here.
const MAX_DEPTH: usize = 128;

/// Why a scan stopped early.
#[derive(Debug)]
enum Stop {
    /// An object with a key twice: what the strict parse refuses.
    Duplicate(Duplicate),
    /// Text that isn't JSON as the scan expects it.
    Malformed,
}

#[derive(Debug)]
struct Duplicate {
    key: String,
    /// The object's pointer, with the key appended.
    pointer: String,
    first: Span,
    second: Span,
}

/// The span of every value in JSON text, by JSON Pointer. Built for text
/// that already parsed, but safe on anything: on text it doesn't expect, it
/// stops rather than panicking or recursing without bound.
fn scan(source: SourceFile<'_>) -> Result<SourceMap, Stop> {
    let text = source.text();
    let mut scanner = Scanner {
        source,
        text,
        bytes: text.as_bytes(),
        at: 0,
        depth: 0,
        pointer: String::new(),
        map: SourceMap::new(),
    };
    scanner.value()?;
    Ok(scanner.map)
}

struct Scanner<'a> {
    source: SourceFile<'a>,
    text: &'a str,
    bytes: &'a [u8],
    at: usize,
    depth: usize,
    pointer: String,
    map: SourceMap,
}

impl Scanner<'_> {
    fn value(&mut self) -> Result<(), Stop> {
        self.skip_whitespace();
        let start = self.at;
        match self.peek()? {
            b'{' => self.object()?,
            b'[' => self.array()?,
            b'"' => {
                self.string()?;
            }
            _ => self.scalar()?,
        }
        self.map
            .insert(self.pointer.clone(), self.source.span(start, self.at));
        Ok(())
    }

    fn object(&mut self) -> Result<(), Stop> {
        self.enter()?;
        let mut keys: BTreeMap<String, Span> = BTreeMap::new();
        self.skip_whitespace();
        if self.peek()? == b'}' {
            self.at += 1;
            return self.leave();
        }
        loop {
            self.skip_whitespace();
            let key_start = self.at;
            let quoted = self.string()?;
            let key: String = serde_json::from_str(quoted).map_err(|_| Stop::Malformed)?;
            let key_span = self.source.span(key_start, self.at);
            let parent = self.pointer.len();
            self.pointer.push('/');
            self.pointer.push_str(&escape_pointer_token(&key));
            if let Some(first) = keys.get(&key) {
                return Err(Stop::Duplicate(Duplicate {
                    key,
                    pointer: self.pointer.clone(),
                    first: *first,
                    second: key_span,
                }));
            }
            keys.insert(key, key_span);
            self.skip_whitespace();
            self.consume(b':')?;
            self.value()?;
            self.pointer.truncate(parent);
            self.skip_whitespace();
            match self.next()? {
                b',' => {}
                b'}' => return self.leave(),
                _ => return Err(Stop::Malformed),
            }
        }
    }

    fn array(&mut self) -> Result<(), Stop> {
        self.enter()?;
        self.skip_whitespace();
        if self.peek()? == b']' {
            self.at += 1;
            return self.leave();
        }
        let mut index = 0usize;
        loop {
            let parent = self.pointer.len();
            self.pointer.push('/');
            self.pointer.push_str(&index.to_string());
            self.value()?;
            self.pointer.truncate(parent);
            index += 1;
            self.skip_whitespace();
            match self.next()? {
                b',' => {}
                b']' => return self.leave(),
                _ => return Err(Stop::Malformed),
            }
        }
    }

    /// A string, quotes included, as it's written.
    fn string(&mut self) -> Result<&str, Stop> {
        let start = self.at;
        self.consume(b'"')?;
        loop {
            match self.next()? {
                b'\\' => {
                    self.next()?;
                }
                b'"' => break,
                _ => {}
            }
        }
        self.text.get(start..self.at).ok_or(Stop::Malformed)
    }

    /// A number, `true`, `false` or `null`: everything up to the next
    /// delimiter.
    fn scalar(&mut self) -> Result<(), Stop> {
        let start = self.at;
        while let Some(&byte) = self.bytes.get(self.at) {
            if matches!(byte, b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r') {
                break;
            }
            self.at += 1;
        }
        if self.at == start {
            return Err(Stop::Malformed);
        }
        Ok(())
    }

    /// Past the opening bracket, one level deeper.
    fn enter(&mut self) -> Result<(), Stop> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(Stop::Malformed);
        }
        self.at += 1;
        Ok(())
    }

    fn leave(&mut self) -> Result<(), Stop> {
        self.depth -= 1;
        Ok(())
    }

    fn skip_whitespace(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.bytes.get(self.at) {
            self.at += 1;
        }
    }

    fn peek(&self) -> Result<u8, Stop> {
        self.bytes.get(self.at).copied().ok_or(Stop::Malformed)
    }

    fn next(&mut self) -> Result<u8, Stop> {
        let byte = self.peek()?;
        self.at += 1;
        Ok(byte)
    }

    fn consume(&mut self, wanted: u8) -> Result<(), Stop> {
        if self.next()? == wanted {
            Ok(())
        } else {
            Err(Stop::Malformed)
        }
    }
}

#[cfg(test)]
mod tests;
