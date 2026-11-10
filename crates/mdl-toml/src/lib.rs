//! MDL's TOML frontend: the IR spelled in TOML, as `spec/toml.md` in the
//! MDL repository describes.
//!
//! A TOML file holds part of a bundle, under the IR's own keys: catalog
//! params and commands, families and sets, in any mix. [`read`] merges the
//! files in the order given and reads the result with the reader every MDL
//! frontend uses, so a definition means the same in TOML as in JSON. Every
//! value keeps its file, line and column for diagnostics.
//!
//! ```
//! use mdl::source::Sources;
//!
//! let mut sources = Sources::new();
//! sources.add("catalog.toml", r#"
//! mdl_version = "2.0"
//!
//! [catalog.params."eps.bus_voltage"]
//! type = "f64"
//! unit = "V"
//! "#);
//! sources.add("vendor_x.toml", r#"
//! mdl_version = "2.0"
//!
//! [[families]]
//! name = "vendor_x"
//! encoding = "binary"
//! byte_order = "big"
//! alias = { kind = "bits", offset_bits = 0, size_bits = 16 }
//! "#);
//! let parsed = mdl_toml::read(sources.iter()).unwrap();
//! assert_eq!(parsed.bundle.families[0].name, "vendor_x");
//! assert!(parsed.bundle.catalog.params.contains_key("eps.bus_voltage"));
//! ```

use std::collections::BTreeMap;

use mdl::diagnostics::escape_pointer_token;
use mdl::ir::IrVersion;
use mdl::read::read_bundle_value;
use mdl::source::{Parsed, SourceDiagnostic, SourceFile, SourceMap, Span};
use serde_json::{Map, Value};
use toml::Spanned;
use toml::de::{DeTable, DeValue};

mod value;

use value::Converter;

/// What a TOML diagnostic is about. The codes are stable once released.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Code {
    /// Text that isn't TOML.
    Syntax,
    /// A file without `mdl_version`, a version that isn't one, or files
    /// with different major versions.
    Version,
    /// Two files that define the same thing.
    Conflict,
    /// An integer outside TOML's 64 bits, or a float that isn't finite.
    Number,
    /// A TOML date or time that isn't an instant in UTC.
    Datetime,
}

impl Code {
    /// Every code, for tools that map a diagnostic's code back.
    pub const ALL: &'static [Code] = &[
        Code::Syntax,
        Code::Version,
        Code::Conflict,
        Code::Number,
        Code::Datetime,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Code::Syntax => "toml.syntax",
            Code::Version => "toml.version",
            Code::Conflict => "toml.conflict",
            Code::Number => "toml.number",
            Code::Datetime => "toml.datetime",
        }
    }
}

/// The repository README's library example, compiled and run with this
/// crate's doc tests so it can't drift from the API.
#[cfg(doctest)]
#[doc = include_str!("../../../README.md")]
pub struct ReadmeExample;

/// Read a bundle from TOML files, merged in the order given. Every problem
/// in every file is reported, in file order and then by position.
pub fn read<'a>(
    files: impl IntoIterator<Item = SourceFile<'a>>,
) -> Result<Parsed, Vec<SourceDiagnostic>> {
    let mut merger = Merger::default();
    for file in files {
        merger.add(file);
    }
    merger.finish()
}

/// Who defined the value at a pointer.
#[derive(Debug, Clone, Copy)]
enum Claim {
    /// One file, all of it.
    Whole(Span),
    /// Files that add to the list or map there: where the first one did,
    /// and how many have.
    Merged { first: Span, files: usize },
}

impl Claim {
    fn first(self) -> Span {
        match self {
            Claim::Whole(span) | Claim::Merged { first: span, .. } => span,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Versions {
    first: (IrVersion, Span),
    highest: (IrVersion, Span),
}

#[derive(Default)]
struct Merger {
    /// How many files have been added, so each file's problems sort
    /// after the ones before it.
    files: usize,
    root: Map<String, Value>,
    map: SourceMap,
    claims: BTreeMap<String, Claim>,
    /// Each problem, after the order of the file it's in.
    errors: Vec<(usize, SourceDiagnostic)>,
    versions: Option<Versions>,
}

impl Merger {
    fn add(&mut self, file: SourceFile<'_>) {
        let order = self.files;
        self.files += 1;
        let (table, syntax) = DeTable::parse_recoverable(file.text());
        if !syntax.is_empty() {
            for err in syntax {
                let mut diagnostic = SourceDiagnostic::error(Code::Syntax.as_str(), err.message());
                if let Some(range) = err.span() {
                    diagnostic = diagnostic.with_span(file.span(range.start, range.end));
                }
                self.errors.push((order, diagnostic));
            }
            return;
        }
        let mut stated_version = false;
        for (key, value) in table.get_ref() {
            match key.get_ref().as_ref() {
                "mdl_version" => {
                    stated_version = true;
                    self.version(order, file, value);
                }
                "catalog" => self.catalog(order, file, value),
                list @ ("families" | "sets") => self.list(order, file, list, value),
                other => self.whole(order, file, &[other], value),
            }
        }
        if !stated_version {
            self.error(
                order,
                Code::Version,
                "every MDL TOML file states the IR version it's written in: start it with \
                 mdl_version = \"2.0\""
                    .to_string(),
                file.span(0, 0),
            );
        }
    }

    /// The bundle's version is the highest any file states; files of one
    /// bundle share a major version.
    fn version(&mut self, order: usize, file: SourceFile<'_>, value: &Spanned<DeValue<'_>>) {
        let span = span_of(file, value);
        let stated = match value.get_ref() {
            DeValue::String(text) => text.parse::<IrVersion>(),
            _ => Err("mdl_version is a string like \"2.0\"".to_string()),
        };
        let version = match stated {
            Ok(version) => version,
            Err(message) => return self.error(order, Code::Version, message, span),
        };
        match self.versions {
            None => {
                self.versions = Some(Versions {
                    first: (version, span),
                    highest: (version, span),
                });
            }
            Some(Versions {
                first: (first, first_span),
                ..
            }) if first.major != version.major => {
                let message = format!(
                    "this file is MDL IR {version}, but an earlier one is {first}; the files of \
                     a bundle share a major version"
                );
                self.errors.push((
                    order,
                    SourceDiagnostic::error(Code::Version.as_str(), message)
                        .with_span(span)
                        .with_first(first_span),
                ));
            }
            Some(ref mut versions) => {
                if version > versions.highest.0 {
                    versions.highest = (version, span);
                }
            }
        }
    }

    /// The catalog: files add params and commands to it, each defined by
    /// one file.
    fn catalog(&mut self, order: usize, file: SourceFile<'_>, value: &Spanned<DeValue<'_>>) {
        let DeValue::Table(table) = value.get_ref() else {
            return self.whole(order, file, &["catalog"], value);
        };
        if !self.merge(
            order,
            &["catalog"],
            span_of(file, value),
            Value::Object(Map::new()),
        ) {
            return;
        }
        for (key, item) in table {
            let key = key.get_ref().as_ref();
            match (key, item.get_ref()) {
                ("params" | "commands", DeValue::Table(members)) => {
                    let empty = Value::Object(Map::new());
                    if !self.merge(order, &["catalog", key], span_of(file, item), empty) {
                        continue;
                    }
                    for (name, member) in members {
                        self.whole(order, file, &["catalog", key, name.get_ref()], member);
                    }
                }
                _ => self.whole(order, file, &["catalog", key], item),
            }
        }
    }

    /// `families` or `sets`: each file's are appended.
    fn list(
        &mut self,
        order: usize,
        file: SourceFile<'_>,
        key: &str,
        value: &Spanned<DeValue<'_>>,
    ) {
        let DeValue::Array(items) = value.get_ref() else {
            return self.whole(order, file, &[key], value);
        };
        if !self.merge(
            order,
            &[key],
            span_of(file, value),
            Value::Array(Vec::new()),
        ) {
            return;
        }
        let start = match self.root.get(key) {
            Some(Value::Array(existing)) => existing.len(),
            _ => return,
        };
        let converted: Vec<Value> = items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let pointer = format!("{}/{}", pointer(&[key]), start + index);
                self.convert(order, file, item, pointer)
            })
            .collect();
        if let Some(Value::Array(existing)) = self.root.get_mut(key) {
            existing.extend(converted);
        }
    }

    /// Claim a list or map that files add to, creating it as `empty`.
    /// False, with the conflict reported, if one file defined it whole.
    fn merge(&mut self, order: usize, keys: &[&str], span: Span, empty: Value) -> bool {
        let at = pointer(keys);
        match self.claims.get_mut(&at) {
            None => {
                self.claims.insert(
                    at,
                    Claim::Merged {
                        first: span,
                        files: 1,
                    },
                );
                self.insert(keys, empty);
                true
            }
            Some(Claim::Merged { files, .. }) => {
                *files += 1;
                true
            }
            Some(&mut Claim::Whole(first)) => {
                self.conflict(order, keys, span, first);
                false
            }
        }
    }

    /// A value one file defines whole: a param, a command, any key that
    /// isn't a list or map files add to.
    fn whole(
        &mut self,
        order: usize,
        file: SourceFile<'_>,
        keys: &[&str],
        value: &Spanned<DeValue<'_>>,
    ) {
        let span = span_of(file, value);
        let at = pointer(keys);
        if let Some(claim) = self.claims.get(&at) {
            let first = claim.first();
            return self.conflict(order, keys, span, first);
        }
        self.claims.insert(at.clone(), Claim::Whole(span));
        let converted = self.convert(order, file, value, at);
        self.insert(keys, converted);
    }

    fn convert(
        &mut self,
        order: usize,
        file: SourceFile<'_>,
        value: &Spanned<DeValue<'_>>,
        mut pointer: String,
    ) -> Value {
        let mut errors = Vec::new();
        let converted = Converter {
            file,
            map: &mut self.map,
            errors: &mut errors,
        }
        .convert(value, &mut pointer);
        self.errors
            .extend(errors.into_iter().map(|error| (order, error)));
        converted
    }

    /// Put `value` at `keys`, under objects the merge created.
    fn insert(&mut self, keys: &[&str], value: Value) {
        let Some((last, parents)) = keys.split_last() else {
            return;
        };
        let mut object = &mut self.root;
        for key in parents {
            let parent = object
                .entry(key.to_string())
                .or_insert_with(|| Value::Object(Map::new()));
            // A parent is a map the merge claimed, so an object.
            let Value::Object(parent) = parent else {
                return;
            };
            object = parent;
        }
        object.insert(last.to_string(), value);
    }

    fn conflict(&mut self, order: usize, keys: &[&str], span: Span, first: Span) {
        let message = format!("{} is defined in two files", toml_key(keys));
        self.errors.push((
            order,
            SourceDiagnostic::error(Code::Conflict.as_str(), message)
                .with_path(pointer(keys))
                .with_span(span)
                .with_first(first),
        ));
    }

    fn error(&mut self, order: usize, code: Code, message: String, span: Span) {
        self.errors.push((
            order,
            SourceDiagnostic::error(code.as_str(), message).with_span(span),
        ));
    }

    fn finish(mut self) -> Result<Parsed, Vec<SourceDiagnostic>> {
        if let Some(versions) = self.versions {
            let (version, span) = versions.highest;
            self.root.insert(
                "mdl_version".to_string(),
                Value::String(version.to_string()),
            );
            self.map.insert("/mdl_version", span);
        }
        // A list or map from one file is where that file has it; one built
        // from several files is nowhere in particular.
        for (at, claim) in &self.claims {
            if let Claim::Merged { first, files: 1 } = claim {
                self.map.insert(at.clone(), *first);
            }
        }
        if !self.errors.is_empty() {
            let mut errors = self.errors;
            errors.sort_by_key(|(order, error)| (*order, error.span.map(|span| span.start)));
            return Err(errors.into_iter().map(|(_, error)| error).collect());
        }
        match read_bundle_value(&Value::Object(self.root)) {
            Ok(loaded) => Ok(Parsed::new(loaded.value, self.map)),
            Err(err) => Err(vec![self.map.locate_read_error(&err)]),
        }
    }
}

fn span_of(file: SourceFile<'_>, value: &Spanned<DeValue<'_>>) -> Span {
    let range = value.span();
    file.span(range.start, range.end)
}

fn pointer(keys: &[&str]) -> String {
    keys.iter()
        .map(|key| format!("/{}", escape_pointer_token(key)))
        .collect()
}

/// Keys as a TOML dotted key, quoting those that aren't bare:
/// `catalog.params."eps.bus_voltage"`.
fn toml_key(keys: &[&str]) -> String {
    let bare = |key: &str| {
        !key.is_empty()
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    };
    keys.iter()
        .map(|key| {
            if bare(key) {
                key.to_string()
            } else {
                serde_json::to_string(key).unwrap_or_else(|_| format!("{key:?}"))
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

#[cfg(test)]
mod tests;
