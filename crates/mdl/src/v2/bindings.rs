//! Bindings: which asset a native id is, over time.
//!
//! A family's alias locator reads a native id from each message: a
//! spacecraft id, a serial number, a MAVLink system id. An asset registry
//! says which asset that id is at a given time and, for a family without a
//! version locator, which set decodes its messages. Decoders ask through
//! the [`Resolver`] trait. [`Bindings`] is the implementation MDL ships,
//! built from rows ([`Bindings::new`]) or from a bindings file
//! ([`Bindings::from_toml`], format in `spec/bindings.md`).
//!
//! Rows are effective-dated by ground time. A row applies from its
//! `effective_from` until the next row for the same family and native id,
//! so a timeline keeps every reassignment, and decoding a message again
//! later gives the answer that was right when it arrived. A retired row
//! ends a timeline: from then on the id is no asset, until a later row
//! binds it again.
//!
//! Native ids compare by value: `"42"` and `42` are one id. Registries
//! often keep ids as text, and a native id is a name, not a JSON value.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::Value;
use thiserror::Error;

use crate::v2::diagnostics::{Code, Diagnostics, Place, Report, escape_pointer_token};
use crate::v2::ir::{Locator, Scalar};
use crate::v2::read::{self, ReadError};
use crate::v2::time::Timestamp;
use crate::v2::validate::Validated;
use crate::v2::validate::rules::{
    check_integer_spelling, check_not_empty, check_scalar, integer_text, is_token,
};

/// A native id: an integer any locator can produce (from `i64::MIN` to
/// `u64::MAX`), or text that doesn't spell one.
///
/// Text in an integer's one decimal spelling (no `+`, no leading zeros) is
/// that integer, so each id has one value and one `Display` form:
///
/// ```
/// use mdl::v2::bindings::NativeId;
///
/// assert_eq!(NativeId::from_text("42"), NativeId::from(42_u64));
/// assert_ne!(NativeId::from_text("042"), NativeId::from(42_u64));
/// assert_eq!(NativeId::from_text("PX-9").to_string(), "PX-9");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NativeId(Repr);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Repr {
    Int(i128),
    Text(String),
}

const SMALLEST_ID: i128 = i64::MIN as i128;
const LARGEST_ID: i128 = u64::MAX as i128;

impl NativeId {
    pub fn from_text(text: impl Into<String>) -> NativeId {
        let text = text.into();
        match integer_text(&text) {
            Some(value) if (SMALLEST_ID..=LARGEST_ID).contains(&value) => {
                NativeId(Repr::Int(value))
            }
            _ => NativeId(Repr::Text(text)),
        }
    }

    pub fn as_int(&self) -> Option<i128> {
        match &self.0 {
            Repr::Int(value) => Some(*value),
            Repr::Text(_) => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match &self.0 {
            Repr::Int(_) => None,
            Repr::Text(text) => Some(text),
        }
    }
}

impl From<u64> for NativeId {
    fn from(value: u64) -> NativeId {
        NativeId(Repr::Int(i128::from(value)))
    }
}

impl From<i64> for NativeId {
    fn from(value: i64) -> NativeId {
        NativeId(Repr::Int(i128::from(value)))
    }
}

impl From<u32> for NativeId {
    fn from(value: u32) -> NativeId {
        NativeId(Repr::Int(i128::from(value)))
    }
}

/// So `NativeId::from(42)` works: an unsuffixed literal is an `i32`.
impl From<i32> for NativeId {
    fn from(value: i32) -> NativeId {
        NativeId(Repr::Int(i128::from(value)))
    }
}

impl From<&Scalar> for NativeId {
    fn from(scalar: &Scalar) -> NativeId {
        match scalar {
            Scalar::Int(n) => NativeId::from(*n),
            Scalar::Str(text) => NativeId::from_text(text.as_str()),
        }
    }
}

impl fmt::Display for NativeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Repr::Int(value) => write!(f, "{value}"),
            Repr::Text(text) => f.write_str(text),
        }
    }
}

/// What a native id is while a row is in effect.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Binding {
    /// The asset, as the registry names it.
    pub asset: String,
    /// The set that decodes the asset's messages. Only for a family without
    /// a version locator; otherwise each message's version picks the set.
    pub set: Option<String>,
}

impl Binding {
    pub fn new(asset: impl Into<String>) -> Self {
        Binding {
            asset: asset.into(),
            set: None,
        }
    }

    pub fn with_set(mut self, set: impl Into<String>) -> Self {
        self.set = Some(set.into());
        self
    }
}

/// One row of an asset registry: from `effective_from` on, `native_id` in
/// `family` is `binding`, or no asset if the row retires it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Row {
    pub family: String,
    pub native_id: NativeId,
    pub effective_from: Timestamp,
    /// `None` retires the native id.
    pub binding: Option<Binding>,
}

impl Row {
    pub fn bound(
        family: impl Into<String>,
        native_id: NativeId,
        effective_from: Timestamp,
        binding: Binding,
    ) -> Self {
        Row {
            family: family.into(),
            native_id,
            effective_from,
            binding: Some(binding),
        }
    }

    pub fn retired(
        family: impl Into<String>,
        native_id: NativeId,
        effective_from: Timestamp,
    ) -> Self {
        Row {
            family: family.into(),
            native_id,
            effective_from,
            binding: None,
        }
    }
}

/// Which asset a native id is at an instant.
///
/// Decoders call this once per message, so implementations answer from
/// memory: a registry kept in a database or a key-value store is mirrored
/// into a local snapshot that a watch keeps current, and decoding never
/// waits on I/O. `None` means the id is no asset at that time: unknown,
/// not yet bound, or retired.
///
/// Resolvers are `Send + Sync` because decoders share them across threads,
/// so an `Arc<dyn Resolver>` can sit in state that any worker uses.
/// References, boxes and `Arc`s of a resolver are resolvers too.
pub trait Resolver: Send + Sync {
    fn resolve(&self, family: &str, native_id: &NativeId, at: Timestamp) -> Option<Arc<Binding>>;
}

impl<R: Resolver + ?Sized> Resolver for &R {
    fn resolve(&self, family: &str, native_id: &NativeId, at: Timestamp) -> Option<Arc<Binding>> {
        (**self).resolve(family, native_id, at)
    }
}

impl<R: Resolver + ?Sized> Resolver for Box<R> {
    fn resolve(&self, family: &str, native_id: &NativeId, at: Timestamp) -> Option<Arc<Binding>> {
        (**self).resolve(family, native_id, at)
    }
}

impl<R: Resolver + ?Sized> Resolver for Arc<R> {
    fn resolve(&self, family: &str, native_id: &NativeId, at: Timestamp) -> Option<Arc<Binding>> {
        (**self).resolve(family, native_id, at)
    }
}

/// The asset registry MDL ships: every row in memory, one timeline per
/// family and native id. A lookup is two map lookups and a binary search.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Bindings {
    timelines: BTreeMap<String, BTreeMap<NativeId, Vec<Change>>>,
}

/// One point on a timeline, with the row it came from for diagnostics.
#[derive(Debug, Clone, PartialEq)]
struct Change {
    from: Timestamp,
    binding: Option<Arc<Binding>>,
    row: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum BindingsError {
    /// The text isn't TOML; the message has the line and column.
    #[error("the bindings file isn't valid TOML: {0}")]
    Syntax(String),
    /// The file has no supported `mdl_version`, or its shape is wrong.
    #[error(transparent)]
    Read(#[from] ReadError),
    #[error("the bindings have problems:\n{0}")]
    Invalid(Diagnostics),
}

impl BindingsError {
    /// The code for a whole-file rejection (`spec/bindings.md`, §7); `None`
    /// for [`Invalid`](BindingsError::Invalid), whose diagnostics carry
    /// their own codes.
    pub fn code(&self) -> Option<&'static str> {
        match self {
            BindingsError::Syntax(_) => Some("invalid_toml"),
            BindingsError::Read(err) => Some(err.code()),
            BindingsError::Invalid(_) => None,
        }
    }
}

impl Bindings {
    /// Bindings from rows, however a registry stores them. Diagnostic
    /// paths name rows by their index: `/binding/3/asset`.
    pub fn new(rows: Vec<Row>) -> Result<Bindings, Diagnostics> {
        let mut report = Report::default();
        for (index, row) in rows.iter().enumerate() {
            let binding = row.binding.as_ref();
            check_fields(
                &row_place(index, &row.family, &row.native_id),
                &row.family,
                &row.native_id,
                binding.map(|b| b.asset.as_str()),
                binding.and_then(|b| b.set.as_deref()),
                &mut report,
            );
        }
        let bindings = Bindings::collect(rows.into_iter().enumerate().collect(), &mut report);
        report.finish(bindings)
    }

    /// Bindings from a bindings file (`spec/bindings.md`).
    pub fn from_toml(text: &str) -> Result<Bindings, BindingsError> {
        let table: toml::Table =
            toml::from_str(text).map_err(|err| BindingsError::Syntax(err.to_string()))?;
        // The version first, like an IR document: a newer file is "upgrade
        // to read it", whatever else it contains.
        check_version(&table)?;
        let value = table_to_json(&table, "")?;
        let file: File = read::typed(&value)?;
        let mut report = Report::default();
        let mut rows = Vec::new();
        for (index, row) in file.binding.into_iter().enumerate() {
            if let Some(row) = row.into_row(index, &mut report) {
                rows.push((index, row));
            }
        }
        let bindings = Bindings::collect(rows, &mut report);
        report.finish(bindings).map_err(BindingsError::Invalid)
    }

    /// Check the bindings against a build: each family exists, each native
    /// id is one its family's alias locator can produce, and each binding
    /// names a set of its family exactly when the family has no version
    /// locator. For tools and CI; decoding doesn't need it.
    pub fn check(&self, validated: &Validated) -> Result<(), Diagnostics> {
        let bundle = validated.bundle();
        let mut report = Report::default();
        for (family_name, ids) in &self.timelines {
            let family = bundle.families.iter().find(|f| &f.name == family_name);
            for (native_id, changes) in ids {
                for change in changes {
                    let place = row_place(change.row, family_name, native_id);
                    let Some(family) = family else {
                        report.error(
                            Code::UnknownFamily,
                            &place.key("family"),
                            format!("the definitions have no family {family_name}"),
                        );
                        continue;
                    };
                    if let Some(problem) = alias_problem(&family.alias, native_id) {
                        report.error(
                            Code::NativeId,
                            &place.key("native_id"),
                            format!("family {family_name} never reads {native_id}: {problem}"),
                        );
                    }
                    let Some(binding) = &change.binding else {
                        continue;
                    };
                    match (&family.version, &binding.set) {
                        (Some(_), Some(_)) => report.error(
                            Code::BindingSet,
                            &place.key("set"),
                            format!(
                                "family {family_name} reads a layout version from each \
                                 message, which picks the set; its bindings name no set"
                            ),
                        ),
                        (None, None) => report.error(
                            Code::BindingSet,
                            &place,
                            format!(
                                "family {family_name} has no version locator, so each binding \
                                 names the set that decodes the asset's messages"
                            ),
                        ),
                        (None, Some(set)) => {
                            let known = bundle
                                .sets
                                .iter()
                                .any(|s| &s.name == set && &s.family == family_name);
                            if !known {
                                report.error(
                                    Code::UnknownSet,
                                    &place.key("set"),
                                    format!("family {family_name} has no set {set}"),
                                );
                            }
                        }
                        (Some(_), None) => {}
                    }
                }
            }
        }
        report.finish(())
    }

    /// Rows into timelines. Of rows sharing a family, native id and
    /// instant, every one but the first in row order is a duplicate.
    fn collect(rows: Vec<(usize, Row)>, report: &mut Report) -> Bindings {
        let mut timelines: BTreeMap<String, BTreeMap<NativeId, Vec<Change>>> = BTreeMap::new();
        for (index, row) in rows {
            let change = Change {
                from: row.effective_from,
                binding: row.binding.map(Arc::new),
                row: index,
            };
            timelines
                .entry(row.family)
                .or_default()
                .entry(row.native_id)
                .or_default()
                .push(change);
        }
        for (family, ids) in &mut timelines {
            for (native_id, changes) in ids.iter_mut() {
                changes.sort_by_key(|change| (change.from, change.row));
                for pair in changes.windows(2) {
                    if pair[0].from == pair[1].from {
                        report.error(
                            Code::DuplicateBinding,
                            &row_place(pair[1].row, family, native_id).key("effective_from"),
                            format!(
                                "binding {} already says what {native_id} is from {}; one row \
                                 per native id and instant",
                                pair[0].row, pair[0].from
                            ),
                        );
                    }
                }
            }
        }
        Bindings { timelines }
    }
}

impl Resolver for Bindings {
    fn resolve(&self, family: &str, native_id: &NativeId, at: Timestamp) -> Option<Arc<Binding>> {
        let changes = self.timelines.get(family)?.get(native_id)?;
        // The changes are sorted, so this is a binary search for the last
        // one in effect at `at`.
        let in_effect = changes.partition_point(|change| change.from <= at);
        changes.get(in_effect.checked_sub(1)?)?.binding.clone()
    }
}

fn row_place(index: usize, family: &str, native_id: &NativeId) -> Place {
    Place::root("").item(
        "binding",
        index,
        "binding",
        &format!("{index} ({family} {native_id})"),
    )
}

/// The checks a row's own values need, built or not.
fn check_fields(
    place: &Place,
    family: &str,
    native_id: &NativeId,
    asset: Option<&str>,
    set: Option<&str>,
    report: &mut Report,
) {
    if !is_token(family) {
        report.error(
            Code::InvalidName,
            &place.key("family"),
            format!(
                "{family:?} isn't a family name: lowercase letters, digits and _, starting with \
                 a letter"
            ),
        );
    }
    if native_id.as_text() == Some("") {
        report.error(
            Code::Empty,
            &place.key("native_id"),
            "native_id must not be empty",
        );
    }
    if let Some(asset) = asset {
        check_not_empty(asset, "asset", &place.key("asset"), report);
    }
    if let Some(set) = set
        && !is_token(set)
    {
        report.error(
            Code::InvalidName,
            &place.key("set"),
            format!(
                "{set:?} isn't a set name: lowercase letters, digits and _, starting with a \
                 letter"
            ),
        );
    }
}

/// Why `locator` can never read `native_id`, if it can't.
fn alias_problem(locator: &Locator, native_id: &NativeId) -> Option<String> {
    match locator {
        Locator::Bits { size_bits, .. } => {
            let max = if *size_bits >= 64 {
                LARGEST_ID
            } else {
                (1_i128 << size_bits) - 1
            };
            let fits = native_id.as_int().is_some_and(|n| (0..=max).contains(&n));
            (!fits).then(|| format!("its alias is a {size_bits}-bit integer, 0 to {max}"))
        }
        Locator::Fixed { value } => (NativeId::from_text(value.as_str()) != *native_id)
            .then(|| format!("its alias is always {value}")),
        Locator::Metadata { .. } | Locator::Path { .. } => None,
    }
}

/// A bindings file, as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    /// Checked before typing.
    #[serde(rename = "mdl_version")]
    _version: IgnoredAny,
    #[serde(default)]
    binding: Vec<FileRow>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRow {
    family: String,
    native_id: Scalar,
    effective_from: String,
    asset: Option<String>,
    set: Option<String>,
    retired: Option<bool>,
}

impl FileRow {
    /// The row, or `None` if its instant is invalid or its meaning unclear
    /// (no asset, or retired yet naming an asset or set). Every problem is
    /// reported either way, so a dropped row still has all of its own.
    fn into_row(self, index: usize, report: &mut Report) -> Option<Row> {
        let native_id = NativeId::from(&self.native_id);
        let place = row_place(index, &self.family, &native_id);
        let id_place = place.key("native_id");
        check_scalar(&self.native_id, "native_id", &id_place, report);
        check_integer_spelling(&self.native_id, "native_id", &id_place, report);
        check_fields(
            &place,
            &self.family,
            &native_id,
            self.asset.as_deref(),
            self.set.as_deref(),
            report,
        );

        let effective_from = self.effective_from.parse::<Timestamp>();
        if let Err(err) = &effective_from {
            report.error(
                Code::InvalidInstant,
                &place.key("effective_from"),
                err.to_string(),
            );
        }
        if self.retired == Some(false) {
            report.error(
                Code::ExplicitDefault,
                &place.key("retired"),
                "retired = false is the default; leave it out",
            );
        }

        let binding = match (self.retired == Some(true), self.asset, self.set) {
            (false, Some(asset), set) => Some(Binding { asset, set }),
            (true, None, None) => None,
            (true, asset, set) => {
                if asset.is_some() {
                    report.error(
                        Code::BindingShape,
                        &place.key("asset"),
                        "a retired row names no asset: the id is no asset from then on",
                    );
                }
                if set.is_some() {
                    report.error(
                        Code::BindingShape,
                        &place.key("set"),
                        "a retired row names no set",
                    );
                }
                return None;
            }
            (false, None, _) => {
                report.error(
                    Code::BindingShape,
                    &place,
                    "a row names an asset, or ends the id's binding with retired = true",
                );
                return None;
            }
        };

        Some(Row {
            family: self.family,
            native_id,
            effective_from: effective_from.ok()?,
            binding,
        })
    }
}

fn check_version(table: &toml::Table) -> Result<(), ReadError> {
    match table.get("mdl_version") {
        None => Err(ReadError::MissingVersion),
        Some(toml::Value::String(text)) => read::check_version_text(text),
        Some(other) => Err(ReadError::BadVersion {
            message: format!(
                "expected a string like \"2.0\", got a TOML {}",
                other.type_str()
            ),
        }),
    }
}

/// TOML as JSON, so the file is typed the way IR documents are and its
/// errors carry JSON Pointers. Bindings have no fractional numbers, and
/// instants are quoted strings: TOML's own datetimes allow many spellings
/// of one instant. The TOML parser limits nesting, which bounds this walk.
fn table_to_json(table: &toml::Table, path: &str) -> Result<Value, ReadError> {
    let mut object = serde_json::Map::new();
    for (key, value) in table {
        let child = format!("{path}/{}", escape_pointer_token(key));
        object.insert(key.clone(), to_json(value, &child)?);
    }
    Ok(Value::Object(object))
}

fn to_json(value: &toml::Value, path: &str) -> Result<Value, ReadError> {
    let shape = |message: &str| ReadError::Shape {
        path: path.to_string(),
        message: message.to_string(),
    };
    Ok(match value {
        toml::Value::String(text) => Value::String(text.clone()),
        toml::Value::Integer(n) => Value::from(*n),
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Float(_) => {
            return Err(shape(
                "a fractional number; bindings use only integers and strings",
            ));
        }
        toml::Value::Datetime(_) => {
            return Err(shape(
                "write instants as quoted strings, like \"2026-06-01T00:00:00Z\"",
            ));
        }
        toml::Value::Array(items) => {
            let mut array = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                array.push(to_json(item, &format!("{path}/{index}"))?);
            }
            Value::Array(array)
        }
        toml::Value::Table(table) => table_to_json(table, path)?,
    })
}

#[cfg(test)]
mod tests;
