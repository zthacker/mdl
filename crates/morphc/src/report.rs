//! What a build prints: diagnostics on stderr and the artifacts on stdout
//! for people, or one JSON document on stdout for tools.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write as _};
use std::process::ExitCode;

use clap::ValueEnum;
use mdl::v2::source::{Severity, SourceDiagnostic, Sources, Span};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// Diagnostics with their source lines on stderr, the result on
    /// stdout.
    Human,
    /// One JSON document on stdout, with every diagnostic.
    Json,
}

/// The JSON document's format version.
const REPORT_VERSION: u32 = 1;

/// One artifact of the build, or one the replaced build had and this one
/// doesn't.
#[derive(Debug, Serialize)]
pub struct Row {
    /// `catalog`, `family` or `set`.
    pub kind: &'static str,
    /// The family's or set's name; none for the catalog.
    pub name: Option<String>,
    /// The artifact's digest, or a removed one's last digest.
    pub digest: String,
    /// Against the build this one replaces or continues: `new`,
    /// `changed`, `unchanged` or `removed`. None when there's none.
    pub status: Option<&'static str>,
    /// Whether this build wrote it to the store.
    pub written: bool,
}

/// What a successful build made.
#[derive(Debug, Serialize)]
pub struct Built {
    pub mdl_version: String,
    pub params: usize,
    pub commands: usize,
    pub families: usize,
    pub sets: usize,
    pub artifacts: Vec<Row>,
    /// The store written, if any.
    pub out: Option<String>,
    /// The digest of every artifact the store didn't have, including
    /// older catalogs a rebuild into a new store brought along.
    pub written: Vec<String>,
}

pub struct Report<'a> {
    sources: &'a Sources,
    format: Format,
    all_warnings: bool,
    diagnostics: Vec<SourceDiagnostic>,
    notes: Vec<String>,
}

impl<'a> Report<'a> {
    pub fn new(sources: &'a Sources, format: Format, all_warnings: bool) -> Self {
        Report {
            sources,
            format,
            all_warnings,
            diagnostics: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn add(&mut self, diagnostics: impl IntoIterator<Item = SourceDiagnostic>) {
        self.diagnostics.extend(diagnostics);
    }

    /// A line for people about what happened or what to do next.
    pub fn note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }

    /// Print everything and give the exit code: 0 when built, 1 when the
    /// definitions have errors.
    pub fn finish<T: Outcome>(self, outcome: Option<T>) -> ExitCode {
        let code = match outcome {
            Some(_) => ExitCode::SUCCESS,
            None => ExitCode::from(1),
        };
        let printed = match self.format {
            Format::Human => emit(io::stderr(), &self.human_diagnostics()).and_then(|()| {
                emit(
                    io::stdout(),
                    &outcome.as_ref().map_or_else(String::new, Outcome::human),
                )
            }),
            Format::Json => {
                let document = Document {
                    ok: outcome.is_some(),
                    error: None,
                    diagnostics: self
                        .diagnostics
                        .iter()
                        .map(|d| JsonDiagnostic::of(d, self.sources))
                        .collect(),
                    notes: &self.notes,
                    outcome: Some(outcome.as_ref()),
                };
                emit(io::stdout(), &json(&document))
            }
        };
        match printed {
            Ok(()) => code,
            Err(err) => output_failed(&err),
        }
    }

    /// Every error, the first warning of each code (or every warning),
    /// the notes, and a tally.
    fn human_diagnostics(&self) -> String {
        let counts = warning_counts(&self.diagnostics);
        let mut text = String::new();
        let mut shown = BTreeSet::new();
        for d in &self.diagnostics {
            let warning = d.severity == Severity::Warning;
            if warning && !self.all_warnings && !shown.insert(d.code) {
                continue;
            }
            text.push_str(&d.display(self.sources).to_string());
            let more = counts.get(d.code).copied().unwrap_or(0).saturating_sub(1);
            if warning && !self.all_warnings && more > 0 {
                let gutter = d
                    .span
                    .and_then(|span| self.sources.location(span))
                    .map_or(2, |location| location.line.to_string().len() + 1);
                text.push_str(&format!(
                    "{:gutter$}= and {more} more {} warning{} (--all-warnings shows them)\n",
                    "",
                    d.code,
                    plural(more)
                ));
            }
            text.push('\n');
        }
        for note in &self.notes {
            text.push_str(&format!("note: {note}\n"));
        }
        if let Some(line) = tally(&self.diagnostics, &counts) {
            text.push_str(&format!("morphc: {line}\n"));
        }
        text
    }
}

/// What morphc prints when it can't run (exit code 2): the reason on
/// stderr, and for tools a document saying so.
pub fn failed(format: Format, err: &anyhow::Error) -> ExitCode {
    let code = ExitCode::from(2);
    let message = format!("{err:#}");
    let mut printed = emit(io::stderr(), &format!("morphc: {message}\n"));
    if format == Format::Json {
        let document: Document<'_, Built> = Document {
            ok: false,
            error: Some(message),
            diagnostics: Vec::new(),
            notes: &[],
            outcome: None,
        };
        printed = printed.and_then(|()| emit(io::stdout(), &json(&document)));
    }
    match printed {
        Ok(()) => code,
        Err(err) => output_failed(&err),
    }
}

/// Write all of `text`. A reader that went away (`morphc build ... |
/// head`) isn't an error.
fn emit(mut to: impl io::Write, text: &str) -> io::Result<()> {
    match to.write_all(text.as_bytes()).and_then(|()| to.flush()) {
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        other => other,
    }
}

/// Output that couldn't be written (a full disk) leaves the result
/// unknown to whoever reads it, so morphc exits 2, saying why on stderr
/// if stderr isn't what failed.
fn output_failed(err: &io::Error) -> ExitCode {
    writeln!(io::stderr(), "morphc: can't write the output: {err}").ok();
    ExitCode::from(2)
}

fn json<T: Serialize>(value: &T) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap_or_else(|err| {
        serde_json::json!({ "report": REPORT_VERSION, "ok": false, "error": err.to_string() })
            .to_string()
    });
    text.push('\n');
    text
}

/// How many warnings of each code.
fn warning_counts(diagnostics: &[SourceDiagnostic]) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for d in diagnostics {
        if d.severity == Severity::Warning {
            *counts.entry(d.code).or_insert(0) += 1;
        }
    }
    counts
}

/// "2 errors, 230 warnings (214 xtce.ignored, 16 xtce.differs)", or
/// nothing when there are no diagnostics.
fn tally(diagnostics: &[SourceDiagnostic], counts: &BTreeMap<&str, usize>) -> Option<String> {
    let errors = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    let warnings: usize = counts.values().sum();
    let mut parts = Vec::new();
    if errors > 0 {
        parts.push(format!("{errors} error{}", plural(errors)));
    }
    if warnings > 0 {
        let mut by_code: Vec<(&str, usize)> = counts.iter().map(|(c, n)| (*c, *n)).collect();
        by_code.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let codes: Vec<String> = by_code.iter().map(|(c, n)| format!("{n} {c}")).collect();
        parts.push(format!(
            "{warnings} warning{} ({})",
            plural(warnings),
            codes.join(", ")
        ));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The artifacts, one per line, then a summary.
fn human_built(built: &Built) -> String {
    let label = |row: &Row| match &row.name {
        Some(name) => format!("{} {name}", row.kind),
        None => row.kind.to_string(),
    };
    let width = built
        .artifacts
        .iter()
        .map(|row| label(row).len())
        .max()
        .unwrap_or(0);
    let mut text = String::new();
    for row in &built.artifacts {
        let mut line = format!("{:width$}  {}", label(row), row.digest);
        if let Some(status) = row.status {
            line.push_str("  ");
            line.push_str(status);
        }
        if row.written {
            line.push_str("  (written)");
        }
        text.push_str(line.trim_end());
        text.push('\n');
    }
    let count = |n: usize, what: &str| format!("{n} {what}{}", plural(n));
    let families = if built.families == 1 {
        "1 family".to_string()
    } else {
        format!("{} families", built.families)
    };
    text.push_str(&format!(
        "ok: {}, {}, {families}, {} (MDL IR {})",
        count(built.params, "param"),
        count(built.commands, "command"),
        count(built.sets, "set"),
        built.mdl_version
    ));
    if let Some(out) = &built.out {
        text.push_str(&format!(
            "; {} written to {out}",
            count(built.written.len(), "new artifact")
        ));
    }
    text.push('\n');
    text
}

/// What a command made, printed after the diagnostics: under its key in
/// the JSON document, or as text on stdout.
pub trait Outcome: Serialize {
    /// Its key in the JSON document: `build`, `inspect`.
    const KEY: &'static str;

    fn human(&self) -> String;
}

impl Outcome for Built {
    const KEY: &'static str = "build";

    fn human(&self) -> String {
        human_built(self)
    }
}

/// The JSON document: the same keys for every command, then the command's
/// own (null when it made nothing). A document for a run that couldn't
/// start has no command key.
struct Document<'a, T> {
    ok: bool,
    /// Why morphc couldn't run (exit code 2), with no diagnostics.
    error: Option<String>,
    diagnostics: Vec<JsonDiagnostic>,
    notes: &'a [String],
    outcome: Option<Option<&'a T>>,
}

impl<T: Outcome> Serialize for Document<'_, T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("report", &REPORT_VERSION)?;
        map.serialize_entry("ok", &self.ok)?;
        map.serialize_entry("error", &self.error)?;
        map.serialize_entry("diagnostics", &self.diagnostics)?;
        map.serialize_entry("notes", self.notes)?;
        if let Some(outcome) = &self.outcome {
            map.serialize_entry(T::KEY, outcome)?;
        }
        map.end()
    }
}

#[derive(Serialize)]
struct JsonDiagnostic {
    severity: &'static str,
    code: &'static str,
    message: String,
    /// Where, for people; none when the location says it all.
    context: Option<String>,
    /// A JSON Pointer into the bundle; none for a problem outside it (a
    /// syntax error) or about all of it.
    path: Option<String>,
    location: Option<Place>,
    first: Option<Place>,
}

/// Where in a file: `start` and `end` are byte offsets (UTF-8), lines and
/// columns count from 1, columns in characters.
#[derive(Serialize)]
struct Place {
    file: String,
    start: usize,
    end: usize,
    line: usize,
    column: usize,
    end_line: usize,
    end_column: usize,
}

impl JsonDiagnostic {
    fn of(d: &SourceDiagnostic, sources: &Sources) -> Self {
        let given = |text: &str| (!text.is_empty()).then(|| text.to_string());
        JsonDiagnostic {
            severity: d.severity.as_str(),
            code: d.code,
            message: d.message.clone(),
            context: given(&d.context),
            path: given(&d.path),
            location: d.span.and_then(|span| Place::of(span, sources)),
            first: d.first.and_then(|span| Place::of(span, sources)),
        }
    }
}

impl Place {
    fn of(span: Span, sources: &Sources) -> Option<Place> {
        let file = sources.get(span.source)?;
        let start = file.location(span.start);
        let end = file.location(span.end);
        Some(Place {
            file: file.name().to_string(),
            start: span.start,
            end: span.end,
            line: start.line,
            column: start.column,
            end_line: end.line,
            end_column: end.column,
        })
    }
}
