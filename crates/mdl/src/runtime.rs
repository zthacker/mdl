//! The runtime API every binding calls (`spec/api.md`): open a store,
//! decode messages, encode commands, with results whose shape and values
//! the spec fixes.
//!
//! A [`Runtime`] is an opened store, with bindings or without; it's
//! immutable, cheap to clone and safe to share across threads. Its results
//! are typed for Rust callers, and each has a `to_value` that gives the
//! spec's object as a [`Value`]: a binding turns that into its language's
//! values, and [`Value`]'s `Serialize` writes its JSON, so the shape is
//! defined once, here.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};
use thiserror::Error;

use crate::artifact::AssembleError;
use crate::bindings::{Binding, Bindings, BindingsError, NativeId, Resolver};
use crate::decode::{
    DecodeError, Decoded, Engineering, EventTime, Quality, Record, SampleValue, Value as Raw,
};
use crate::diagnostics::{Diagnostic, Diagnostics};
use crate::encode::{CommandRequest, EncodeError, Encoded};
use crate::ir::{Artifact, Bundle, Digest};
use crate::message::{Message, MetaValue};
use crate::read::{Loaded, read_artifact};
use crate::route::FamilySets;
use crate::store::{self, StoreError, Stored};
use crate::time::Timestamp;
use crate::validate::{Validated, validate};

/// The version of the result format (`spec/api.md`), which
/// `spec/compatibility.md` §4 says what changes.
pub const FORMAT: u32 = 1;

/// A value of a result (`spec/api.md` §1). `Serialize` writes its JSON:
/// integers past ±(2^53 − 1) as decimal strings, NaN and the infinities as
/// `{"non_finite": ...}`, bytes as lowercase hex, instants as RFC 3339.
///
/// Not `#[non_exhaustive]`: a binding bundles one runtime release, and a
/// new value form (for a new IR feature) should stop its conversion
/// compiling rather than reach a wildcard.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Integer(i128),
    Float(f64),
    Bool(bool),
    Text(String),
    Bytes(Vec<u8>),
    Instant(Timestamp),
    Absent,
    List(Vec<Value>),
    /// Keys in the order `spec/api.md` lists them.
    Object(Vec<(String, Value)>),
}

/// The largest integer every JSON reader keeps exact.
const MAX_SAFE_INTEGER: u128 = (1 << 53) - 1;

impl Value {
    /// The instant as a binding returns it: nanoseconds since
    /// 1970-01-01T00:00:00Z, every day counted as 86,400 seconds.
    pub fn unix_nanos(at: Timestamp) -> i128 {
        i128::from(at.seconds()) * 1_000_000_000 + i128::from(at.nanos())
    }

    fn object<const N: usize>(entries: [(&str, Value); N]) -> Value {
        Value::Object(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }

    fn text(text: impl Into<String>) -> Value {
        Value::Text(text.into())
    }

    fn or_absent<T>(value: Option<T>, present: impl FnOnce(T) -> Value) -> Value {
        value.map_or(Value::Absent, present)
    }
}

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Value::Integer(n) if n.unsigned_abs() <= MAX_SAFE_INTEGER => match i64::try_from(*n) {
                Ok(n) => serializer.serialize_i64(n),
                Err(_) => serializer.serialize_str(&n.to_string()),
            },
            Value::Integer(n) => serializer.serialize_str(&n.to_string()),
            Value::Float(x) if x.is_finite() => serializer.serialize_f64(*x),
            Value::Float(x) => {
                let name = if x.is_nan() {
                    "NaN"
                } else if *x > 0.0 {
                    "inf"
                } else {
                    "-inf"
                };
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("non_finite", name)?;
                map.end()
            }
            Value::Bool(b) => serializer.serialize_bool(*b),
            Value::Text(text) => serializer.serialize_str(text),
            Value::Bytes(bytes) => serializer.serialize_str(&hex(bytes)),
            Value::Instant(at) => serializer.serialize_str(&at.to_string()),
            Value::Absent => serializer.serialize_none(),
            Value::List(values) => {
                let mut seq = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    seq.serialize_element(value)?;
                }
                seq.end()
            }
            Value::Object(entries) => {
                let mut map = serializer.serialize_map(Some(entries.len()))?;
                for (key, value) in entries {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// An enum value's IR spelling (`ECEF`, `ms`).
fn spelled<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// One of the problems an error or a warning stands for: its own code,
/// its message, and where it is, as a JSON Pointer into the build or the
/// bindings (empty for the whole of either).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Problem {
    pub code: &'static str,
    pub message: String,
    pub path: String,
}

impl Problem {
    fn of(diagnostic: &Diagnostic) -> Problem {
        let message = if diagnostic.context.is_empty() {
            diagnostic.message.clone()
        } else {
            format!("{}: {}", diagnostic.context, diagnostic.message)
        };
        Problem {
            code: diagnostic.code.as_str(),
            message,
            path: diagnostic.path.clone(),
        }
    }

    pub fn to_value(&self) -> Value {
        Value::object([
            ("code", Value::text(self.code)),
            ("message", Value::text(&self.message)),
            ("path", Value::text(&self.path)),
        ])
    }
}

/// Why an operation failed (`spec/api.md` §4): a stable code, a message
/// for people, and, for an error that stands for several problems (a build
/// that doesn't validate), each of them.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
#[non_exhaustive]
pub struct ApiError {
    pub code: &'static str,
    pub message: String,
    pub problems: Vec<Problem>,
}

impl ApiError {
    fn new(code: &'static str, message: impl Into<String>) -> ApiError {
        ApiError {
            code,
            message: message.into(),
            problems: Vec::new(),
        }
    }

    /// Several problems as one error under `code`: `summary`, then each
    /// problem's message, a line each.
    fn of_diagnostics(code: &'static str, summary: &str, diagnostics: &Diagnostics) -> ApiError {
        let problems: Vec<Problem> = diagnostics.iter().map(Problem::of).collect();
        let mut message = summary.to_string();
        for problem in &problems {
            message.push('\n');
            message.push_str(&format!("{} [{}]", problem.message, problem.code));
        }
        ApiError {
            code,
            message,
            problems,
        }
    }

    pub fn to_value(&self) -> Value {
        Value::object([
            ("code", Value::text(self.code)),
            ("message", Value::text(&self.message)),
            (
                "problems",
                Value::List(self.problems.iter().map(Problem::to_value).collect()),
            ),
        ])
    }
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> ApiError {
        match &error {
            StoreError::Assemble {
                error: assemble, ..
            } => {
                let mut api = ApiError::from(assemble.clone());
                api.message = error.to_string();
                api
            }
            other => ApiError::new(other.code(), other.to_string()),
        }
    }
}

impl From<AssembleError> for ApiError {
    fn from(error: AssembleError) -> ApiError {
        let problems = match &error {
            AssembleError::Invalid(diagnostics)
            | AssembleError::IncompatibleCatalog {
                problems: diagnostics,
                ..
            } => diagnostics.iter().map(Problem::of).collect(),
            _ => Vec::new(),
        };
        ApiError {
            code: error.code(),
            message: error.to_string(),
            problems,
        }
    }
}

impl From<DecodeError> for ApiError {
    fn from(error: DecodeError) -> ApiError {
        ApiError::new(error.code(), error.to_string())
    }
}

impl From<EncodeError> for ApiError {
    fn from(error: EncodeError) -> ApiError {
        ApiError::new(error.code(), error.to_string())
    }
}

impl From<BindingsError> for ApiError {
    fn from(error: BindingsError) -> ApiError {
        match &error {
            BindingsError::Invalid(diagnostics) => ApiError::of_diagnostics(
                "invalid_bindings",
                "the bindings have problems",
                diagnostics,
            ),
            other => ApiError::new(
                other.code().unwrap_or("invalid_bindings"),
                other.to_string(),
            ),
        }
    }
}

/// An opened store (`spec/api.md` §2.1): every family's sets, ready to
/// decode and encode, and the bindings it was opened with. Cloning it, or
/// giving it other bindings ([`Runtime::with_bindings`]), shares the
/// assembled build.
#[derive(Debug, Clone)]
pub struct Runtime {
    build: Arc<Build>,
    bindings: Option<Arc<Bindings>>,
    /// What the bindings say that the build doesn't have.
    warnings: Arc<Vec<Problem>>,
}

/// A store's build, assembled to decode and encode.
#[derive(Debug)]
struct Build {
    validated: Validated,
    families: BTreeMap<String, Assembled>,
}

/// One family's sets, and the stand-in its messages are decoded with when
/// there are no bindings, or why there can't be one.
#[derive(Debug)]
struct Assembled {
    sets: FamilySets,
    stand_in: Result<StandIn, ApiError>,
}

impl Runtime {
    /// The store in `dir`, with the bindings file's text if any.
    pub fn open(dir: &Path, bindings: Option<&str>) -> Result<Runtime, ApiError> {
        let stored = store::load(dir)?;
        Runtime::new(&stored, read_bindings(bindings)?)
    }

    /// A store from its files' bytes: the manifest, and each artifact,
    /// found by its own identity (`store::load_bytes`).
    pub fn open_bytes(
        manifest: &[u8],
        artifacts: &[&[u8]],
        bindings: Option<&str>,
    ) -> Result<Runtime, ApiError> {
        let stored = store::load_bytes(manifest, artifacts)?;
        Runtime::new(&stored, read_bindings(bindings)?)
    }

    /// A build already read: every family assembled with its sets and the
    /// catalogs they pin, with the bindings given.
    pub fn new(stored: &Stored, bindings: Option<Bindings>) -> Result<Runtime, ApiError> {
        let validated = validated(stored)?;
        let load = |canonical: &str| -> Result<Loaded<Artifact>, ApiError> {
            read_artifact(canonical.as_bytes())
                .map_err(|error| ApiError::new(error.code(), error.to_string()))
        };
        let catalogs = stored.catalogs().map(load).collect::<Result<Vec<_>, _>>()?;
        let mut families = BTreeMap::new();
        for (name, written) in &stored.artifacts.families {
            let family = load(&written.canonical)?;
            let mut sets = Vec::new();
            for written in stored.artifacts.sets.values() {
                if let Artifact::Set { set, .. } = &written.artifact
                    && set.family == *name
                {
                    sets.push(load(&written.canonical)?);
                }
            }
            let sets = FamilySets::new(&family, &sets, &catalogs)?;
            let stand_in = StandIn::of(&sets);
            families.insert(name.clone(), Assembled { sets, stand_in });
        }
        let build = Arc::new(Build {
            validated,
            families,
        });
        Ok(Runtime::with(build, bindings))
    }

    /// The same build with other bindings, or none: the build is shared,
    /// not assembled again, so a bindings change is cheap.
    pub fn with_bindings(&self, bindings: Option<Bindings>) -> Runtime {
        Runtime::with(Arc::clone(&self.build), bindings)
    }

    /// `build` with `bindings`, and what they say that the build doesn't
    /// have as warnings (`spec/api.md` §2.1). Rows for families the build
    /// doesn't have aren't warned about: bindings are a fleet's, a store
    /// may carry some of its families.
    fn with(build: Arc<Build>, bindings: Option<Bindings>) -> Runtime {
        let warnings = match bindings.as_ref().map(|b| b.check(&build.validated)) {
            Some(Err(diagnostics)) => diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code.as_str() != "unknown_family")
                .map(Problem::of)
                .collect(),
            _ => Vec::new(),
        };
        Runtime {
            build,
            bindings: bindings.map(Arc::new),
            warnings: Arc::new(warnings),
        }
    }

    /// What the bindings say that the build doesn't have: a set it lacks, a
    /// set named for a family whose version locator chooses. The messages
    /// these affect are refused when decoded, with their own codes.
    pub fn warnings(&self) -> &[Problem] {
        &self.warnings
    }

    /// The families' names.
    pub fn families(&self) -> impl Iterator<Item = &str> {
        self.build.families.keys().map(String::as_str)
    }

    /// The family's sets, assembled.
    pub fn family(&self, name: &str) -> Result<&FamilySets, ApiError> {
        self.assembled(name).map(|assembled| &assembled.sets)
    }

    fn assembled(&self, name: &str) -> Result<&Assembled, ApiError> {
        self.build
            .families
            .get(name)
            .ok_or_else(|| ApiError::new("unknown_family", format!("there's no family {name}")))
    }

    /// The message `family` sent, decoded (`spec/api.md` §2.2).
    pub fn decode(&self, family: &str, message: &Message<'_>) -> Result<DecodedMessage, ApiError> {
        let assembled = self.assembled(family)?;
        let sets = &assembled.sets;
        let decoded = match &self.bindings {
            Some(bindings) => {
                let decoded = sets.decode(message, bindings.as_ref())?;
                DecodedMessage::of(&decoded, sets, message.ground_time, false)
            }
            None => {
                let stand_in = assembled.stand_in.as_ref().map_err(Clone::clone)?;
                let decoded = sets.decode(message, stand_in)?;
                DecodedMessage::of(&decoded, sets, message.ground_time, true)
            }
        };
        Ok(decoded)
    }

    /// Each message, decoded or why not, in order (`spec/api.md` §2.3).
    pub fn decode_many(
        &self,
        family: &str,
        messages: &[Message<'_>],
    ) -> Vec<Result<DecodedMessage, ApiError>> {
        messages
            .iter()
            .map(|message| self.decode(family, message))
            .collect()
    }

    /// The command, encoded by `family`'s set (`spec/api.md` §2.4).
    pub fn encode(
        &self,
        family: &str,
        request: &CommandRequest,
    ) -> Result<EncodedCommand, ApiError> {
        let sets = self.family(family)?;
        let encoded = sets.encode(request)?;
        Ok(EncodedCommand::of(&encoded, family, &request.command))
    }
}

/// The bindings file's text, read.
fn read_bindings(text: Option<&str>) -> Result<Option<Bindings>, ApiError> {
    text.map(Bindings::from_toml)
        .transpose()
        .map_err(ApiError::from)
}

/// The store's build, validated as one bundle: the manifest's catalog,
/// every family and every set.
fn validated(stored: &Stored) -> Result<Validated, ApiError> {
    let artifacts = &stored.artifacts;
    let Artifact::Catalog { catalog, .. } = &artifacts.catalog.artifact else {
        return Err(ApiError::new(
            "wrong_artifact",
            "the store's catalog artifact isn't a catalog",
        ));
    };
    let mut bundle = Bundle::new(catalog.clone());
    for written in artifacts.families.values() {
        if let Artifact::Family { family, .. } = &written.artifact {
            bundle.families.push(family.clone());
        }
    }
    for written in artifacts.sets.values() {
        if let Artifact::Set { set, .. } = &written.artifact {
            bundle.sets.push(set.clone());
        }
    }
    validate(bundle).map_err(|diagnostics| {
        ApiError::of_diagnostics(
            "invalid_build",
            "the store's build doesn't validate",
            &diagnostics,
        )
    })
}

/// Without bindings, each native id is an asset of its own (`spec/api.md`
/// §2.2), decoded by the family's one set, or by the set its version
/// locator chooses.
#[derive(Debug)]
struct StandIn {
    set: Option<String>,
}

impl StandIn {
    fn of(sets: &FamilySets) -> Result<StandIn, ApiError> {
        if sets.versioned() {
            return Ok(StandIn { set: None });
        }
        let names: Vec<&str> = sets.sets().map(|set| set.name.as_str()).collect();
        match names.as_slice() {
            [only] => Ok(StandIn {
                set: Some((*only).to_string()),
            }),
            _ => Err(ApiError::new(
                "missing_set",
                format!(
                    "family {} has {} sets and no version locator, so each asset's binding names \
                     its set; open the store with bindings",
                    sets.family().name,
                    names.len()
                ),
            )),
        }
    }
}

impl Resolver for StandIn {
    fn resolve(&self, _family: &str, native_id: &NativeId, _at: Timestamp) -> Option<Arc<Binding>> {
        let binding = Binding::new(native_id.to_string());
        Some(Arc::new(match &self.set {
            Some(set) => binding.with_set(set),
            None => binding,
        }))
    }
}

/// A decoded message (`spec/api.md` §3.1).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct DecodedMessage {
    pub family: String,
    /// What the family's alias locator read.
    pub native_id: NativeId,
    pub asset: String,
    /// The asset stands for a native id with no bindings to name it.
    pub stand_in: bool,
    pub set: String,
    pub def_version: Digest,
    pub packet: String,
    pub event_time: EventTime,
    /// The message's ground time, which an event time of `ground` is.
    pub ground_time: Timestamp,
    pub samples: Vec<Sample>,
    /// Every present entry's raw value, as an object by entry name.
    pub record: Value,
    /// Each field's and scalar element's engineering value, by path, in
    /// the record's order.
    pub engineering: Vec<(String, Engineering)>,
}

/// One catalog value of a decoded message (`spec/api.md` §3.1).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Sample {
    pub param: String,
    pub value: SampleValue,
    /// The param's unit.
    pub unit: Option<String>,
    pub path: String,
    pub index: Option<u64>,
    pub time_offset_ns: Option<i64>,
    pub quality: Quality,
}

impl DecodedMessage {
    /// What `sets` decoded, as the API gives it: the ground time the
    /// message carried, and whether its asset is a stand-in. For callers
    /// that route with resolvers of their own (morphc's stand-ins for a
    /// chosen set) and give the same results.
    pub fn of(
        decoded: &Decoded<'_>,
        sets: &FamilySets,
        ground_time: Timestamp,
        stand_in: bool,
    ) -> DecodedMessage {
        let set = &decoded.route.set.name;
        let catalog = sets.catalog(set);
        let samples = decoded
            .samples
            .iter()
            .map(|sample| Sample {
                param: sample.param.to_string(),
                value: sample.value.clone(),
                unit: catalog
                    .and_then(|catalog| catalog.params.get(sample.param))
                    .and_then(|param| param.unit.clone()),
                path: sample.path.clone(),
                index: sample.index,
                time_offset_ns: sample.time_offset_ns,
                quality: sample.quality,
            })
            .collect();
        DecodedMessage {
            family: sets.family().name.clone(),
            native_id: decoded.route.native_id.clone(),
            asset: decoded.route.binding.asset.clone(),
            stand_in,
            set: set.clone(),
            def_version: decoded.route.def_version.clone(),
            packet: decoded.packet.name.clone(),
            event_time: decoded.event_time,
            ground_time,
            samples,
            record: record_value(&decoded.record),
            engineering: decoded.engineering_values(),
        }
    }

    pub fn to_value(&self) -> Value {
        Value::object([
            ("family", Value::text(&self.family)),
            ("native_id", native_id_value(&self.native_id)),
            ("asset", Value::text(&self.asset)),
            ("stand_in", Value::Bool(self.stand_in)),
            ("set", Value::text(&self.set)),
            ("def_version", Value::text(self.def_version.to_string())),
            ("packet", Value::text(&self.packet)),
            (
                "event_time",
                event_time_value(&self.event_time, self.ground_time),
            ),
            (
                "samples",
                Value::List(self.samples.iter().map(Sample::to_value).collect()),
            ),
            ("record", self.record.clone()),
            (
                "engineering",
                Value::List(
                    self.engineering
                        .iter()
                        .map(|(path, engineering)| {
                            Value::object([
                                ("path", Value::text(path)),
                                ("value", sample_value(&engineering.value)),
                                ("quality", Value::text(engineering.quality.as_str())),
                            ])
                        })
                        .collect(),
                ),
            ),
        ])
    }
}

impl Sample {
    pub fn to_value(&self) -> Value {
        Value::object([
            ("param", Value::text(&self.param)),
            ("value", sample_value(&self.value)),
            ("unit", Value::or_absent(self.unit.as_ref(), Value::text)),
            ("path", Value::text(&self.path)),
            (
                "index",
                Value::or_absent(self.index, |n| Value::Integer(i128::from(n))),
            ),
            (
                "time_offset_ns",
                Value::or_absent(self.time_offset_ns, |n| Value::Integer(i128::from(n))),
            ),
            ("quality", Value::text(self.quality.as_str())),
        ])
    }
}

fn native_id_value(native_id: &NativeId) -> Value {
    match native_id.as_int() {
        Some(n) => Value::Integer(n),
        None => Value::text(native_id.to_string()),
    }
}

fn event_time_value(time: &EventTime, ground: Timestamp) -> Value {
    match time {
        EventTime::Ground => Value::object([
            ("kind", Value::text("ground")),
            ("time", Value::Instant(ground)),
        ]),
        EventTime::At(at) => {
            Value::object([("kind", Value::text("at")), ("time", Value::Instant(*at))])
        }
        EventTime::SinceBoot { value, unit } => Value::object([
            ("kind", Value::text("since_boot")),
            ("value", Value::Integer(i128::from(*value))),
            ("unit", Value::text(spelled(unit))),
        ]),
    }
}

fn sample_value(value: &SampleValue) -> Value {
    match value {
        SampleValue::F64(x) => Value::Float(*x),
        SampleValue::I64(n) => Value::Integer(i128::from(*n)),
        SampleValue::U64(n) => Value::Integer(i128::from(*n)),
        SampleValue::Bool(b) => Value::Bool(*b),
        SampleValue::Str(text) | SampleValue::Enum(text) => Value::text(text),
        SampleValue::Bytes(bytes) => Value::Bytes(bytes.clone()),
        SampleValue::Vec3 { frame, x, y, z } => Value::object([
            ("frame", Value::text(spelled(frame))),
            ("x", Value::Float(*x)),
            ("y", Value::Float(*y)),
            ("z", Value::Float(*z)),
        ]),
        SampleValue::Quat { frame, w, x, y, z } => Value::object([
            ("frame", Value::text(spelled(frame))),
            ("w", Value::Float(*w)),
            ("x", Value::Float(*x)),
            ("y", Value::Float(*y)),
            ("z", Value::Float(*z)),
        ]),
    }
}

/// A record as an object of its entries' raw values (`spec/api.md` §3.1).
fn record_value(record: &Record<'_>) -> Value {
    Value::Object(
        record
            .iter()
            .map(|(name, value)| (name.to_string(), raw_value(value)))
            .collect(),
    )
}

fn raw_value(value: &Raw<'_>) -> Value {
    match value {
        Raw::Uint(n) => Value::Integer(i128::from(*n)),
        Raw::Int(n) => Value::Integer(i128::from(*n)),
        Raw::Float(x) => Value::Float(*x),
        Raw::Bool(b) => Value::Bool(*b),
        Raw::Str(text) => Value::text(text),
        Raw::Bytes(bytes) => Value::Bytes(bytes.clone()),
        Raw::Array(values) => Value::List(values.iter().map(raw_value).collect()),
        Raw::Record(record) => record_value(record),
        Raw::Null => Value::Absent,
    }
}

impl Serialize for DecodedMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(serializer)
    }
}

/// An encoded command (`spec/api.md` §3.2).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct EncodedCommand {
    pub family: String,
    pub set: String,
    pub def_version: Digest,
    pub packet: String,
    pub command: String,
    pub payload: Vec<u8>,
    /// The values the link frames the payload with, by name: integers or
    /// text.
    pub metadata: Vec<(String, Value)>,
    pub defaulted: Vec<String>,
    pub deprecated: Option<String>,
    /// Each field's and checksum's raw value, as an object by entry name.
    pub record: Value,
}

impl EncodedCommand {
    /// What `family`'s set encoded for `command`, as the API gives it.
    pub fn of(encoded: &Encoded<'_>, family: &str, command: &str) -> EncodedCommand {
        let metadata = encoded
            .metadata()
            .into_iter()
            .map(|(name, value)| {
                let value = match value {
                    MetaValue::Uint(n) => Value::Integer(i128::from(n)),
                    MetaValue::Int(n) => Value::Integer(i128::from(n)),
                    MetaValue::Text(text) => Value::text(text),
                };
                (name.to_string(), value)
            })
            .collect();
        EncodedCommand {
            family: family.to_string(),
            set: encoded.set.name.clone(),
            def_version: encoded.def_version.clone(),
            packet: encoded.packet.name.clone(),
            command: command.to_string(),
            payload: encoded.payload.clone(),
            metadata,
            defaulted: encoded.defaulted.iter().map(|s| (*s).to_string()).collect(),
            deprecated: encoded.deprecated.map(str::to_string),
            record: record_value(&encoded.record),
        }
    }

    pub fn to_value(&self) -> Value {
        Value::object([
            ("family", Value::text(&self.family)),
            ("set", Value::text(&self.set)),
            ("def_version", Value::text(self.def_version.to_string())),
            ("packet", Value::text(&self.packet)),
            ("command", Value::text(&self.command)),
            ("payload", Value::Bytes(self.payload.clone())),
            ("metadata", Value::Object(self.metadata.clone())),
            (
                "defaulted",
                Value::List(self.defaulted.iter().map(Value::text).collect()),
            ),
            (
                "deprecated",
                Value::or_absent(self.deprecated.as_ref(), Value::text),
            ),
            ("record", self.record.clone()),
        ])
    }
}

impl Serialize for EncodedCommand {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(serializer)
    }
}

impl Serialize for ApiError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(serializer)
    }
}
