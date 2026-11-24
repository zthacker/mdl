//! `morphc encode`: a catalog command and its arguments as one set's
//! command packet, with the metadata a link frames it with and the
//! `def_version` to record with it; or, with `--packet`, one of a set's
//! telemetry packets, from its params' values and raw values, as an asset
//! sends it.

use std::collections::BTreeMap;
use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::Args;
use mdl::bindings::NativeId;
use mdl::decode::{EventTime, SampleValue};
use mdl::encode::{ArgumentValue, CommandRequest, RawValue, TelemetryRequest};
use mdl::ir::{
    Argument, Element, Encoding, Entry, Frame, Locator, Param, ParamType, ScalarType, StructType,
};
use mdl::runtime::{EncodedCommand, EncodedTelemetry, Value};
use mdl::source::SourceDiagnostic;
use mdl::time::Timestamp;
use serde::Serialize;
use serde_json::Value as Json;

use crate::decode::parse_time;
use crate::report::{Outcome, Report};
use crate::values::{Failure, hex, parse_hex};
use crate::{Opened, Origin, ReadArgs, family_sets, open};

#[derive(Debug, Args)]
pub struct EncodeArgs {
    /// The catalog command to send (`eps.set_mode`).
    #[arg(
        long,
        value_name = "COMMAND",
        required_unless_present = "packet",
        conflicts_with = "packet"
    )]
    command: Option<String>,

    /// A telemetry packet to encode instead, as the asset `--from` names
    /// sends it (`HK`).
    #[arg(long, value_name = "PACKET", conflicts_with_all = ["args", "target"])]
    packet: Option<String>,

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

    /// For --packet, a param's value as NAME=VALUE, in the param's units
    /// (repeat for each). VALUE reads as for --arg; a vec3 or quat as a
    /// JSON object (`{"frame": "BODY", "x": 1, "y": 0, "z": 0}`, a quat's
    /// with `w`), and a param the packet holds several times, such as an
    /// array's elements, as a JSON array of its values in order.
    #[arg(long = "value", value_name = "NAME=VALUE")]
    values: Vec<String>,

    /// For --packet, a raw value as ENTRY=VALUE, for a top-level entry
    /// (repeat for each): a field's as its type reads it (a number, `true`
    /// or `false`, text, bytes in hex), an array's as a JSON array, and a
    /// struct's as a JSON object of its entries' raw values by name.
    #[arg(long = "raw", value_name = "ENTRY=VALUE")]
    raw: Vec<String>,

    /// For --packet, the native id of the asset that sends it [default:
    /// the family's alias, when it's always one value]
    #[arg(long, value_name = "ID")]
    from: Option<String>,

    /// For --packet, when its values were taken, as RFC 3339
    /// (`2026-11-09T12:00:00Z`): the event time its clocks read. A packet
    /// with no clock has ground time only, so it ignores it.
    #[arg(long, value_name = "INSTANT", value_parser = parse_time)]
    at: Option<Timestamp>,

    /// For --packet, when the asset booted, as RFC 3339: with --at, what
    /// clocks that count time since boot read.
    #[arg(long, value_name = "INSTANT", value_parser = parse_time)]
    booted: Option<Timestamp>,

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
    let Some(opened) = open(&origin, &sources, &mut report)? else {
        return Ok(report.finish::<Encode>(None));
    };
    if let Some(packet) = &args.packet {
        let encode = telemetry(args, packet, &opened)?;
        return Ok(report.finish(Some(encode)));
    }
    if !args.values.is_empty()
        || !args.raw.is_empty()
        || args.from.is_some()
        || args.at.is_some()
        || args.booted.is_some()
    {
        bail!(
            "--value, --raw, --from, --at and --booted give a telemetry packet's values; say \
             which with --packet"
        );
    }
    let Some(command_name) = args.command.as_deref() else {
        bail!("say what to encode with --command or --packet");
    };
    let bundle = opened.validated.bundle();
    let Some(command) = bundle.catalog.commands.get(command_name) else {
        bail!("the catalog has no command {command_name}");
    };
    let set = choose_set(&opened, command_name, args.set.as_deref())?;
    let family = bundle
        .sets
        .iter()
        .find(|s| s.name == set)
        .map(|s| s.family.clone())
        .with_context(|| format!("there's no set {set}"))?;
    let family_sets = family_sets(&opened, &family)?;

    let mut request = CommandRequest::new(set.as_str(), command_name);
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
                    format!("command {command_name} is deprecated: {note}"),
                )]);
            }
            Encode {
                ok: true,
                packet: Some(EncodedCommand::of(&encoded, &family, command_name)),
                telemetry: None,
                error: None,
                json_family: family_sets.family().encoding == Encoding::Json,
            }
        }
        Err(err) => Encode::failed_with(err.code(), err.to_string()),
    };
    Ok(report.finish(Some(encode)))
}

/// One of a set's telemetry packets, as the asset `--from` names sends it.
fn telemetry(args: &EncodeArgs, packet: &str, opened: &Opened) -> anyhow::Result<Encode> {
    let bundle = opened.validated.bundle();
    let set = choose_packet_set(opened, packet, args.set.as_deref())?;
    let Some(set_def) = bundle.sets.iter().find(|s| s.name == set) else {
        bail!("there's no set {set}");
    };
    let Some(packet_def) = set_def.packets.iter().find(|p| p.name == packet) else {
        bail!("set {set} has no packet {packet}");
    };
    let family = set_def.family.clone();
    let family_sets = family_sets(opened, &family)?;
    let native_id = match (&args.from, &family_sets.family().alias) {
        (Some(id), _) => NativeId::from_text(id.as_str()),
        (None, Locator::Fixed { value, .. }) => NativeId::from_text(value.as_str()),
        (None, _) => bail!("say which asset sends packet {packet} with --from"),
    };
    let mut request = TelemetryRequest::new(set.as_str(), packet, native_id);
    for given in &args.values {
        let Some((name, text)) = given.split_once('=').filter(|(name, _)| !name.is_empty()) else {
            bail!("--value {given:?} isn't NAME=VALUE");
        };
        let values = match bundle.catalog.params.get(name) {
            Some(param) => {
                param_values(param, text).map_err(|why| anyhow::anyhow!("--value {name}: {why}"))?
            }
            // The library reports a param the packet doesn't map.
            None => vec![SampleValue::Str(text.to_string())],
        };
        request = request.with_params(name, values);
    }
    for given in &args.raw {
        let Some((name, text)) = given.split_once('=').filter(|(name, _)| !name.is_empty()) else {
            bail!("--raw {given:?} isn't ENTRY=VALUE");
        };
        let found = packet_def
            .entries
            .iter()
            .find(|entry| entry_name(entry) == Some(name));
        let value = match found {
            Some(entry) => raw_value(entry, &set_def.types, text)
                .map_err(|why| anyhow::anyhow!("--raw {name}: {why}"))?,
            // The library reports an entry the packet doesn't have.
            None => RawValue::Text(text.to_string()),
        };
        request = request.with_raw(name, value);
    }
    if let Some(at) = args.at {
        request = request.with_at(at);
    }
    if let Some(booted) = args.booted {
        request = request.with_booted(booted);
    }
    Ok(match family_sets.encode_telemetry(&request) {
        Ok(encoded) => Encode {
            ok: true,
            packet: None,
            telemetry: Some(EncodedTelemetry::of(&encoded, &family, &request.native_id)),
            error: None,
            json_family: family_sets.family().encoding == Encoding::Json,
        },
        Err(err) => Encode::failed_with(err.code(), err.to_string()),
    })
}

/// The set named, or the only one with a telemetry packet of that name.
fn choose_packet_set(opened: &Opened, packet: &str, named: Option<&str>) -> anyhow::Result<String> {
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
        .filter(|s| s.packets.iter().any(|p| p.name == packet))
        .map(|s| s.name.as_str())
        .collect();
    match sets.as_slice() {
        [only] => Ok((*only).to_string()),
        [] => bail!("no set has a packet {packet}"),
        _ => bail!(
            "sets {} have a packet {packet}; say which with --set",
            sets.join(", ")
        ),
    }
}

fn entry_name(entry: &Entry) -> Option<&str> {
    match entry {
        Entry::Field(field) => Some(&field.name),
        Entry::Array(array) => Some(&array.name),
        Entry::Struct(inner) => Some(&inner.name),
        Entry::Checksum(checksum) => Some(&checksum.name),
        _ => None,
    }
}

/// A param's values from VALUE: one, or a JSON array of them in order.
fn param_values(param: &Param, text: &str) -> Result<Vec<SampleValue>, String> {
    let json = || serde_json::from_str::<Json>(text).map_err(|_| format!("{text:?} isn't JSON"));
    if text.trim_start().starts_with('[') {
        let Json::Array(values) = json()? else {
            return Err(format!("{text:?} isn't a JSON array"));
        };
        return values
            .iter()
            .map(|value| param_json(param, value))
            .collect();
    }
    match param.ty {
        ParamType::Vec3 | ParamType::Quat => Ok(vec![param_json(param, &json()?)?]),
        ty => sample(ty, text).map(|value| vec![value]),
    }
}

/// One param value from text, as its type reads it.
fn sample(ty: ParamType, text: &str) -> Result<SampleValue, String> {
    Ok(match scalar_value(ty, text)? {
        ArgumentValue::Float(x) => SampleValue::F64(x),
        ArgumentValue::Int(n) => SampleValue::I64(n),
        ArgumentValue::Uint(n) => SampleValue::U64(n),
        ArgumentValue::Bool(b) => SampleValue::Bool(b),
        ArgumentValue::Text(text) if ty == ParamType::Enum => SampleValue::Enum(text),
        ArgumentValue::Text(text) => SampleValue::Str(text),
        ArgumentValue::Bytes(bytes) => SampleValue::Bytes(bytes),
        other => return Err(format!("{other} isn't a param value")),
    })
}

/// One param value from its JSON form: a vector as an object, anything
/// else as a list argument's element reads.
fn param_json(param: &Param, value: &Json) -> Result<SampleValue, String> {
    let Json::Object(fields) = value else {
        return match element_value(param.ty, value)? {
            ArgumentValue::List(_) => Err(format!("{value} isn't one value")),
            single => sample(param.ty, &single.to_string()).or_else(|_| match single {
                ArgumentValue::Text(text) if param.ty == ParamType::Enum => {
                    Ok(SampleValue::Enum(text))
                }
                ArgumentValue::Text(text) => Ok(SampleValue::Str(text)),
                ArgumentValue::Bytes(bytes) => Ok(SampleValue::Bytes(bytes)),
                _ => Err(format!("{value} isn't a param value")),
            }),
        };
    };
    let frame = fields
        .get("frame")
        .cloned()
        .and_then(|frame| serde_json::from_value::<Frame>(frame).ok())
        .ok_or_else(|| format!("{value} names no frame, such as \"BODY\""))?;
    let number = |key: &str| {
        fields
            .get(key)
            .and_then(Json::as_f64)
            .ok_or_else(|| format!("{value} has no number {key}"))
    };
    match param.ty {
        ParamType::Vec3 => Ok(SampleValue::Vec3 {
            frame,
            x: number("x")?,
            y: number("y")?,
            z: number("z")?,
        }),
        ParamType::Quat => Ok(SampleValue::Quat {
            frame,
            w: number("w")?,
            x: number("x")?,
            y: number("y")?,
            z: number("z")?,
        }),
        _ => Err(format!("{value} is a vector, and the param isn't")),
    }
}

/// An entry's raw value from VALUE: a field's as its type reads text, an
/// array's or struct's from JSON.
fn raw_value(
    entry: &Entry,
    types: &BTreeMap<String, StructType>,
    text: &str,
) -> Result<RawValue, String> {
    match entry {
        Entry::Field(field) => raw_scalar(field.ty, text),
        Entry::Checksum(_) => raw_scalar(ScalarType::Uint, text),
        _ => {
            let json: Json =
                serde_json::from_str(text).map_err(|_| format!("{text:?} isn't JSON"))?;
            raw_json(entry, types, &json)
        }
    }
}

fn raw_scalar(ty: ScalarType, text: &str) -> Result<RawValue, String> {
    match ty {
        ScalarType::Uint | ScalarType::Int => text
            .parse()
            .map(RawValue::Integer)
            .map_err(|_| format!("{text:?} isn't an integer")),
        ScalarType::Float => text
            .parse()
            .map(RawValue::Float)
            .map_err(|_| format!("{text:?} isn't a number")),
        ScalarType::Bool => match text {
            "true" => Ok(RawValue::Bool(true)),
            "false" => Ok(RawValue::Bool(false)),
            _ => Err(format!("{text:?} isn't true or false")),
        },
        ScalarType::String => Ok(RawValue::Text(text.to_string())),
        ScalarType::Bytes => parse_hex(text).map(RawValue::Bytes),
        _ => Err(format!("{text:?} isn't a raw value of this field")),
    }
}

/// An entry's raw value from JSON, typed by its definition: a field's by
/// its type (bytes in hex), an array's as a JSON array, a struct's as an
/// object by entry name.
fn raw_json(
    entry: &Entry,
    types: &BTreeMap<String, StructType>,
    json: &Json,
) -> Result<RawValue, String> {
    match entry {
        Entry::Field(field) => raw_json_scalar(field.ty, json),
        Entry::Checksum(_) => raw_json_scalar(ScalarType::Uint, json),
        Entry::Array(array) => {
            let Json::Array(items) = json else {
                return Err(format!("{json} isn't a JSON array"));
            };
            items
                .iter()
                .map(|item| match (&array.element, item) {
                    (_, Json::Null) => Ok(RawValue::Null),
                    (Element::Scalar(element), item) => raw_json_scalar(element.ty, item),
                    (Element::Struct { name, .. }, item) => raw_record(name, types, item),
                    _ => Err(format!("{item} isn't an element of this array")),
                })
                .collect::<Result<Vec<_>, _>>()
                .map(RawValue::List)
        }
        Entry::Struct(inner) => raw_record(&inner.struct_type, types, json),
        _ => Err(format!("{json} isn't a raw value of this entry")),
    }
}

fn raw_record(
    type_name: &str,
    types: &BTreeMap<String, StructType>,
    json: &Json,
) -> Result<RawValue, String> {
    let Json::Object(members) = json else {
        return Err(format!("{json} isn't a JSON object"));
    };
    let entries = types.get(type_name).map_or(&[][..], |t| &t.entries);
    members
        .iter()
        .map(|(name, value)| {
            let raw = match entries
                .iter()
                .find(|e| entry_name(e) == Some(name.as_str()))
            {
                Some(entry) => raw_json(entry, types, value)?,
                // The library reports an entry the struct doesn't have.
                None => RawValue::Null,
            };
            Ok((name.clone(), raw))
        })
        .collect::<Result<Vec<_>, String>>()
        .map(RawValue::Record)
}

fn raw_json_scalar(ty: ScalarType, json: &Json) -> Result<RawValue, String> {
    match (ty, json) {
        (_, Json::Null) => Ok(RawValue::Null),
        (ScalarType::Uint | ScalarType::Int, Json::Number(n)) => n
            .as_i64()
            .map(i128::from)
            .or_else(|| n.as_u64().map(i128::from))
            .map(RawValue::Integer)
            .ok_or_else(|| format!("{n} isn't an integer")),
        (ScalarType::Uint | ScalarType::Int, Json::String(text)) => text
            .parse()
            .map(RawValue::Integer)
            .map_err(|_| format!("{text:?} isn't an integer")),
        (ScalarType::Float, Json::Number(n)) => n
            .as_f64()
            .map(RawValue::Float)
            .ok_or_else(|| format!("{n} isn't a number")),
        (ScalarType::Bool, Json::Bool(b)) => Ok(RawValue::Bool(*b)),
        (ScalarType::String, Json::String(text)) => Ok(RawValue::Text(text.clone())),
        (ScalarType::Bytes, Json::String(text)) => parse_hex(text).map(RawValue::Bytes),
        (_, json) => Err(format!("{json} isn't a raw value of this field")),
    }
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
    /// The command as the runtime API gives it (`spec/api.md` §3.2).
    #[serde(skip_serializing_if = "Option::is_none")]
    packet: Option<EncodedCommand>,
    /// The telemetry packet as the runtime API gives it (§3.6).
    #[serde(skip_serializing_if = "Option::is_none")]
    telemetry: Option<EncodedTelemetry>,
    /// Why it couldn't be encoded: the library's stable code and message.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Failure>,
    /// For people: the payload is a JSON family's text.
    #[serde(skip)]
    json_family: bool,
}

impl Encode {
    fn failed_with(code: &str, message: String) -> Encode {
        Encode {
            ok: false,
            packet: None,
            telemetry: None,
            json_family: false,
            error: Some(Failure {
                code: code.to_string(),
                message,
            }),
        }
    }

    /// A payload for people: a JSON family's as its text.
    fn shown(&self, payload: &[u8]) -> String {
        match (self.json_family, std::str::from_utf8(payload)) {
            (true, Ok(text)) => text.to_string(),
            _ => hex(payload),
        }
    }
}

/// Metadata for people, `name = value`.
fn metadata_line(metadata: &[(String, Value)]) -> String {
    let metadata: Vec<String> = metadata
        .iter()
        .map(|(name, value)| match value {
            Value::Text(text) => format!("{name} = {text}"),
            Value::Integer(n) => format!("{name} = {n}"),
            other => format!("{name} = {other:?}"),
        })
        .collect();
    format!("  metadata {}\n", metadata.join(", "))
}

impl Outcome for Encode {
    const KEY: &'static str = "encode";

    fn human(&self) -> String {
        if let Some(error) = &self.error {
            return format!("error[{}]: {}\n", error.code, error.message);
        }
        if let Some(t) = &self.telemetry {
            let mut text = format!(
                "set {}  packet {}  from native id {}\n  def_version {}\n  payload {}\n",
                t.set,
                t.packet,
                t.native_id,
                t.def_version,
                self.shown(&t.payload)
            );
            if !t.metadata.is_empty() {
                text.push_str(&metadata_line(&t.metadata));
            }
            match t.event_time {
                EventTime::At(at) => text.push_str(&format!("  event time {at}\n")),
                EventTime::SinceBoot { value, unit } => {
                    text.push_str(&format!("  {value} {} since boot\n", unit.as_str()));
                }
                _ => {}
            }
            // A correlation packet that teaches when its asset booted.
            if let Some(booted) = t.booted {
                text.push_str(&format!("  booted {booted}\n"));
            }
            return text;
        }
        let Some(p) = &self.packet else {
            return String::new();
        };
        // A JSON payload is UTF-8 text, shown as sent.
        let mut text = format!(
            "set {}  command {}  encodes {}\n  def_version {}\n  payload {}\n",
            p.set,
            p.packet,
            p.command,
            p.def_version,
            self.shown(&p.payload)
        );
        if !p.metadata.is_empty() {
            text.push_str(&metadata_line(&p.metadata));
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
