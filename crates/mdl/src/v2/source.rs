//! Where things are in the files a bundle was read from, so a problem can
//! say `catalog.toml:12:5` instead of `/catalog/params/eps.bus_voltage`.
//!
//! A frontend reads files held in [`Sources`], builds a bundle, and records
//! a [`SourceMap`]: for each JSON Pointer into the bundle, the [`Span`] of
//! text it came from. Validation reports pointers, and the map turns each
//! into a [`SourceDiagnostic`] at the nearest place it recorded. A
//! frontend's own problems (a TOML syntax error, an XTCE construct MDL
//! can't represent) are `SourceDiagnostic`s from the start. [`merge`]
//! makes the bundles of several frontends one.
//! `spec/frontends.md` has the rules every frontend follows.

use std::collections::BTreeMap;
use std::fmt;

use crate::v2::diagnostics::{Diagnostic, Diagnostics};
use crate::v2::ir::Bundle;
use crate::v2::read::ReadError;

mod merge;

pub use merge::merge;

/// The text of every file a build reads, each under a name for people (a
/// path, usually).
#[derive(Debug, Clone, Default)]
pub struct Sources {
    files: Vec<Source>,
}

#[derive(Debug, Clone)]
struct Source {
    name: String,
    text: String,
    /// The byte offset where each line starts.
    line_starts: Vec<usize>,
}

/// One file in a [`Sources`]. An id is an index: used with another
/// `Sources`, it names whatever file that one has at the same place, or
/// none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceId(usize);

impl Sources {
    pub fn new() -> Self {
        Sources::default()
    }

    pub fn add(&mut self, name: impl Into<String>, text: impl Into<String>) -> SourceId {
        let text = text.into();
        let line_starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(at, _)| at + 1))
            .collect();
        self.files.push(Source {
            name: name.into(),
            text,
            line_starts,
        });
        SourceId(self.files.len() - 1)
    }

    /// The file, which frontends read through so that the spans they record
    /// and the text they read can't disagree.
    pub fn get(&self, id: SourceId) -> Option<SourceFile<'_>> {
        self.files.get(id.0).map(|file| SourceFile { id, file })
    }

    /// Every file, in the order they were added.
    pub fn iter(&self) -> impl Iterator<Item = SourceFile<'_>> {
        self.files
            .iter()
            .enumerate()
            .map(|(index, file)| SourceFile {
                id: SourceId(index),
                file,
            })
    }

    pub fn name(&self, id: SourceId) -> Option<&str> {
        self.get(id).map(|file| file.name())
    }

    pub fn text(&self, id: SourceId) -> Option<&str> {
        self.get(id).map(|file| file.text())
    }

    /// Where a span starts. `None` for a span whose file this `Sources`
    /// doesn't have.
    pub fn location(&self, span: Span) -> Option<Location> {
        self.get(span.source).map(|file| file.location(span.start))
    }
}

/// One file of a [`Sources`]: its id, name and text.
#[derive(Debug, Clone, Copy)]
pub struct SourceFile<'a> {
    id: SourceId,
    file: &'a Source,
}

impl<'a> SourceFile<'a> {
    pub fn id(&self) -> SourceId {
        self.id
    }

    pub fn name(&self) -> &'a str {
        &self.file.name
    }

    pub fn text(&self) -> &'a str {
        &self.file.text
    }

    /// A span of this file.
    pub fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.id, start, end)
    }

    /// The byte offset line `line` (counted from 1) starts at.
    pub fn line_start(&self, line: usize) -> Option<usize> {
        self.file.line_starts.get(line.checked_sub(1)?).copied()
    }

    /// The line and column of a byte offset. An offset inside a character
    /// counts as that character; one past the end, as the end.
    pub fn location(&self, offset: usize) -> Location {
        let offset = char_boundary(self.text(), offset);
        let line = self
            .file
            .line_starts
            .partition_point(|&start| start <= offset);
        let line_start = self.line_start(line).unwrap_or_default();
        let before = self.text().get(line_start..offset).unwrap_or_default();
        Location {
            line,
            column: before.chars().count() + 1,
        }
    }

    /// The line an offset is on, without its line break, and the byte
    /// offset the line starts at.
    fn line_at(&self, offset: usize) -> (&'a str, usize) {
        let line = self.location(offset).line;
        let start = self.line_start(line).unwrap_or_default();
        let rest = self.text().get(start..).unwrap_or_default();
        let text = rest.split('\n').next().unwrap_or_default();
        (text.strip_suffix('\r').unwrap_or(text), start)
    }
}

/// The nearest character boundary at or before `offset`, within the text.
fn char_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// A range of bytes in one source: `start` up to, not including, `end`.
/// Spans order by file, then by position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Span {
    pub source: SourceId,
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(source: SourceId, start: usize, end: usize) -> Self {
        Span { source, start, end }
    }
}

/// A position for people: a line and a column, both counted from 1. The
/// column counts characters (Unicode scalar values), not bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    pub line: usize,
    pub column: usize,
}

/// For each JSON Pointer into a bundle, the span of text its value came
/// from.
#[derive(Debug, Clone, Default)]
pub struct SourceMap {
    spans: BTreeMap<String, Span>,
}

impl SourceMap {
    pub fn new() -> Self {
        SourceMap::default()
    }

    /// Record where the value at `pointer` came from, replacing any span
    /// recorded for it before.
    pub fn insert(&mut self, pointer: impl Into<String>, span: Span) {
        self.spans.insert(pointer.into(), span);
    }

    /// The span recorded for exactly this pointer.
    pub fn get(&self, pointer: &str) -> Option<Span> {
        self.spans.get(pointer).copied()
    }

    /// The span of the value at `pointer`, or else of its nearest ancestor
    /// that has one: a missing field is reported where its object is.
    pub fn find(&self, pointer: &str) -> Option<Span> {
        let mut pointer = pointer;
        loop {
            if let Some(span) = self.spans.get(pointer) {
                return Some(*span);
            }
            let parent = pointer.rfind('/')?;
            pointer = pointer.get(..parent)?;
        }
    }

    pub fn len(&self) -> usize {
        self.spans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// A validation diagnostic, at the span its path leads to.
    /// A duplicate's first definition is located by its exact pointer;
    /// one the map doesn't have is named in the message instead.
    pub fn locate(&self, diagnostic: &Diagnostic) -> SourceDiagnostic {
        let mut message = diagnostic.message.clone();
        let first = diagnostic.first.as_deref().and_then(|first| {
            let span = self.get(first);
            if span.is_none() {
                message.push_str(&format!("; the first is at {first}"));
            }
            span
        });
        SourceDiagnostic {
            severity: Severity::Error,
            code: diagnostic.code.as_str(),
            message,
            context: diagnostic.context.clone(),
            path: diagnostic.path.clone(),
            span: self.find(&diagnostic.path),
            first,
        }
    }

    pub fn locate_all(&self, diagnostics: &Diagnostics) -> Vec<SourceDiagnostic> {
        diagnostics.iter().map(|d| self.locate(d)).collect()
    }

    /// A reader error for a bundle a frontend built (it doesn't have the
    /// IR's shape, say), at the span its path leads to.
    pub fn locate_read_error(&self, err: &ReadError) -> SourceDiagnostic {
        let path = err.path();
        SourceDiagnostic {
            severity: Severity::Error,
            code: err.code(),
            message: err.detail(),
            context: String::new(),
            path: path.to_string(),
            span: self.find(path),
            first: None,
        }
    }
}

/// What a frontend reads: a bundle, where each part of it came from, and
/// the problems that didn't stop it (what an importer left out, say).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Parsed {
    pub bundle: Bundle,
    pub map: SourceMap,
    pub warnings: Vec<SourceDiagnostic>,
}

impl Parsed {
    pub fn new(bundle: Bundle, map: SourceMap) -> Self {
        Parsed {
            bundle,
            map,
            warnings: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Severity {
    /// The bundle can't be built, or would decode or encode wrongly.
    Error,
    /// Something was left out that doesn't change decoded or encoded
    /// values.
    Warning,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A problem with where it is in the sources.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SourceDiagnostic {
    pub severity: Severity,
    /// A stable code. Validation's and the reader's are as they are
    /// (`unit_mismatch`); a frontend's start with its name (`toml.syntax`).
    pub code: &'static str,
    pub message: String,
    /// Where, for people, when the span alone doesn't say: "set
    /// vendor_x_v2, packet HK, entry mode". May be empty.
    pub context: String,
    /// A JSON Pointer into the bundle; empty for a problem outside it (a
    /// syntax error) or about all of it.
    pub path: String,
    pub span: Option<Span>,
    /// For something defined twice, where the first one is; `span` is
    /// the second.
    pub first: Option<Span>,
}

impl SourceDiagnostic {
    pub fn error(code: &'static str, message: impl Into<String>) -> Self {
        SourceDiagnostic::new(Severity::Error, code, message)
    }

    pub fn warning(code: &'static str, message: impl Into<String>) -> Self {
        SourceDiagnostic::new(Severity::Warning, code, message)
    }

    fn new(severity: Severity, code: &'static str, message: impl Into<String>) -> Self {
        SourceDiagnostic {
            severity,
            code,
            message: message.into(),
            context: String::new(),
            path: String::new(),
            span: None,
            first: None,
        }
    }

    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = context.into();
        self
    }

    pub fn with_first(mut self, first: Span) -> Self {
        self.first = Some(first);
        self
    }

    /// The diagnostic with its file, line and column, the line itself, and
    /// the span underlined:
    ///
    /// ```text
    /// error[unit_mismatch]: unit A isn't compatible with the param's unit V
    ///   --> bundle.json:12:21
    ///    |
    /// 12 |             "unit": "A",
    ///    |                     ^^^
    ///    = set vendor_x_v2, packet HK, entry bus_counts
    /// ```
    ///
    /// Something defined twice ends with where the first one is:
    /// `= the first is at catalog.toml:3:1`.
    ///
    /// A line longer than 120 columns is shown as a window around the span,
    /// with `...` where it's cut. Tabs are shown as four spaces and other
    /// control characters as spaces, so source text can't move the cursor.
    /// The underline counts every other character as one column, so it
    /// drifts after wide characters (most CJK, most emoji).
    pub fn display<'a>(&'a self, sources: &'a Sources) -> impl fmt::Display + 'a {
        Rendered {
            diagnostic: self,
            sources,
        }
    }
}

/// The one-line form, without sources: `error[code]: message (context)`.
impl fmt::Display for SourceDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}[{}]: {}", self.severity, self.code, self.message)?;
        if !self.context.is_empty() {
            write!(f, " ({})", self.context)?;
        }
        Ok(())
    }
}

struct Rendered<'a> {
    diagnostic: &'a SourceDiagnostic,
    sources: &'a Sources,
}

/// Tabs are shown as this many spaces, so the underline lines up.
const TAB_WIDTH: usize = 4;
/// Lines wider than this are shown as a window around the span: this many
/// characters before it, and this many from its start.
const MAX_LINE_WIDTH: usize = 120;
const WINDOW_BEFORE: usize = 40;
const WINDOW_FROM: usize = 80;
const CUT: &str = "...";

impl fmt::Display for Rendered<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let d = self.diagnostic;
        writeln!(f, "{}[{}]: {}", d.severity, d.code, d.message)?;
        let shown = d.span.and_then(|span| {
            let file = self.sources.get(span.source)?;
            Some((span, file))
        });
        // As wide as a one-digit line number's when there's no line.
        let mut gutter = 2;
        if let Some((span, file)) = shown {
            let location = file.location(span.start);
            let (line, line_start) = file.line_at(span.start);
            let number = location.line.to_string();
            gutter = number.len() + 1;
            let start = char_boundary(line, span.start.saturating_sub(line_start));
            let end = char_boundary(line, span.end.saturating_sub(line_start)).max(start);
            let window = Window::of(line, start, end);
            writeln!(
                f,
                "{:pad$}--> {}:{}:{}",
                "",
                file.name(),
                location.line,
                location.column,
                pad = gutter - 1
            )?;
            writeln!(f, "{:gutter$}|", "")?;
            writeln!(f, "{number} | {}", window.text)?;
            writeln!(
                f,
                "{:gutter$}| {}{}",
                "",
                " ".repeat(window.caret_at),
                "^".repeat(window.caret_width.max(1))
            )?;
        }
        if !d.context.is_empty() {
            writeln!(f, "{:gutter$}= {}", "", d.context)?;
        } else if shown.is_none() && !d.path.is_empty() {
            writeln!(f, "{:gutter$}= at {}", "", d.path)?;
        }
        let first = d.first.and_then(|span| {
            let file = self.sources.get(span.source)?;
            Some((file.name(), file.location(span.start)))
        });
        if let Some((name, location)) = first {
            writeln!(
                f,
                "{:gutter$}= the first is at {name}:{}:{}",
                "", location.line, location.column
            )?;
        }
        Ok(())
    }
}

/// The part of a line a diagnostic shows, ready to print, and where the
/// underline goes in it.
struct Window {
    text: String,
    caret_at: usize,
    caret_width: usize,
}

impl Window {
    /// `start` and `end` are byte offsets into `line`, on character
    /// boundaries, `start <= end`.
    fn of(line: &str, start: usize, end: usize) -> Window {
        let before = line.get(..start).unwrap_or_default();
        let from = line.get(start..).unwrap_or_default();
        let under = line.get(start..end).unwrap_or_default();
        if width(line) <= MAX_LINE_WIDTH {
            return Window {
                text: shown(line),
                caret_at: width(before),
                caret_width: width(under),
            };
        }
        let (before, cut_front) = last_chars(before, WINDOW_BEFORE);
        let (from, cut_back) = first_chars(from, WINDOW_FROM);
        let (under, _) = first_chars(under, WINDOW_FROM);
        let front = if cut_front { CUT } else { "" };
        let back = if cut_back { CUT } else { "" };
        Window {
            text: format!("{front}{}{}{back}", shown(before), shown(from)),
            caret_at: front.len() + width(before),
            caret_width: width(under),
        }
    }
}

/// The last `n` characters of `text`, and whether any were cut.
fn last_chars(text: &str, n: usize) -> (&str, bool) {
    let count = text.chars().count();
    if count <= n {
        return (text, false);
    }
    let at = text
        .char_indices()
        .nth(count - n)
        .map_or(text.len(), |(at, _)| at);
    (text.get(at..).unwrap_or_default(), true)
}

/// The first `n` characters of `text`, and whether any were cut.
fn first_chars(text: &str, n: usize) -> (&str, bool) {
    match text.char_indices().nth(n) {
        Some((at, _)) => (text.get(..at).unwrap_or_default(), true),
        None => (text, false),
    }
}

/// Source text as printed: tabs as spaces, and other control characters
/// (a carriage return, an escape) as one space each.
fn shown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\t' => out.push_str(&" ".repeat(TAB_WIDTH)),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// How many columns text takes as printed.
fn width(text: &str) -> usize {
    text.chars()
        .map(|c| if c == '\t' { TAB_WIDTH } else { 1 })
        .sum()
}

#[cfg(test)]
mod tests;
