//! `morphc decode`: messages through definitions or a store, as a ground
//! system decodes them: routed to an asset and a set, identified, and
//! turned into catalog samples. Command payloads decode into the command
//! and arguments that encode them.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use clap::{ArgGroup, Args};
use mdl::v2::bindings::{Binding, Bindings, BindingsError, NativeId, Resolver};
use mdl::v2::decode::{Decoded, DecodedCommand, EventTime, Quality, Record, Value};
use mdl::v2::ir::{Bundle, Encoding};
use mdl::v2::message::{Message, MetaValue, Payload};
use mdl::v2::source::SourceDiagnostic;
use mdl::v2::time::Timestamp;
use serde::Serialize;
use serde_json::{Map, Value as Json};

use crate::describe::ir;
use crate::report::{Outcome, Report};
use crate::values::{
    Failure, argument_json, integer_json, parse_hex, record_json, sample_json, unsupported,
    value_json,
};
use crate::{Opened, Origin, ReadArgs, family_sets, open};

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("message")
        .required(true)
        .args(["hex", "payload", "lines", "records"])
))]
pub struct DecodeArgs {
    /// One binary payload in hex (`0a1b2c`; spaces, `_` and `:` between
    /// bytes and a leading `0x` are allowed).
    #[arg(long, value_name = "HEX")]
    hex: Option<String>,

    /// One payload from a file: its bytes, or a JSON document for a JSON
    /// family.
    #[arg(long, value_name = "FILE")]
    payload: Option<PathBuf>,

    /// One payload per line (`-` for stdin): hex for a binary family, a
    /// JSON document for a JSON one. Blank lines and lines starting with
    /// `#` are skipped. Every line is decoded and reported.
    #[arg(long, value_name = "FILE")]
    lines: Option<PathBuf>,

    /// One message per line as a JSON record (`-` for stdin):
    /// `{"hex": "0a1b", "meta": {"apid": 100}, "time": "2026-11-09T12:00:00Z"}`.
    /// A JSON family's payload is `"text"`, the document as sent, or
    /// `"json"`, the document as a value (written again, so not byte for
    /// byte). `--meta` and `--time` fill in what a record leaves out. Blank
    /// lines and lines starting with `#` are skipped. Every record is
    /// decoded and reported.
    #[arg(long, value_name = "FILE")]
    records: Option<PathBuf>,

    /// Metadata the link gives with every message, as NAME=VALUE (repeat
    /// for more). A value in decimal digits is an integer.
    #[arg(long = "meta", value_name = "NAME=VALUE", value_parser = parse_meta)]
    meta: Vec<Meta>,

    /// When the ground received the messages, as RFC 3339
    /// (`2026-11-09T12:00:00Z`). Bindings are resolved at this time.
    /// [default: now]
    #[arg(long, value_name = "INSTANT", value_parser = parse_time)]
    time: Option<Timestamp>,

    /// A bindings file (spec/bindings.md) naming the asset each native id
    /// is and the set that decodes it. Without one, each native id is an
    /// asset of the same name, decoded by the family's version locator,
    /// --set, or the family's only set.
    #[arg(long, value_name = "FILE")]
    bindings: Option<PathBuf>,

    /// The family the messages come from [default: --set's, or the
    /// build's only family]
    #[arg(long, value_name = "NAME")]
    family: Option<String>,

    /// The set to decode with, without --bindings, and for command
    /// payloads [default: the family's only set]
    #[arg(long, value_name = "NAME")]
    set: Option<String>,

    /// The messages are command payloads: decode which command each is,
    /// and its arguments.
    #[arg(long)]
    command: bool,

    /// Also show every entry's raw value, and each field's engineering
    /// value.
    #[arg(long)]
    raw: bool,

    // Last, since the XTCE options' heading applies to every argument
    // after them.
    #[command(flatten)]
    pub read: ReadArgs,
}

/// One `--meta` value, as the link gives it.
#[derive(Debug, Clone)]
struct Meta {
    name: String,
    value: MetaArg,
}

#[derive(Debug, Clone)]
enum MetaArg {
    Uint(u64),
    Int(i64),
    Text(String),
}

impl MetaArg {
    fn value(&self) -> MetaValue<'_> {
        match self {
            MetaArg::Uint(n) => MetaValue::Uint(*n),
            MetaArg::Int(n) => MetaValue::Int(*n),
            MetaArg::Text(text) => MetaValue::Text(text),
        }
    }
}

fn parse_meta(text: &str) -> Result<Meta, String> {
    let (name, value) = text
        .split_once('=')
        .filter(|(name, _)| !name.is_empty())
        .ok_or_else(|| format!("{text:?} isn't NAME=VALUE"))?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let value = if digits(value) {
        value
            .parse()
            .map(MetaArg::Uint)
            .map_err(|_| format!("{value} is too large for a metadata integer"))?
    } else if value.strip_prefix('-').is_some_and(digits) {
        value
            .parse()
            .map(MetaArg::Int)
            .map_err(|_| format!("{value} is too small for a metadata integer"))?
    } else {
        MetaArg::Text(value.to_string())
    };
    Ok(Meta {
        name: name.to_string(),
        value,
    })
}

fn parse_time(text: &str) -> Result<Timestamp, String> {
    Timestamp::from_rfc3339(text).map_err(|err| err.to_string())
}

pub fn run(args: &DecodeArgs) -> anyhow::Result<ExitCode> {
    let read = &args.read;
    let from_stdin = [&args.lines, &args.records]
        .into_iter()
        .any(|file| file.as_deref() == Some(Path::new("-")));
    if from_stdin && read.inputs.iter().any(|input| input.as_os_str() == "-") {
        bail!("the definitions and the messages can't both come from stdin");
    }
    if args.bindings.is_some() && args.set.is_some() && !args.command {
        bail!("with --bindings, each asset's binding names its set; leave out --set");
    }
    if args.bindings.is_some() && args.command {
        bail!("command payloads are decoded with --set, not --bindings");
    }
    let origin = Origin::of(read)?;
    let sources = origin.sources()?;
    let mut report = Report::new(&sources, read.format, read.all_warnings);
    let Some(opened) = open(&origin, read, &sources, &mut report)? else {
        return Ok(report.finish::<Decoding>(None));
    };
    let family = choose_family(&opened, args)?;
    let family_sets = family_sets(&opened, &family)?;
    let encoding = family_sets.family().encoding;
    let messages = messages(args, encoding)?;
    let mode = if args.command {
        Mode::Command(choose_set(&opened, &family, args.set.as_deref())?)
    } else {
        match &args.bindings {
            Some(path) => match read_bindings(path, &opened, &mut report)? {
                Some(bindings) => Mode::Telemetry(Assets::Bindings(bindings)),
                None => return Ok(report.finish::<Decoding>(None)),
            },
            None => Mode::Telemetry(Assets::StandIn(stand_in_set(
                &opened,
                &family,
                args.set.as_deref(),
            )?)),
        }
    };
    let now = now();

    let mut results = Vec::with_capacity(messages.len());
    for message in &messages {
        let result = match &message.payload {
            Err(failure) => Err(failure.clone()),
            Ok(bytes) => {
                // A record's metadata replaces --meta's of the same name.
                let given = |meta: &&Meta| !message.meta.iter().any(|m| m.name == meta.name);
                let metadata: Vec<(&str, MetaValue<'_>)> = args
                    .meta
                    .iter()
                    .filter(given)
                    .chain(&message.meta)
                    .map(|meta| (meta.name.as_str(), meta.value.value()))
                    .collect();
                let time = message.time.or(args.time).unwrap_or(now);
                let message_in =
                    Message::new(payload(encoding, bytes), time).with_metadata(&metadata);
                let failure = |code: &str, message: String| Failure {
                    code: code.to_string(),
                    message,
                };
                match &mode {
                    Mode::Command(set) => family_sets
                        .decode_command(set, &message_in)
                        .map(|decoded| Decodes::Command(command_result(&decoded)))
                        .map_err(|err| failure(err.code(), err.to_string())),
                    Mode::Telemetry(assets) => family_sets
                        .decode(&message_in, assets)
                        .map(|decoded| {
                            let stand_in = matches!(assets, Assets::StandIn(_));
                            Decodes::Telemetry(telemetry(
                                &decoded, &opened, time, args.raw, stand_in,
                            ))
                        })
                        .map_err(|err| failure(err.code(), err.to_string())),
                }
            }
        };
        results.push(MessageResult::of(message.line, result));
    }
    let decoding = Decoding {
        family,
        lines: args.lines.is_some() || args.records.is_some(),
        messages: results,
    };
    Ok(report.finish(Some(decoding)))
}

/// What the messages are decoded as.
enum Mode {
    /// Command payloads, with this set.
    Command(String),
    Telemetry(Assets),
}

fn now() -> Timestamp {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_nanos()).unwrap_or(i64::MAX)
        });
    Timestamp::from_unix_nanos(nanos)
}

fn payload(encoding: Encoding, bytes: &[u8]) -> Payload<'_> {
    match encoding {
        Encoding::Json => Payload::Json(bytes),
        _ => Payload::Binary(bytes),
    }
}

/// The family whose messages these are.
fn choose_family(opened: &Opened, args: &DecodeArgs) -> anyhow::Result<String> {
    let bundle = opened.validated.bundle();
    let names = |bundle: &Bundle| -> String {
        let names: Vec<&str> = bundle.families.iter().map(|f| f.name.as_str()).collect();
        names.join(", ")
    };
    if let Some(set) = &args.set {
        let Some(found) = bundle.sets.iter().find(|s| s.name == *set) else {
            let sets: Vec<&str> = bundle.sets.iter().map(|s| s.name.as_str()).collect();
            bail!("there's no set {set}; the sets are {}", sets.join(", "));
        };
        if let Some(family) = &args.family
            && *family != found.family
        {
            bail!("set {set} is of family {}, not {family}", found.family);
        }
        return Ok(found.family.clone());
    }
    match (&args.family, bundle.families.as_slice()) {
        (Some(family), families) => {
            if !families.iter().any(|f| f.name == *family) {
                bail!(
                    "there's no family {family}; the families are {}",
                    names(bundle)
                );
            }
            Ok(family.clone())
        }
        (None, [only]) => Ok(only.name.clone()),
        (None, _) => bail!(
            "the build has families {}; say which with --family or --set",
            names(bundle)
        ),
    }
}

/// The set named, or the family's only one.
fn choose_set(opened: &Opened, family: &str, named: Option<&str>) -> anyhow::Result<String> {
    if let Some(set) = named {
        return Ok(set.to_string());
    }
    let sets: Vec<&str> = opened
        .validated
        .bundle()
        .sets
        .iter()
        .filter(|s| s.family == family)
        .map(|s| s.name.as_str())
        .collect();
    match sets.as_slice() {
        [only] => Ok((*only).to_string()),
        _ => bail!(
            "family {family} has sets {}; say which with --set",
            sets.join(", ")
        ),
    }
}

/// The set a stand-in binding names: none when the family's version
/// locator chooses it.
fn stand_in_set(
    opened: &Opened,
    family: &str,
    named: Option<&str>,
) -> anyhow::Result<Option<String>> {
    let bundle = opened.validated.bundle();
    let versioned = bundle
        .families
        .iter()
        .any(|f| f.name == family && f.version.is_some());
    if versioned {
        if let Some(set) = named {
            bail!(
                "family {family} picks each message's set by its version, so --set {set} isn't used"
            );
        }
        return Ok(None);
    }
    choose_set(opened, family, named).map(Some)
}

/// The bindings file, checked against the build; none, with its problems
/// in `report`, when it has any.
fn read_bindings(
    path: &Path,
    opened: &Opened,
    report: &mut Report,
) -> anyhow::Result<Option<Bindings>> {
    let text =
        fs::read_to_string(path).with_context(|| format!("can't read {}", path.display()))?;
    // The file has no spans in these diagnostics: `context` names it, and
    // `path` points into it rather than into the bundle.
    let problem = |code: &'static str, message: String, pointer: &str| {
        let context = if pointer.is_empty() {
            path.display().to_string()
        } else {
            format!("{} at {pointer}", path.display())
        };
        SourceDiagnostic::error(code, message)
            .with_context(context)
            .with_path(pointer)
    };
    let bindings = match Bindings::from_toml(&text) {
        Ok(bindings) => bindings,
        Err(BindingsError::Invalid(diagnostics)) => {
            report.add(
                diagnostics
                    .iter()
                    .map(|d| problem(d.code.as_str(), d.message.clone(), &d.path)),
            );
            return Ok(None);
        }
        Err(BindingsError::Read(err)) => {
            report.add([problem(err.code(), err.detail(), err.path())]);
            return Ok(None);
        }
        Err(err) => {
            let code = err.code().unwrap_or("morphc.bindings");
            report.add([problem(code, err.to_string(), "")]);
            return Ok(None);
        }
    };
    if let Err(diagnostics) = bindings.check(&opened.validated) {
        report.add(
            diagnostics
                .iter()
                .map(|d| problem(d.code.as_str(), d.message.clone(), &d.path)),
        );
        return Ok(None);
    }
    Ok(Some(bindings))
}

/// Who sent each message: the bindings file, or a stand-in that makes
/// every native id an asset of the same name.
enum Assets {
    Bindings(Bindings),
    /// The set each stand-in binding names (none when the family's
    /// version locator chooses).
    StandIn(Option<String>),
}

impl Resolver for Assets {
    fn resolve(&self, family: &str, native_id: &NativeId, at: Timestamp) -> Option<Arc<Binding>> {
        match self {
            Assets::Bindings(bindings) => bindings.resolve(family, native_id, at),
            Assets::StandIn(set) => {
                let binding = Binding::new(native_id.to_string());
                Some(Arc::new(match set {
                    Some(set) => binding.with_set(set),
                    None => binding,
                }))
            }
        }
    }
}

/// One message to decode: its payload, or why it couldn't be read, and
/// a record's own metadata and ground time.
struct Input {
    line: Option<usize>,
    payload: Result<Vec<u8>, Failure>,
    meta: Vec<Meta>,
    time: Option<Timestamp>,
}

impl Input {
    fn one(payload: Vec<u8>) -> Input {
        Input {
            line: None,
            payload: Ok(payload),
            meta: Vec::new(),
            time: None,
        }
    }
}

/// The code for a line or record morphc can't read: not the library's,
/// so prefixed, as frontends prefix theirs.
fn unreadable(code: &str, message: String) -> Failure {
    Failure {
        code: format!("morphc.{code}"),
        message,
    }
}

fn messages(args: &DecodeArgs, encoding: Encoding) -> anyhow::Result<Vec<Input>> {
    let binary = encoding != Encoding::Json;
    if let Some(hex) = &args.hex {
        if !binary {
            bail!("the family is JSON; give its payload with --payload, --lines or --records");
        }
        let payload = parse_hex(hex).map_err(|why| anyhow::anyhow!("--hex: {why}"))?;
        return Ok(vec![Input::one(payload)]);
    }
    if let Some(path) = &args.payload {
        let payload = fs::read(path).with_context(|| format!("can't read {}", path.display()))?;
        return Ok(vec![Input::one(payload)]);
    }
    let (path, records) = match (&args.lines, &args.records) {
        (Some(path), _) => (path, false),
        (None, Some(path)) => (path, true),
        (None, None) => bail!("give the messages with --hex, --payload, --lines or --records"),
    };
    let text = if path.as_os_str() == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .context("can't read stdin as UTF-8 text")?;
        text
    } else {
        fs::read_to_string(path)
            .with_context(|| format!("can't read {} as UTF-8 text", path.display()))?
    };
    let inputs = text
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with('#')
        })
        .map(|(index, line)| {
            let line_number = Some(index + 1);
            if records {
                return match record(line, binary) {
                    Ok(input) => Input {
                        line: line_number,
                        ..input
                    },
                    Err(failure) => Input {
                        line: line_number,
                        payload: Err(failure),
                        meta: Vec::new(),
                        time: None,
                    },
                };
            }
            let payload = if binary {
                parse_hex(line).map_err(|why| unreadable("invalid_hex", why))
            } else {
                Ok(line.trim().as_bytes().to_vec())
            };
            Input {
                line: line_number,
                payload,
                meta: Vec::new(),
                time: None,
            }
        })
        .collect();
    Ok(inputs)
}

/// One `--records` line: a JSON object with the payload (`hex`, or
/// `json` for a JSON family), and optionally `meta` and `time`.
fn record(line: &str, binary: bool) -> Result<Input, Failure> {
    let bad = |why: String| unreadable("invalid_record", why);
    let value: Json =
        serde_json::from_str(line).map_err(|err| bad(format!("not a JSON record: {err}")))?;
    let Json::Object(fields) = value else {
        return Err(bad("a record is a JSON object".to_string()));
    };
    if let Some(unknown) = fields
        .keys()
        .find(|key| !matches!(key.as_str(), "hex" | "text" | "json" | "meta" | "time"))
    {
        return Err(bad(format!(
            "unknown key {unknown:?}; a record has hex, text or json, then meta and time"
        )));
    }
    let given = |key: &str| fields.get(key);
    let payload = match (binary, given("hex"), given("text"), given("json")) {
        (true, Some(Json::String(hex)), None, None) => {
            parse_hex(hex).map_err(|why| unreadable("invalid_hex", why))?
        }
        // The payload's own text, byte for byte.
        (false, None, Some(Json::String(text)), None) => text.clone().into_bytes(),
        // A convenience for records written by hand: the document is
        // written again, so its key order and number spelling are
        // serde_json's, and it can't hold a duplicate key.
        (false, None, None, Some(document)) => document.to_string().into_bytes(),
        (true, _, _, _) => {
            return Err(bad(
                "a binary family's record has \"hex\": \"...\"".to_string()
            ));
        }
        (false, _, _, _) => {
            return Err(bad(
                "a JSON family's record has \"text\": \"...\" (the payload as sent) or \
                 \"json\": {...}"
                    .to_string(),
            ));
        }
    };
    let mut meta = Vec::new();
    match fields.get("meta") {
        None => {}
        Some(Json::Object(given)) => {
            for (name, value) in given {
                let value = match value {
                    Json::Number(n) => match (n.as_u64(), n.as_i64()) {
                        (Some(u), _) => MetaArg::Uint(u),
                        (None, Some(i)) => MetaArg::Int(i),
                        _ => return Err(bad(format!("meta {name}: {n} isn't an integer"))),
                    },
                    Json::String(text) => MetaArg::Text(text.clone()),
                    other => {
                        return Err(bad(format!(
                            "meta {name}: {other} isn't an integer or text"
                        )));
                    }
                };
                meta.push(Meta {
                    name: name.clone(),
                    value,
                });
            }
        }
        Some(_) => return Err(bad("meta is an object of names and values".to_string())),
    }
    let time = match fields.get("time") {
        None => None,
        Some(Json::String(text)) => {
            Some(parse_time(text).map_err(|why| bad(format!("time: {why}")))?)
        }
        Some(_) => return Err(bad("time is an RFC 3339 string".to_string())),
    };
    Ok(Input {
        line: None,
        payload: Ok(payload),
        meta,
        time,
    })
}

// ---- What a decode reports ---------------------------------------------------------

/// What `morphc decode` found, under `decode` in the JSON document.
#[derive(Debug, Serialize)]
pub struct Decoding {
    family: String,
    #[serde(skip)]
    lines: bool,
    messages: Vec<MessageResult>,
}

#[derive(Debug, Serialize)]
struct MessageResult {
    /// The line it came from, with --lines.
    line: Option<usize>,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    telemetry: Option<Telemetry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<CommandResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Failure>,
}

enum Decodes {
    Telemetry(Telemetry),
    Command(CommandResult),
}

impl MessageResult {
    fn of(line: Option<usize>, result: Result<Decodes, Failure>) -> Self {
        let mut out = MessageResult {
            line,
            ok: result.is_ok(),
            telemetry: None,
            command: None,
            error: None,
        };
        match result {
            Ok(Decodes::Telemetry(telemetry)) => out.telemetry = Some(telemetry),
            Ok(Decodes::Command(command)) => out.command = Some(command),
            Err(failure) => out.error = Some(failure),
        }
        out
    }
}

#[derive(Debug, Serialize)]
struct Telemetry {
    /// What the alias locator read: a number for an integer id, text
    /// otherwise.
    native_id: Json,
    asset: String,
    /// The asset is a stand-in (no bindings file), not a registry's.
    stand_in: bool,
    set: String,
    def_version: String,
    packet: String,
    event_time: Json,
    samples: Vec<SampleOut>,
    /// With --raw: every entry's raw value, as a tree like the packet's.
    #[serde(skip_serializing_if = "Option::is_none")]
    record: Option<Json>,
    /// With --raw: each field's and scalar element's engineering value,
    /// by path.
    #[serde(skip_serializing_if = "Option::is_none")]
    engineering: Option<Vec<EngineeringOut>>,
    /// With --raw, for people: every entry's raw value by path, in the
    /// packet's order.
    #[serde(skip)]
    raw_rows: Vec<(String, String)>,
}

#[derive(Debug, Serialize)]
struct SampleOut {
    param: String,
    value: Json,
    /// The catalog param's unit.
    unit: Option<String>,
    /// Where it came from: an entry's path, or a composite's members.
    path: String,
    index: Option<u64>,
    time_offset_ns: Option<i64>,
    quality: String,
}

#[derive(Debug, Serialize)]
struct EngineeringOut {
    path: String,
    value: Json,
    quality: String,
}

fn telemetry(
    decoded: &Decoded<'_>,
    opened: &Opened,
    ground: Timestamp,
    raw: bool,
    stand_in: bool,
) -> Telemetry {
    let catalog = &opened.validated.bundle().catalog;
    let samples = decoded
        .samples
        .iter()
        .map(|sample| SampleOut {
            param: sample.param.to_string(),
            value: sample_json(&sample.value),
            unit: catalog
                .params
                .get(sample.param)
                .and_then(|p| p.unit.clone()),
            path: sample.path.clone(),
            index: sample.index,
            time_offset_ns: sample.time_offset_ns,
            quality: quality(sample.quality),
        })
        .collect();
    let native_id = &decoded.route.native_id;
    let mut raw_rows = Vec::new();
    if raw {
        record_rows(&decoded.record, "", &mut raw_rows);
    }
    Telemetry {
        native_id: match native_id.as_int() {
            Some(n) => integer_json(n),
            None => Json::String(native_id.to_string()),
        },
        asset: decoded.route.binding.asset.clone(),
        stand_in,
        set: decoded.route.set.name.clone(),
        def_version: decoded.route.def_version.to_string(),
        packet: decoded.packet.name.clone(),
        event_time: event_time(&decoded.event_time, ground),
        samples,
        record: raw.then(|| record_json(&decoded.record)),
        engineering: raw.then(|| {
            decoded
                .engineering_values()
                .into_iter()
                .map(|(path, engineering)| EngineeringOut {
                    path,
                    value: sample_json(&engineering.value),
                    quality: quality(engineering.quality),
                })
                .collect()
        }),
        raw_rows,
    }
}

/// Every entry's raw value by path (`pos.x`, `samples[3]`, as the library
/// names engineering values), in the record's order; an empty array as
/// `[]`.
fn record_rows(record: &Record<'_>, prefix: &str, rows: &mut Vec<(String, String)>) {
    for (name, value) in record.iter() {
        let path = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}.{name}")
        };
        value_rows(value, path, rows);
    }
}

fn value_rows(value: &Value<'_>, path: String, rows: &mut Vec<(String, String)>) {
    match value {
        Value::Record(record) => record_rows(record, &path, rows),
        Value::Array(values) if values.is_empty() => rows.push((path, "[]".to_string())),
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                value_rows(value, format!("{path}[{index}]"), rows);
            }
        }
        scalar => rows.push((path, plain(&value_json(scalar)))),
    }
}

#[derive(Debug, Serialize)]
struct CommandResult {
    set: String,
    def_version: String,
    packet: String,
    command: String,
    /// Each argument of the catalog command, in catalog order.
    arguments: Vec<ArgumentOut>,
    target: Option<String>,
}

#[derive(Debug, Serialize)]
struct ArgumentOut {
    name: String,
    value: Json,
    /// The set doesn't encode it, so it has its default.
    defaulted: bool,
}

fn command_result(decoded: &DecodedCommand<'_>) -> CommandResult {
    CommandResult {
        set: decoded.set.name.clone(),
        def_version: decoded.def_version.to_string(),
        packet: decoded.packet.name.clone(),
        command: decoded.command().to_string(),
        arguments: decoded
            .arguments
            .iter()
            .map(|(name, value)| ArgumentOut {
                name: (*name).to_string(),
                value: argument_json(value),
                defaulted: decoded.defaulted.contains(name),
            })
            .collect(),
        target: decoded.target.as_ref().map(NativeId::to_string),
    }
}

/// `good`, `invalid`, `out_of_calibration`, as the library names them.
fn quality(quality: Quality) -> String {
    match quality {
        Quality::Good => "good",
        Quality::UnknownState => "unknown_state",
        Quality::OutOfCalibration => "out_of_calibration",
        Quality::OutOfRange => "out_of_range",
        Quality::NonFinite => "non_finite",
        Quality::Invalid => "invalid",
        _ => "unsupported",
    }
    .to_string()
}

fn event_time(time: &EventTime, ground: Timestamp) -> Json {
    let mut out = Map::new();
    match time {
        EventTime::Ground => {
            out.insert("kind".into(), "ground".into());
            out.insert("time".into(), ground.to_string().into());
        }
        EventTime::At(at) => {
            out.insert("kind".into(), "at".into());
            out.insert("time".into(), at.to_string().into());
        }
        EventTime::SinceBoot { value, unit } => {
            out.insert("kind".into(), "since_boot".into());
            out.insert("value".into(), integer_json(i128::from(*value)));
            out.insert("unit".into(), ir(unit).into());
        }
        other => return unsupported(other),
    }
    Json::Object(out)
}

// ---- For people ----------------------------------------------------------------------

impl Outcome for Decoding {
    const KEY: &'static str = "decode";

    fn human(&self) -> String {
        let mut text = String::new();
        for (index, message) in self.messages.iter().enumerate() {
            if index > 0 {
                text.push('\n');
            }
            let prefix = message
                .line
                .map_or_else(String::new, |line| format!("line {line}: "));
            if let Some(telemetry) = &message.telemetry {
                text.push_str(&format!("{prefix}{}", telemetry_text(telemetry)));
            } else if let Some(command) = &message.command {
                text.push_str(&format!("{prefix}{}", command_text(command)));
            } else if let Some(error) = &message.error {
                text.push_str(&format!(
                    "{prefix}error[{}]: {}\n",
                    error.code, error.message
                ));
            }
        }
        if self.lines {
            let decoded = self.messages.iter().filter(|m| m.ok).count();
            text.push_str(&format!(
                "\ndecoded {decoded} of {} messages\n",
                self.messages.len()
            ));
        }
        text
    }

    fn failed(&self) -> bool {
        self.messages.iter().any(|m| !m.ok)
    }
}

/// A JSON value as people read it: text without quotes.
fn plain(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn table(rows: &[Vec<String>]) -> String {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..columns)
        .map(|c| {
            rows.iter()
                .filter_map(|row| row.get(c))
                .map(|cell| cell.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut text = String::new();
    for row in rows {
        let cells: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, width)| format!("{cell:width$}"))
            .collect();
        text.push_str(format!("  {}", cells.join("  ")).trim_end());
        text.push('\n');
    }
    text
}

fn telemetry_text(t: &Telemetry) -> String {
    let native_id = plain(&t.native_id);
    let asset = if t.stand_in {
        format!("native id {native_id} (no bindings)")
    } else {
        format!("asset {} (native id {native_id})", t.asset)
    };
    let time = match t.event_time.get("kind").and_then(Json::as_str) {
        Some("ground") => format!("ground time {}", plain(&t.event_time["time"])),
        Some("at") => format!("event time {}", plain(&t.event_time["time"])),
        Some("since_boot") => format!(
            "{} {} since boot",
            plain(&t.event_time["value"]),
            plain(&t.event_time["unit"])
        ),
        _ => t.event_time.to_string(),
    };
    let mut text = format!(
        "set {}  packet {}  {asset}\n  def_version {}\n  {time}\n",
        t.set, t.packet, t.def_version
    );
    if t.samples.is_empty() {
        text.push_str("  no catalog samples\n");
    } else {
        let all_good = t.samples.iter().all(|s| s.quality == "good");
        let mut header = vec![
            "param".to_string(),
            "value".to_string(),
            "unit".to_string(),
            "from".to_string(),
        ];
        if !all_good {
            header.push("quality".to_string());
        }
        let mut rows = vec![header];
        for sample in &t.samples {
            let from = match sample.time_offset_ns {
                Some(offset) if offset != 0 => format!("{} (+{offset} ns)", sample.path),
                _ => sample.path.clone(),
            };
            let mut row = vec![
                sample.param.clone(),
                plain(&sample.value),
                sample.unit.clone().unwrap_or_default(),
                from,
            ];
            if sample.quality != "good" {
                row.push(sample.quality.clone());
            }
            rows.push(row);
        }
        text.push_str(&table(&rows));
    }
    if let Some(engineering) = &t.engineering {
        let mut rows = vec![vec![
            "entry".to_string(),
            "raw".to_string(),
            "engineering".to_string(),
        ]];
        for (path, value) in &t.raw_rows {
            let converted = engineering
                .iter()
                .find(|e| e.path == *path)
                .map(|e| {
                    let value = plain(&e.value);
                    if e.quality == "good" {
                        value
                    } else {
                        format!("{value} ({})", e.quality)
                    }
                })
                .unwrap_or_default();
            rows.push(vec![path.clone(), value.clone(), converted]);
        }
        text.push_str("  entries\n");
        text.push_str(&table(&rows));
    }
    text
}

fn command_text(c: &CommandResult) -> String {
    let mut text = format!(
        "set {}  command {}  encodes {}\n  def_version {}\n",
        c.set, c.packet, c.command, c.def_version
    );
    let mut rows = Vec::new();
    for argument in &c.arguments {
        let mut row = vec![argument.name.clone(), plain(&argument.value)];
        if argument.defaulted {
            row.push("(default; the set doesn't encode it)".to_string());
        }
        rows.push(row);
    }
    if rows.is_empty() {
        text.push_str("  no arguments\n");
    } else {
        text.push_str(&table(&rows));
    }
    if let Some(target) = &c.target {
        text.push_str(&format!("  sent to {target}\n"));
    }
    text
}
