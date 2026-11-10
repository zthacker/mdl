//! `morphc encode`: a catalog command and its arguments as one set's
//! command packet, with the metadata a link frames it with and the
//! `def_version` to record with it.

use std::collections::BTreeMap;
use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::Args;
use mdl::v2::bindings::NativeId;
use mdl::v2::encode::{ArgumentValue, CommandRequest, Encoded};
use mdl::v2::ir::{Argument, Element, Encoding, Entry, ParamType, StructType};
use mdl::v2::message::MetaValue;
use mdl::v2::source::SourceDiagnostic;
use serde::Serialize;
use serde_json::{Map, Value as Json};

use crate::report::{Outcome, Report};
use crate::values::{Failure, hex, integer_json, parse_hex, record_json, unsupported};
use crate::{Opened, Origin, ReadArgs, family_sets, open};

#[derive(Debug, Args)]
pub struct EncodeArgs {
    /// The catalog command to send (`eps.set_mode`).
    #[arg(long, value_name = "COMMAND")]
    command: String,

    /// An argument as NAME=VALUE, in the argument's units (repeat for
    /// each). The catalog's type says how VALUE reads: a number, `true` or
    /// `false`, a state's name, text, bytes in hex, or for a list argument
    /// a JSON array (`[100, 200]`). Arguments left out take their
    /// defaults.
    #[arg(long = "arg", value_name = "NAME=VALUE")]
    args: Vec<String>,

    /// The set whose command packet encodes it [default: the only set
    /// that encodes the command]
    #[arg(long, value_name = "NAME")]
    set: Option<String>,

    /// The native id of the asset it's sent to, for a packet that
    /// carries one.
    #[arg(long, value_name = "ID")]
    target: Option<String>,

    // Last, since the XTCE options' heading applies to every argument
    // after them.
    #[command(flatten)]
    pub read: ReadArgs,
}

pub fn run(args: &EncodeArgs) -> anyhow::Result<ExitCode> {
    let read = &args.read;
    let origin = Origin::of(read)?;
    let sources = origin.sources()?;
    let mut report = Report::new(&sources, read.format, read.all_warnings);
    let Some(opened) = open(&origin, read, &sources, &mut report)? else {
        return Ok(report.finish::<Encode>(None));
    };
    let bundle = opened.validated.bundle();
    let Some(command) = bundle.catalog.commands.get(&args.command) else {
        bail!("the catalog has no command {}", args.command);
    };
    let set = choose_set(&opened, &args.command, args.set.as_deref())?;
    let family = bundle
        .sets
        .iter()
        .find(|s| s.name == set)
        .map(|s| s.family.clone())
        .with_context(|| format!("there's no set {set}"))?;
    let family_sets = family_sets(&opened, &family)?;

    let mut request = CommandRequest::new(set.as_str(), args.command.as_str());
    for given in &args.args {
        let Some((name, text)) = given.split_once('=').filter(|(name, _)| !name.is_empty()) else {
            bail!("--arg {given:?} isn't NAME=VALUE");
        };
        let value = match command.arguments.iter().find(|a| a.name == name) {
            Some(argument) => argument_value(argument, text)
                .map_err(|why| anyhow::anyhow!("--arg {name}: {why}"))?,
            // The library reports an argument the command doesn't have.
            None => ArgumentValue::Text(text.to_string()),
        };
        request = request.with_argument(name, value);
    }
    if let Some(target) = &args.target {
        request = request.with_target(NativeId::from_text(target.as_str()));
    }
    let encode = match family_sets.encode(&request) {
        Ok(encoded) => {
            if args.target.is_some() && !writes_target(&encoded.packet.entries, &encoded.set.types)
            {
                bail!(
                    "command packet {} carries no asset id, so --target goes nowhere; leave it out",
                    encoded.packet.name
                );
            }
            if let Some(note) = encoded.deprecated {
                report.add([SourceDiagnostic::warning(
                    "morphc.deprecated_command",
                    format!("command {} is deprecated: {note}", args.command),
                )]);
            }
            Encode {
                ok: true,
                packet: Some(packet_out(
                    &encoded,
                    &args.command,
                    family_sets.family().encoding,
                )),
                error: None,
            }
        }
        Err(err) => Encode {
            ok: false,
            packet: None,
            error: Some(Failure {
                code: err.code().to_string(),
                message: err.to_string(),
            }),
        },
    };
    Ok(report.finish(Some(encode)))
}

/// The set named, or the only one with a command packet for `command`.
fn choose_set(opened: &Opened, command: &str, named: Option<&str>) -> anyhow::Result<String> {
    let bundle = opened.validated.bundle();
    if let Some(set) = named {
        if !bundle.sets.iter().any(|s| s.name == set) {
            let sets: Vec<&str> = bundle.sets.iter().map(|s| s.name.as_str()).collect();
            bail!("there's no set {set}; the sets are {}", sets.join(", "));
        }
        return Ok(set.to_string());
    }
    let sets: Vec<&str> = bundle
        .sets
        .iter()
        .filter(|s| s.commands.iter().any(|c| c.canonical == command))
        .map(|s| s.name.as_str())
        .collect();
    match sets.as_slice() {
        [only] => Ok((*only).to_string()),
        [] => bail!("no set encodes command {command}"),
        _ => bail!(
            "sets {} encode command {command}; say which with --set",
            sets.join(", ")
        ),
    }
}

/// Whether a field of `entries`, or of a struct among them, writes the
/// asset's native id.
fn writes_target(entries: &[Entry], types: &BTreeMap<String, StructType>) -> bool {
    entries.iter().any(|entry| match entry {
        Entry::Field(field) => field.target == Some(true),
        Entry::Struct(inner) => types
            .get(&inner.struct_type)
            .is_some_and(|t| writes_target(&t.entries, types)),
        Entry::Array(array) => match &array.element {
            Element::Struct { name, .. } => types
                .get(name)
                .is_some_and(|t| writes_target(&t.entries, types)),
            _ => false,
        },
        _ => false,
    })
}

/// VALUE as the catalog types the argument.
fn argument_value(argument: &Argument, text: &str) -> Result<ArgumentValue, String> {
    if argument.list.is_some() {
        let values: Json = serde_json::from_str(text)
            .map_err(|_| format!("{text:?} isn't a JSON array; a list is given as [1, 2]"))?;
        let Json::Array(values) = values else {
            return Err(format!(
                "{text:?} isn't a JSON array; a list is given as [1, 2]"
            ));
        };
        return values
            .iter()
            .map(|value| element_value(argument.ty, value))
            .collect::<Result<Vec<_>, _>>()
            .map(ArgumentValue::List);
    }
    scalar_value(argument.ty, text)
}

fn scalar_value(ty: ParamType, text: &str) -> Result<ArgumentValue, String> {
    match ty {
        ParamType::F64 => text
            .parse()
            .map(ArgumentValue::Float)
            .map_err(|_| format!("{text:?} isn't a number")),
        ParamType::I64 => text
            .parse()
            .map(ArgumentValue::Int)
            .map_err(|_| format!("{text:?} isn't an integer")),
        ParamType::U64 => text
            .parse()
            .map(ArgumentValue::Uint)
            .map_err(|_| format!("{text:?} isn't an unsigned integer")),
        ParamType::Bool => match text {
            "true" => Ok(ArgumentValue::Bool(true)),
            "false" => Ok(ArgumentValue::Bool(false)),
            _ => Err(format!("{text:?} isn't true or false")),
        },
        ParamType::Str | ParamType::Enum => Ok(ArgumentValue::Text(text.to_string())),
        ParamType::Bytes => parse_hex(text).map(ArgumentValue::Bytes),
        other => Err(format!(
            "a {} argument can't be given on the command line",
            crate::describe::ir(&other)
        )),
    }
}

/// One element of a list argument, from its JSON form: as decode writes
/// it, so its output can be given back (an integer past ±(2^53 - 1) as
/// decimal text, a non-finite float as `{"non_finite": "NaN"}`).
fn element_value(ty: ParamType, value: &Json) -> Result<ArgumentValue, String> {
    match (ty, value) {
        (ParamType::I64 | ParamType::U64, Json::String(text)) => scalar_value(ty, text),
        (ParamType::F64, Json::Object(fields)) if fields.len() == 1 => {
            match fields.get("non_finite").and_then(Json::as_str) {
                Some("NaN") => Ok(ArgumentValue::Float(f64::NAN)),
                Some("inf") => Ok(ArgumentValue::Float(f64::INFINITY)),
                Some("-inf") => Ok(ArgumentValue::Float(f64::NEG_INFINITY)),
                _ => Err(format!("{value} isn't a f64 element")),
            }
        }
        (ParamType::F64, Json::Number(n)) => n
            .as_f64()
            .map(ArgumentValue::Float)
            .ok_or_else(|| format!("{n} isn't a number")),
        (ParamType::I64, Json::Number(n)) => n
            .as_i64()
            .map(ArgumentValue::Int)
            .ok_or_else(|| format!("{n} isn't an integer")),
        (ParamType::U64, Json::Number(n)) => n
            .as_u64()
            .map(ArgumentValue::Uint)
            .ok_or_else(|| format!("{n} isn't an unsigned integer")),
        (ParamType::Bool, Json::Bool(b)) => Ok(ArgumentValue::Bool(*b)),
        (ParamType::Str | ParamType::Enum, Json::String(text)) => {
            Ok(ArgumentValue::Text(text.clone()))
        }
        (ParamType::Bytes, Json::String(text)) => parse_hex(text).map(ArgumentValue::Bytes),
        (ty, value) => Err(format!(
            "{value} isn't a {} element",
            crate::describe::ir(&ty)
        )),
    }
}

// ---- What an encode reports --------------------------------------------------------

/// What `morphc encode` made, under `encode` in the JSON document.
#[derive(Debug, Serialize)]
pub struct Encode {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    packet: Option<PacketOut>,
    /// Why it couldn't be encoded: the library's stable code and message.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Failure>,
}

#[derive(Debug, Serialize)]
struct PacketOut {
    set: String,
    /// The set artifact's digest, to record with what was sent.
    def_version: String,
    packet: String,
    command: String,
    /// `binary` or `json`.
    encoding: String,
    /// The payload: hex for a binary family, the JSON text for a JSON one.
    payload: String,
    /// The metadata a link frames the payload with.
    metadata: Map<String, Json>,
    /// Arguments left out, which took their defaults, in catalog order.
    defaulted: Vec<String>,
    /// The catalog's deprecation note, when the command is retired.
    deprecated: Option<String>,
    /// Each field's and checksum's raw value, as decoding gives them.
    record: Json,
}

fn packet_out(encoded: &Encoded<'_>, command: &str, family: Encoding) -> PacketOut {
    // A JSON payload is UTF-8 text.
    let (encoding, payload) = match (family, std::str::from_utf8(&encoded.payload)) {
        (Encoding::Json, Ok(text)) => ("json", text.to_string()),
        _ => ("binary", hex(&encoded.payload)),
    };
    PacketOut {
        set: encoded.set.name.clone(),
        def_version: encoded.def_version.to_string(),
        packet: encoded.packet.name.clone(),
        command: command.to_string(),
        encoding: encoding.to_string(),
        payload,
        metadata: encoded
            .metadata()
            .into_iter()
            .map(|(name, value)| {
                let value = match value {
                    MetaValue::Uint(n) => integer_json(i128::from(n)),
                    MetaValue::Int(n) => integer_json(i128::from(n)),
                    MetaValue::Text(text) => Json::String(text.to_string()),
                    other => unsupported(&other),
                };
                (name.to_string(), value)
            })
            .collect(),
        defaulted: encoded.defaulted.iter().map(|s| (*s).to_string()).collect(),
        deprecated: encoded.deprecated.map(str::to_string),
        record: record_json(&encoded.record),
    }
}

impl Outcome for Encode {
    const KEY: &'static str = "encode";

    fn human(&self) -> String {
        if let Some(error) = &self.error {
            return format!("error[{}]: {}\n", error.code, error.message);
        }
        let Some(p) = &self.packet else {
            return String::new();
        };
        let mut text = format!(
            "set {}  command {}  encodes {}\n  def_version {}\n  payload {}\n",
            p.set, p.packet, p.command, p.def_version, p.payload
        );
        if !p.metadata.is_empty() {
            let metadata: Vec<String> = p
                .metadata
                .iter()
                .map(|(name, value)| match value {
                    Json::String(text) => format!("{name} = {text}"),
                    other => format!("{name} = {other}"),
                })
                .collect();
            text.push_str(&format!("  metadata {}\n", metadata.join(", ")));
        }
        if !p.defaulted.is_empty() {
            text.push_str(&format!("  defaulted {}\n", p.defaulted.join(", ")));
        }
        if let Some(note) = &p.deprecated {
            text.push_str(&format!("  deprecated: {note}\n"));
        }
        text
    }

    fn failed(&self) -> bool {
        !self.ok
    }
}
