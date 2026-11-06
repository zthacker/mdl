//! Encoding commands: a catalog command and its arguments, as the payload
//! of a set's command packet. `spec/encoding.md` is the normative
//! description.
//!
//! Encoding runs in three steps: check the request's arguments against the
//! catalog command (types, states, ranges, defaults); turn each into the raw
//! value its field holds (inverse conversions, rounding, fit); then lay the
//! packet out, as bits or as a JSON document, writing checksums last.

mod write;

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use thiserror::Error;

use crate::canonical::format_double;
use crate::v2::bindings::NativeId;
use crate::v2::decode::{Record, Value, convert};
use crate::v2::ir::{
    Argument, ArgumentRange, ByteLength, Command, CommandPacket, Conversion, Digest, Entry, Field,
    FromField, Literal, ParamType, Scalar, ScalarType, Set, TextEncoding,
};
use crate::v2::route::FamilySets;
use crate::v2::validate::rules::integer_value;

/// A value for one argument of a command, in the argument's engineering
/// units.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ArgumentValue {
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    /// A string, or the name of one of an enum argument's states.
    Text(String),
    Bytes(Vec<u8>),
}

impl From<bool> for ArgumentValue {
    fn from(value: bool) -> Self {
        ArgumentValue::Bool(value)
    }
}

impl From<i64> for ArgumentValue {
    fn from(value: i64) -> Self {
        ArgumentValue::Int(value)
    }
}

impl From<i32> for ArgumentValue {
    fn from(value: i32) -> Self {
        ArgumentValue::Int(i64::from(value))
    }
}

impl From<u64> for ArgumentValue {
    fn from(value: u64) -> Self {
        ArgumentValue::Uint(value)
    }
}

impl From<u32> for ArgumentValue {
    fn from(value: u32) -> Self {
        ArgumentValue::Uint(u64::from(value))
    }
}

impl From<f64> for ArgumentValue {
    fn from(value: f64) -> Self {
        ArgumentValue::Float(value)
    }
}

impl From<&str> for ArgumentValue {
    fn from(value: &str) -> Self {
        ArgumentValue::Text(value.to_string())
    }
}

impl From<String> for ArgumentValue {
    fn from(value: String) -> Self {
        ArgumentValue::Text(value)
    }
}

impl From<&[u8]> for ArgumentValue {
    fn from(value: &[u8]) -> Self {
        ArgumentValue::Bytes(value.to_vec())
    }
}

impl From<Vec<u8>> for ArgumentValue {
    fn from(value: Vec<u8>) -> Self {
        ArgumentValue::Bytes(value)
    }
}

impl fmt::Display for ArgumentValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArgumentValue::Bool(value) => write!(f, "{value}"),
            ArgumentValue::Int(value) => write!(f, "{value}"),
            ArgumentValue::Uint(value) => write!(f, "{value}"),
            ArgumentValue::Float(value) => write!(f, "{value}"),
            ArgumentValue::Text(value) => write!(f, "{value:?}"),
            ArgumentValue::Bytes(value) => write!(f, "{} bytes", value.len()),
        }
    }
}

/// A command to encode: the set that encodes it, the catalog command, its
/// arguments by name, and the native id of the asset it's sent to (for a
/// packet that writes it).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct CommandRequest {
    pub set: String,
    pub command: String,
    pub arguments: Vec<(String, ArgumentValue)>,
    pub target: Option<NativeId>,
}

impl CommandRequest {
    pub fn new(set: impl Into<String>, command: impl Into<String>) -> Self {
        CommandRequest {
            set: set.into(),
            command: command.into(),
            arguments: Vec::new(),
            target: None,
        }
    }

    pub fn with_argument(
        mut self,
        name: impl Into<String>,
        value: impl Into<ArgumentValue>,
    ) -> Self {
        self.arguments.push((name.into(), value.into()));
        self
    }

    pub fn with_target(mut self, target: impl Into<NativeId>) -> Self {
        self.target = Some(target.into());
        self
    }
}

/// An encoded command, ready for its link to frame with the packet's
/// metadata.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Encoded<'a> {
    pub set: &'a Set,
    /// The set artifact's digest, to record with what was sent.
    pub def_version: &'a Digest,
    pub packet: &'a CommandPacket,
    /// The payload. A JSON payload is UTF-8 text. Empty for a packet with
    /// no entries.
    pub payload: Vec<u8>,
    /// Each field's and checksum's raw value, as decoding the payload
    /// gives them.
    pub record: Record<'a>,
    /// Arguments the request left out, which took their defaults, in
    /// catalog order.
    pub defaulted: Vec<&'a str>,
    /// The catalog's deprecation note, if the command is retired.
    pub deprecated: Option<&'a str>,
}

/// Why a command can't be encoded.
#[derive(Debug, Clone, PartialEq, Error)]
#[non_exhaustive]
pub enum EncodeError {
    #[error("family {family} has no set {set}")]
    UnknownSet { family: String, set: String },
    #[error("set {set} encodes no command {command}")]
    UnknownCommand { set: String, command: String },
    #[error("command {command}, argument {argument}: {problem}")]
    Argument {
        command: String,
        argument: String,
        problem: ArgumentProblem,
    },
    #[error("command packet {packet}, {field}: {problem}")]
    Field {
        packet: String,
        field: String,
        problem: FieldProblem,
    },
}

impl EncodeError {
    /// The stable error code, as `spec/encoding.md` lists them.
    pub fn code(&self) -> &'static str {
        match self {
            EncodeError::UnknownSet { .. } => "unknown_set",
            EncodeError::UnknownCommand { .. } => "unknown_command",
            EncodeError::Argument { problem, .. } => problem.code(),
            EncodeError::Field { problem, .. } => problem.code(),
        }
    }
}

/// What's wrong with an argument of a request.
#[derive(Debug, Clone, PartialEq, Error)]
#[non_exhaustive]
pub enum ArgumentProblem {
    #[error("the command has no such argument")]
    Unknown,
    #[error("it's given more than once")]
    Duplicate,
    #[error("it has no default, so the request needs it")]
    Missing,
    #[error("{given} isn't {expected}")]
    WrongType {
        given: String,
        expected: &'static str,
    },
    #[error("{given} isn't a finite number")]
    NonFinite { given: String },
    #[error("{given:?} isn't one of its states")]
    UnknownState { given: String },
    #[error("{given} is outside its range, {range}")]
    OutOfRange { given: String, range: String },
    #[error("the set doesn't encode it, so it can only be its default, {default}")]
    NotDefault { default: String },
}

impl ArgumentProblem {
    pub fn code(&self) -> &'static str {
        match self {
            ArgumentProblem::Unknown => "unknown_argument",
            ArgumentProblem::Duplicate => "duplicate_argument",
            ArgumentProblem::Missing => "missing_argument",
            ArgumentProblem::WrongType { .. } => "wrong_type",
            ArgumentProblem::NonFinite { .. } => "non_finite",
            ArgumentProblem::UnknownState { .. } => "unknown_state",
            ArgumentProblem::OutOfRange { .. } => "out_of_range",
            ArgumentProblem::NotDefault { .. } => "not_default",
        }
    }
}

/// Why a field of the packet can't hold what it's given.
#[derive(Debug, Clone, PartialEq, Error)]
#[non_exhaustive]
pub enum FieldProblem {
    #[error("raw value {raw} doesn't fit; the field holds {holds}")]
    RawRange { raw: String, holds: String },
    #[error("a value of {length} bytes doesn't fit: {reason}")]
    BadLength { length: usize, reason: String },
    #[error("the text can't be written as {encoding}")]
    InvalidText { encoding: &'static str },
    #[error("the value contains its terminator byte {byte:#04x}")]
    ContainsTerminator { byte: u8 },
    #[error("the field holds the target's native id, and the request names no target")]
    MissingTarget,
    #[error("native id {id} isn't {expected}")]
    TargetType { id: String, expected: &'static str },
    /// The set isn't one validation accepts.
    #[error("not a valid definition: {0}")]
    Layout(&'static str),
}

impl FieldProblem {
    pub fn code(&self) -> &'static str {
        match self {
            FieldProblem::RawRange { .. } => "raw_range",
            FieldProblem::BadLength { .. } => "bad_length",
            FieldProblem::InvalidText { .. } => "invalid_text",
            FieldProblem::ContainsTerminator { .. } => "contains_terminator",
            FieldProblem::MissingTarget => "missing_target",
            FieldProblem::TargetType { .. } => "target_type",
            FieldProblem::Layout(_) => "invalid_layout",
        }
    }
}

impl FamilySets {
    /// Encodes a command with the set the request names (an asset's
    /// binding names its set when its family has no version locator).
    pub fn encode(&self, request: &CommandRequest) -> Result<Encoded<'_>, EncodeError> {
        let loaded = self
            .loaded(&request.set)
            .ok_or_else(|| EncodeError::UnknownSet {
                family: self.family().name.clone(),
                set: request.set.clone(),
            })?;
        let set = &loaded.set;
        let unknown = || EncodeError::UnknownCommand {
            set: set.name.clone(),
            command: request.command.clone(),
        };
        let packet = set
            .commands
            .iter()
            .find(|packet| packet.canonical == request.command)
            .ok_or_else(unknown)?;
        let command = loaded
            .catalog
            .commands
            .get(&request.command)
            .ok_or_else(unknown)?;
        let (arguments, defaulted) = check_arguments(&request.command, command, packet, request)?;
        let order = self.byte_order();
        let raws = raw_values(packet, &arguments, request.target.as_ref(), order.is_some())?;
        let (payload, record) = match order {
            Some(order) => write::binary(packet, raws, order)?,
            None => write::json(packet, raws)?,
        };
        Ok(Encoded {
            set,
            def_version: &loaded.def_version,
            packet,
            payload,
            record,
            defaulted,
            deprecated: command.deprecated.as_deref(),
        })
    }
}

/// An argument's value once checked against its type.
#[derive(Debug, Clone, PartialEq)]
enum Checked {
    /// An f64 argument.
    Number(f64),
    /// An i64 or u64 argument.
    Integer(i128),
    Bool(bool),
    /// A str argument, or an enum argument's state.
    Text(String),
    Bytes(Vec<u8>),
}

/// Every argument of the command, checked: given or defaulted, by name, and
/// the names that were defaulted.
fn check_arguments<'a>(
    name: &str,
    command: &'a Command,
    packet: &CommandPacket,
    request: &CommandRequest,
) -> Result<(BTreeMap<&'a str, Checked>, Vec<&'a str>), EncodeError> {
    let error = |argument: &str, problem| EncodeError::Argument {
        command: name.to_string(),
        argument: argument.to_string(),
        problem,
    };
    let mut given: BTreeMap<&str, &ArgumentValue> = BTreeMap::new();
    for (argument, value) in &request.arguments {
        if !command
            .arguments
            .iter()
            .any(|known| &known.name == argument)
        {
            return Err(error(argument, ArgumentProblem::Unknown));
        }
        if given.insert(argument, value).is_some() {
            return Err(error(argument, ArgumentProblem::Duplicate));
        }
    }
    let encoded: BTreeSet<&str> = packet
        .entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Field(field) => field.argument.as_deref(),
            _ => None,
        })
        .collect();
    let mut checked = BTreeMap::new();
    let mut defaulted = Vec::new();
    for argument in &command.arguments {
        let default = argument
            .default
            .as_ref()
            .map(|literal| {
                from_literal(argument, literal).and_then(|value| check(argument, &value))
            })
            .transpose()
            .map_err(|problem| error(&argument.name, problem))?;
        let value = match (given.get(argument.name.as_str()), &default) {
            (Some(value), _) => {
                check(argument, value).map_err(|problem| error(&argument.name, problem))?
            }
            (None, Some(default)) => {
                defaulted.push(argument.name.as_str());
                default.clone()
            }
            (None, None) => return Err(error(&argument.name, ArgumentProblem::Missing)),
        };
        // A set from before the argument was added doesn't encode it, so
        // only its default can be sent with that set.
        if !encoded.contains(argument.name.as_str()) && default.as_ref() != Some(&value) {
            let default = argument
                .default
                .as_ref()
                .map_or_else(|| "none".to_string(), show_literal);
            return Err(error(
                &argument.name,
                ArgumentProblem::NotDefault { default },
            ));
        }
        checked.insert(argument.name.as_str(), value);
    }
    Ok((checked, defaulted))
}

/// A catalog default as a value: bytes are written as lowercase hex, and
/// integers past ±(2^53 − 1) as decimal strings.
fn from_literal(argument: &Argument, literal: &Literal) -> Result<ArgumentValue, ArgumentProblem> {
    let invalid = || ArgumentProblem::WrongType {
        given: show_literal(literal),
        expected: "a default validation accepts",
    };
    Ok(match (argument.ty, literal) {
        (ParamType::Bytes, Literal::Str(hex)) => {
            ArgumentValue::Bytes(from_hex(hex).ok_or_else(invalid)?)
        }
        (ParamType::I64, Literal::Str(digits)) => {
            ArgumentValue::Int(digits.parse().map_err(|_| invalid())?)
        }
        (ParamType::U64, Literal::Str(digits)) => {
            ArgumentValue::Uint(digits.parse().map_err(|_| invalid())?)
        }
        (_, Literal::Bool(value)) => ArgumentValue::Bool(*value),
        (_, Literal::Int(value)) => ArgumentValue::Int(*value),
        (_, Literal::Float(value)) => ArgumentValue::Float(*value),
        (_, Literal::Str(value)) => ArgumentValue::Text(value.clone()),
    })
}

fn from_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let digit = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    };
    hex.as_bytes()
        .chunks(2)
        .map(|pair| match pair {
            [high, low] => Some(digit(*high)? << 4 | digit(*low)?),
            _ => None,
        })
        .collect()
}

fn show_literal(literal: &Literal) -> String {
    match literal {
        Literal::Bool(value) => value.to_string(),
        Literal::Int(value) => value.to_string(),
        Literal::Float(value) => value.to_string(),
        Literal::Str(value) => format!("{value:?}"),
    }
}

/// A value checked against its argument's type, states and range. Numbers
/// compare by value: 3, 3.0 and a u64 3 are the same integer.
fn check(argument: &Argument, value: &ArgumentValue) -> Result<Checked, ArgumentProblem> {
    let wrong = |expected| ArgumentProblem::WrongType {
        given: value.to_string(),
        expected,
    };
    let checked = match (argument.ty, value) {
        (ParamType::F64, ArgumentValue::Float(x)) => Checked::Number(*x),
        (ParamType::F64, ArgumentValue::Int(n)) => {
            Checked::Number(exact_f64(i128::from(*n)).ok_or_else(|| wrong("an f64 exactly"))?)
        }
        (ParamType::F64, ArgumentValue::Uint(n)) => {
            Checked::Number(exact_f64(i128::from(*n)).ok_or_else(|| wrong("an f64 exactly"))?)
        }
        (ParamType::I64 | ParamType::U64, _) => {
            Checked::Integer(integer(value).ok_or_else(|| wrong("an integer"))?)
        }
        (ParamType::Bool, ArgumentValue::Bool(b)) => Checked::Bool(*b),
        (ParamType::Str, ArgumentValue::Text(text)) => Checked::Text(text.clone()),
        (ParamType::Bytes, ArgumentValue::Bytes(bytes)) => Checked::Bytes(bytes.clone()),
        (ParamType::Enum, ArgumentValue::Text(text)) => {
            if !argument.states.contains(text) {
                return Err(ArgumentProblem::UnknownState {
                    given: text.clone(),
                });
            }
            Checked::Text(text.clone())
        }
        (ParamType::F64, _) => return Err(wrong("a number")),
        (ParamType::Bool, _) => return Err(wrong("a bool")),
        (ParamType::Str, _) => return Err(wrong("text")),
        (ParamType::Bytes, _) => return Err(wrong("bytes")),
        (ParamType::Enum, _) => return Err(wrong("the name of a state")),
        (ParamType::Vec3 | ParamType::Quat, _) => {
            return Err(wrong("a scalar; vector arguments aren't supported"));
        }
    };
    match &checked {
        Checked::Number(x) if !x.is_finite() => {
            return Err(ArgumentProblem::NonFinite {
                given: value.to_string(),
            });
        }
        Checked::Number(x) => check_range(argument.range, value, |bound| x.partial_cmp(&bound))?,
        Checked::Integer(n) => {
            let (low, high) = if argument.ty == ParamType::U64 {
                (0, i128::from(u64::MAX))
            } else {
                (i128::from(i64::MIN), i128::from(i64::MAX))
            };
            if !(low..=high).contains(n) {
                return Err(ArgumentProblem::OutOfRange {
                    given: value.to_string(),
                    range: format!("{low} to {high} for its type"),
                });
            }
            check_range(argument.range, value, |bound| Some(compare(*n, bound)))?;
        }
        _ => {}
    }
    Ok(checked)
}

/// The value inside `range`, by `order`, which compares the value with a
/// bound.
fn check_range(
    range: Option<ArgumentRange>,
    value: &ArgumentValue,
    order: impl Fn(f64) -> Option<Ordering>,
) -> Result<(), ArgumentProblem> {
    let Some(range) = range else {
        return Ok(());
    };
    let above_min = range
        .min
        .is_none_or(|min| matches!(order(min), Some(Ordering::Greater | Ordering::Equal)));
    let below_max = range
        .max
        .is_none_or(|max| matches!(order(max), Some(Ordering::Less | Ordering::Equal)));
    if above_min && below_max {
        return Ok(());
    }
    let range = match (range.min, range.max) {
        (Some(min), Some(max)) => format!("{min} to {max}"),
        (Some(min), None) => format!("at least {min}"),
        (None, Some(max)) => format!("at most {max}"),
        (None, None) => "anything".to_string(),
    };
    Err(ArgumentProblem::OutOfRange {
        given: value.to_string(),
        range,
    })
}

/// An integer compared with a finite double, exactly. From 2^53 up a double
/// is an integer, so it converts to i128 without loss (a double past i128
/// saturates, which keeps the order). Below that, the integer either
/// converts exactly or is farther from 0 than the bound, so converting it
/// can't change the order.
fn compare(n: i128, bound: f64) -> Ordering {
    const EXACT: f64 = 9_007_199_254_740_992.0;
    if bound.abs() >= EXACT {
        n.cmp(&(bound as i128))
    } else {
        (n as f64).partial_cmp(&bound).unwrap_or(Ordering::Less)
    }
}

/// The double that holds `n` exactly, if one does.
fn exact_f64(n: i128) -> Option<f64> {
    let x = n as f64;
    (x as i128 == n).then_some(x)
}

/// An integer value: an integer, or a double with no fraction. A double
/// past the i128 range saturates (`as` from a float does), which the
/// caller's type-range check then refuses.
fn integer(value: &ArgumentValue) -> Option<i128> {
    match value {
        ArgumentValue::Int(n) => Some(i128::from(*n)),
        ArgumentValue::Uint(n) => Some(i128::from(*n)),
        ArgumentValue::Float(x) if x.is_finite() && x.fract() == 0.0 => Some(*x as i128),
        _ => None,
    }
}

/// What one field writes: its raw value, and for a binary string or bytes
/// field the bytes it writes (its terminator not included).
#[derive(Debug, Clone)]
struct Raw {
    value: Value<'static>,
    bytes: Vec<u8>,
}

/// Each entry's raw value, by entry index; padding and checksums have none
/// yet.
fn raw_values(
    packet: &CommandPacket,
    arguments: &BTreeMap<&str, Checked>,
    target: Option<&NativeId>,
    binary: bool,
) -> Result<Vec<Option<Raw>>, EncodeError> {
    let fail = |field: &Field, problem| EncodeError::Field {
        packet: packet.name.clone(),
        field: field.name.clone(),
        problem,
    };
    let mut raws: Vec<Option<Raw>> = Vec::with_capacity(packet.entries.len());
    for entry in &packet.entries {
        let Entry::Field(field) = entry else {
            raws.push(None);
            continue;
        };
        let value = if let Some(name) = &field.argument {
            let checked = arguments
                .get(name.as_str())
                .ok_or(FieldProblem::Layout("an argument the command doesn't have"));
            checked.and_then(|checked| from_argument(field, checked, binary))
        } else if let Some(value) = &field.value {
            constant(field, value, binary)
        } else if field.target == Some(true) {
            target
                .ok_or(FieldProblem::MissingTarget)
                .and_then(|id| target_value(field, id, binary))
        } else {
            // A length, filled in below from the field that takes it.
            raws.push(None);
            continue;
        };
        let value = value.map_err(|problem| fail(field, problem))?;
        let bytes = if binary {
            run_bytes(field, &value).map_err(|problem| fail(field, problem))?
        } else {
            Vec::new()
        };
        raws.push(Some(Raw { value, bytes }));
    }
    if binary {
        fill_lengths(packet, &mut raws).map_err(|(field, problem)| fail(field, problem))?;
    }
    Ok(raws)
}

/// Each length field's raw value, from the field that takes its length:
/// `(length - add) / multiply`, which must be a whole number of at least 0.
fn fill_lengths<'p>(
    packet: &'p CommandPacket,
    raws: &mut [Option<Raw>],
) -> Result<(), (&'p Field, FieldProblem)> {
    for (data, entry) in packet.entries.iter().enumerate() {
        let Entry::Field(field) = entry else {
            continue;
        };
        let Some(ByteLength::Field(from)) = &field.length else {
            continue;
        };
        let length = raws
            .get(data)
            .and_then(Option::as_ref)
            .map_or(0, |raw| raw.bytes.len());
        let (length_at, value) =
            length_raw(packet, from, length).map_err(|problem| (field, problem))?;
        if let Some(slot) = raws.get_mut(length_at) {
            *slot = Some(Raw {
                value,
                bytes: Vec::new(),
            });
        }
    }
    Ok(())
}

/// The raw value of the length field `from` names, for a value of `length`
/// bytes, and that field's entry index.
pub(crate) fn length_raw(
    packet: &CommandPacket,
    from: &FromField,
    length: usize,
) -> Result<(usize, Value<'static>), FieldProblem> {
    let found = packet
        .entries
        .iter()
        .enumerate()
        .find_map(|(at, entry)| match entry {
            Entry::Field(field) if field.name == from.field => Some((at, field)),
            _ => None,
        });
    let Some((at, length_field)) = found else {
        return Err(FieldProblem::Layout("a length from a field the packet has"));
    };
    let multiply = i128::from(from.multiply.unwrap_or(1));
    if multiply < 1 {
        return Err(FieldProblem::Layout("a length multiply of at least 1"));
    }
    let add = i128::from(from.add.unwrap_or(0));
    let stored = i128::try_from(length).map_or(-1, |length| length - add);
    let bad = |reason: String| FieldProblem::BadLength { length, reason };
    if stored < 0 || stored % multiply != 0 {
        let base = match add.cmp(&0) {
            Ordering::Greater => format!("(length - {add})"),
            Ordering::Less => format!("(length + {})", -add),
            Ordering::Equal => "length".to_string(),
        };
        let formula = if multiply == 1 {
            base
        } else {
            format!("{base} / {multiply}")
        };
        return Err(bad(format!(
            "{} holds {formula}, which must be a whole number of at least 0",
            from.field
        )));
    }
    let value =
        integer_raw(length_field, stored / multiply, true).map_err(|problem| match problem {
            FieldProblem::RawRange { holds, .. } => bad(format!("{} holds {holds}", from.field)),
            problem => problem,
        })?;
    Ok((at, value))
}

/// An argument's raw value in its field: unconverted, or through the
/// inverse of the field's conversion, rounded to the nearest integer (ties
/// away from zero) for an integer field.
fn from_argument(
    field: &Field,
    value: &Checked,
    binary: bool,
) -> Result<Value<'static>, FieldProblem> {
    let cannot = FieldProblem::Layout("an argument this field can't encode");
    let converted = |x: f64| -> Result<f64, FieldProblem> {
        match (&field.conversion, &field.inverse) {
            (None, _) => Ok(x),
            (Some(Conversion::Linear { scale, offset }), None) => Ok((x - offset) / scale),
            (Some(Conversion::Poly { .. }), Some(inverse)) => {
                convert(inverse, x).ok_or(cannot.clone())
            }
            _ => Err(cannot.clone()),
        }
    };
    match (field.ty, value) {
        (ScalarType::Uint | ScalarType::Int, Checked::Integer(n)) if field.conversion.is_none() => {
            integer_raw(field, *n, binary)
        }
        (ScalarType::Uint | ScalarType::Int, Checked::Bool(b)) => {
            integer_raw(field, i128::from(*b), binary)
        }
        (ScalarType::Uint | ScalarType::Int, Checked::Text(name)) => match &field.conversion {
            Some(Conversion::States { states }) => {
                let state = states
                    .iter()
                    .find(|state| &state.name == name)
                    .ok_or(cannot)?;
                integer_raw(field, i128::from(state.raw), binary)
            }
            _ => Err(cannot),
        },
        (ScalarType::Uint | ScalarType::Int, Checked::Number(x)) => {
            let raw = converted(*x)?;
            match rounded(raw) {
                Some(n) => integer_raw(field, n, binary),
                None => Err(FieldProblem::RawRange {
                    raw: show_double(raw),
                    holds: integer_bounds(field, binary)
                        .map_or_else(String::new, |(low, high)| format!("{low} to {high}")),
                }),
            }
        }
        (ScalarType::Float, Checked::Number(x)) => float_raw(field, converted(*x)?, binary),
        (ScalarType::Bool, Checked::Bool(b)) => Ok(Value::Bool(*b)),
        (ScalarType::String, Checked::Text(text)) => Ok(Value::Str(text.clone())),
        (ScalarType::Bytes, Checked::Bytes(bytes)) => Ok(Value::Bytes(bytes.clone())),
        _ => Err(cannot),
    }
}

/// The nearest integer to `x`, ties away from zero, if `x` is finite and
/// within ±1.7e38 (where a double converts to i128 exactly).
fn rounded(x: f64) -> Option<i128> {
    let r = x.round();
    (r.is_finite() && r.abs() < 1.7e38).then_some(r as i128)
}

/// The integers a uint or int field holds: by its size in a binary payload,
/// 64 bits in a JSON one.
fn integer_bounds(field: &Field, binary: bool) -> Option<(i128, i128)> {
    let size = if binary {
        field.size_bits.filter(|size| (1..=64).contains(size))?
    } else {
        64
    };
    match field.ty {
        ScalarType::Uint => Some((0, (1_i128 << size) - 1)),
        ScalarType::Int => Some((-(1_i128 << (size - 1)), (1_i128 << (size - 1)) - 1)),
        _ => None,
    }
}

/// `n` as a uint or int field's raw value, if the field holds it.
fn integer_raw(field: &Field, n: i128, binary: bool) -> Result<Value<'static>, FieldProblem> {
    let (low, high) = integer_bounds(field, binary)
        .ok_or(FieldProblem::Layout("an integer field with a size"))?;
    let out = || FieldProblem::RawRange {
        raw: n.to_string(),
        holds: format!("{low} to {high}"),
    };
    if !(low..=high).contains(&n) {
        return Err(out());
    }
    match field.ty {
        ScalarType::Uint => u64::try_from(n).map(Value::Uint).map_err(|_| out()),
        _ => i64::try_from(n).map(Value::Int).map_err(|_| out()),
    }
}

/// `x` as a float field's raw value: binary32 fields round it to the
/// nearest binary32 (ties to even, as `as f32` does), which must be finite.
fn float_raw(field: &Field, x: f64, binary: bool) -> Result<Value<'static>, FieldProblem> {
    let out = |holds: String| FieldProblem::RawRange {
        raw: show_double(x),
        holds,
    };
    if !x.is_finite() {
        return Err(out("finite numbers".to_string()));
    }
    if binary && field.size_bits == Some(32) {
        let single = x as f32;
        if !single.is_finite() {
            return Err(out(format!(
                "finite binary32 numbers, up to {} (this one rounds to infinity)",
                show_double(f64::from(f32::MAX))
            )));
        }
        return Ok(Value::Float(f64::from(single)));
    }
    Ok(Value::Float(x))
}

/// A double for a message: as ECMAScript writes it when finite.
fn show_double(x: f64) -> String {
    if x.is_finite() {
        format_double(x)
    } else {
        x.to_string()
    }
}

/// A constant's raw value.
fn constant(field: &Field, value: &Scalar, binary: bool) -> Result<Value<'static>, FieldProblem> {
    match (field.ty, value) {
        (ScalarType::String, Scalar::Str(text)) => Ok(Value::Str(text.clone())),
        (ScalarType::Bool, _) => match integer_value(value) {
            Some(0) => Ok(Value::Bool(false)),
            Some(1) => Ok(Value::Bool(true)),
            _ => Err(FieldProblem::Layout("a bool constant of 0 or 1")),
        },
        (ScalarType::Uint | ScalarType::Int, _) => {
            let n = integer_value(value).ok_or(FieldProblem::Layout("an integer constant"))?;
            integer_raw(field, n, binary)
        }
        _ => Err(FieldProblem::Layout("a constant validation accepts")),
    }
}

/// The target's native id as a target field's raw value: an integer field
/// takes an integer id that fits it, a string field the id's text (an
/// integer id in decimal).
pub(crate) fn target_value(
    field: &Field,
    id: &NativeId,
    binary: bool,
) -> Result<Value<'static>, FieldProblem> {
    match field.ty {
        ScalarType::Uint | ScalarType::Int => {
            let n = id.as_int().ok_or_else(|| FieldProblem::TargetType {
                id: id.to_string(),
                expected: "an integer",
            })?;
            integer_raw(field, n, binary)
        }
        ScalarType::String => Ok(Value::Str(id.to_string())),
        _ => Err(FieldProblem::Layout(
            "a target field is a uint, int or string field",
        )),
    }
}

/// The bytes a binary string or bytes field writes, its terminator not
/// included, once they fit its length; empty for other fields.
pub(crate) fn run_bytes(field: &Field, value: &Value<'_>) -> Result<Vec<u8>, FieldProblem> {
    let bytes = match value {
        Value::Str(text) => text_bytes(text, field.text)?,
        Value::Bytes(bytes) => bytes.clone(),
        _ => return Ok(Vec::new()),
    };
    match &field.length {
        Some(ByteLength::Fixed { bytes: size }) if bytes.len() != *size as usize => {
            Err(FieldProblem::BadLength {
                length: bytes.len(),
                reason: format!("the field is exactly {size} bytes"),
            })
        }
        Some(ByteLength::Terminator { byte }) if bytes.contains(byte) => {
            Err(FieldProblem::ContainsTerminator { byte: *byte })
        }
        _ => Ok(bytes),
    }
}

/// Text in a field's encoding: UTF-8 as it is, ASCII below 0x80, Latin-1
/// one byte per code point up to U+00FF.
fn text_bytes(text: &str, encoding: Option<TextEncoding>) -> Result<Vec<u8>, FieldProblem> {
    match encoding.unwrap_or(TextEncoding::Utf8) {
        TextEncoding::Utf8 => Ok(text.as_bytes().to_vec()),
        TextEncoding::Ascii if text.is_ascii() => Ok(text.as_bytes().to_vec()),
        TextEncoding::Ascii => Err(FieldProblem::InvalidText { encoding: "ASCII" }),
        TextEncoding::Latin1 => text
            .chars()
            .map(|c| u8::try_from(u32::from(c)).ok())
            .collect::<Option<Vec<u8>>>()
            .ok_or(FieldProblem::InvalidText {
                encoding: "Latin-1",
            }),
    }
}

#[cfg(test)]
mod tests;
