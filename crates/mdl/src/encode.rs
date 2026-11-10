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

use crate::bindings::NativeId;
use crate::canonical::format_double;
use crate::convert::inverse;
use crate::decode::{Record, Value};
use crate::ir::{
    Argument, ArgumentRange, ArrayEntry, ByteLength, Command, CommandPacket, Conversion, Count,
    Digest, Element, Entry, Field, FromField, Literal, ParamType, Scalar, ScalarType, Set,
    StructType,
};
use crate::message::MetaValue;
use crate::route::FamilySets;
use crate::text;
use crate::units::convert_between;
use crate::validate::rules::integer_value;

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
    /// A list argument's values.
    List(Vec<ArgumentValue>),
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

impl From<Vec<ArgumentValue>> for ArgumentValue {
    fn from(values: Vec<ArgumentValue>) -> Self {
        ArgumentValue::List(values)
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
            ArgumentValue::List(values) => write!(f, "a list of {}", count_of(values.len())),
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

impl Encoded<'_> {
    /// The packet's metadata as a link provides it with the payload: what
    /// a sender frames the command with, what command history keeps with
    /// it, and what decoding it again needs (`spec/encoding.md` §7.1).
    pub fn metadata(&self) -> Vec<(&str, MetaValue<'_>)> {
        self.packet
            .metadata
            .iter()
            .map(|(name, value)| {
                let value = match value {
                    Scalar::Int(n) => MetaValue::Int(*n),
                    Scalar::Str(text) => MetaValue::Text(text),
                };
                (name.as_str(), value)
            })
            .collect()
    }
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
    /// Decoding: a raw value its field's states don't list.
    #[error("raw value {raw} names none of its states")]
    NoState { raw: String },
    #[error("{given} is outside its range, {range}")]
    OutOfRange { given: String, range: String },
    /// Decoding: a converted argument's raw value outside what its range
    /// encodes to.
    #[error("raw value {raw} is outside {low} to {high}, the raw values its range encodes to")]
    RawOutOfRange {
        raw: String,
        low: String,
        high: String,
    },
    /// Decoding: a raw value outside its field's table or spline.
    #[error("raw value {raw} is outside its field's calibration")]
    OutsideCalibration { raw: String },
    #[error("the set doesn't encode it, so it can only be its default, {default}")]
    NotDefault { default: String },
    #[error("a list of {} doesn't fit: it takes {bounds}", count_of(*count))]
    ListLength { count: usize, bounds: String },
    /// One value of a list argument.
    #[error("value {index}: {problem}")]
    Item {
        index: usize,
        problem: Box<ArgumentProblem>,
    },
}

impl ArgumentProblem {
    pub fn code(&self) -> &'static str {
        match self {
            ArgumentProblem::Unknown => "unknown_argument",
            ArgumentProblem::Duplicate => "duplicate_argument",
            ArgumentProblem::Missing => "missing_argument",
            ArgumentProblem::WrongType { .. } => "wrong_type",
            ArgumentProblem::NonFinite { .. } => "non_finite",
            ArgumentProblem::UnknownState { .. } | ArgumentProblem::NoState { .. } => {
                "unknown_state"
            }
            ArgumentProblem::OutOfRange { .. }
            | ArgumentProblem::RawOutOfRange { .. }
            | ArgumentProblem::OutsideCalibration { .. } => "out_of_range",
            ArgumentProblem::NotDefault { .. } => "not_default",
            ArgumentProblem::ListLength { .. } => "list_length",
            ArgumentProblem::Item { problem, .. } => problem.code(),
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
    #[error("an array of {} doesn't fit: {reason}", count_of(*count))]
    BadCount { count: usize, reason: String },
    /// One element of an array, or one field of an element's record.
    #[error("{}", element_text(*index, field.as_deref(), problem))]
    Element {
        index: usize,
        field: Option<String>,
        problem: Box<FieldProblem>,
    },
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
            FieldProblem::BadCount { .. } => "bad_count",
            FieldProblem::Element { problem, .. } => problem.code(),
            FieldProblem::InvalidText { .. } => "invalid_text",
            FieldProblem::ContainsTerminator { .. } => "contains_terminator",
            FieldProblem::MissingTarget => "missing_target",
            FieldProblem::TargetType { .. } => "target_type",
            FieldProblem::Layout(_) => "invalid_layout",
        }
    }
}

/// `count` values, in words.
fn count_of(count: usize) -> String {
    match count {
        1 => "1 value".to_string(),
        _ => format!("{count} values"),
    }
}

fn element_text(index: usize, field: Option<&str>, problem: &FieldProblem) -> String {
    match field {
        Some(field) => format!("element {index}, field {field}: {problem}"),
        None => format!("element {index}: {problem}"),
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
        let (arguments, defaulted) =
            check_arguments(&request.command, command, packet, &set.types, request)?;
        let order = self.byte_order();
        let slots = raw_values(
            packet,
            &set.types,
            &arguments,
            request.target.as_ref(),
            order.is_some(),
        )?;
        let (payload, record) = match order {
            Some(order) => write::binary(packet, &set.types, slots, order)?,
            None => write::json(packet, &set.types, slots)?,
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
    /// A list argument's values.
    List(Vec<Checked>),
}

/// Each argument's checked value and unit, by name.
type Arguments<'a> = BTreeMap<&'a str, (Checked, Option<&'a str>)>;

/// Every argument of the command, checked: given or defaulted, by name, with
/// its unit, and the names that were defaulted.
fn check_arguments<'a>(
    name: &str,
    command: &'a Command,
    packet: &CommandPacket,
    types: &BTreeMap<String, StructType>,
    request: &CommandRequest,
) -> Result<(Arguments<'a>, Vec<&'a str>), EncodeError> {
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
    let encoded = encoded_arguments(packet, types);
    let mut checked = BTreeMap::new();
    let mut defaulted = Vec::new();
    for argument in &command.arguments {
        let default = argument
            .default
            .as_ref()
            .map(|literal| {
                from_literal(argument, literal).and_then(|value| check_given(argument, &value))
            })
            .transpose()
            .map_err(|problem| error(&argument.name, problem))?;
        let value = match (given.get(argument.name.as_str()), &default) {
            (Some(value), _) => {
                check_given(argument, value).map_err(|problem| error(&argument.name, problem))?
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
        checked.insert(argument.name.as_str(), (value, argument.unit.as_deref()));
    }
    Ok((checked, defaulted))
}

/// The arguments a packet encodes: by its fields, its arrays, and the
/// fields of its arrays' records.
pub(crate) fn encoded_arguments<'p>(
    packet: &'p CommandPacket,
    types: &'p BTreeMap<String, StructType>,
) -> BTreeSet<&'p str> {
    let mut encoded = BTreeSet::new();
    for entry in &packet.entries {
        match entry {
            Entry::Field(field) => encoded.extend(field.argument.as_deref()),
            Entry::Array(array) => {
                encoded.extend(array.argument.as_deref());
                if let Element::Struct { name } = &array.element {
                    let record = types.get(name).map_or(&[][..], |t| t.entries.as_slice());
                    for entry in record {
                        if let Entry::Field(field) = entry {
                            encoded.extend(field.argument.as_deref());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    encoded
}

/// Whether the catalog allows `value` as one value of `argument`: its type,
/// states and range, as encoding checks them (each of a list argument's
/// values is one).
pub(crate) fn allowed(argument: &Argument, value: &ArgumentValue) -> Result<(), ArgumentProblem> {
    check(argument, value).map(|_| ())
}

/// Whether a list argument takes `count` values.
pub(crate) fn allowed_count(argument: &Argument, count: usize) -> Result<(), ArgumentProblem> {
    let Some(list) = argument.list else {
        return Ok(());
    };
    let fits = |bound: Option<u32>, keep: fn(usize, usize) -> bool| {
        bound.is_none_or(|bound| usize::try_from(bound).is_ok_and(|bound| keep(count, bound)))
    };
    if fits(list.min, |count, min| count >= min) && fits(list.max, |count, max| count <= max) {
        return Ok(());
    }
    let bounds = match (list.min, list.max) {
        (Some(min), Some(max)) if min == max => format!("exactly {min}"),
        (Some(min), Some(max)) => format!("{min} to {max}"),
        (Some(min), None) => format!("at least {min}"),
        (None, Some(max)) => format!("at most {max}"),
        (None, None) => "any number".to_string(),
    };
    Err(ArgumentProblem::ListLength { count, bounds })
}

/// A given value checked against its argument: a single value, or for a
/// list argument a list of them, each checked and named by its index.
fn check_given(argument: &Argument, value: &ArgumentValue) -> Result<Checked, ArgumentProblem> {
    let wrong = |expected| ArgumentProblem::WrongType {
        given: value.to_string(),
        expected,
    };
    match (argument.list, value) {
        (None, ArgumentValue::List(_)) => Err(wrong("a single value")),
        (None, _) => check(argument, value),
        (Some(_), ArgumentValue::List(values)) => {
            allowed_count(argument, values.len())?;
            let mut checked = Vec::with_capacity(values.len());
            for (index, value) in values.iter().enumerate() {
                let one = check(argument, value).map_err(|problem| ArgumentProblem::Item {
                    index,
                    problem: Box::new(problem),
                })?;
                checked.push(one);
            }
            Ok(Checked::List(checked))
        }
        (Some(_), _) => Err(wrong("a list")),
    }
}

/// An argument's default as a value, if it has one.
pub(crate) fn default_of(argument: &Argument) -> Option<Result<ArgumentValue, ArgumentProblem>> {
    argument
        .default
        .as_ref()
        .map(|literal| from_literal(argument, literal))
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
        // An f64 default is a number whatever its spelling; integer
        // literals are within ±(2^53 - 1), so this is exact.
        (ParamType::F64, Literal::Int(value)) => ArgumentValue::Float(*value as f64),
        (_, Literal::Bool(value)) => ArgumentValue::Bool(*value),
        (_, Literal::Int(value)) => ArgumentValue::Int(*value),
        (_, Literal::Float(value)) => ArgumentValue::Float(*value),
        (_, Literal::Str(value)) => ArgumentValue::Text(value.clone()),
        (_, Literal::List(items)) => ArgumentValue::List(
            items
                .iter()
                .map(|item| from_literal(argument, item))
                .collect::<Result<_, _>>()?,
        ),
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

/// A catalog literal for a message, as the catalog's JSON writes it.
pub(crate) fn show_literal(literal: &Literal) -> String {
    match literal {
        Literal::Bool(value) => value.to_string(),
        Literal::Int(value) => value.to_string(),
        Literal::Float(value) => show_double(*value),
        Literal::Str(value) => format!("{value:?}"),
        Literal::List(items) => {
            let shown: Vec<String> = items.iter().map(show_literal).collect();
            format!("[{}]", shown.join(", "))
        }
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
pub(crate) fn compare(n: i128, bound: f64) -> Ordering {
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

/// What one entry writes, once worked out.
#[derive(Debug, Clone, Default)]
enum Slot {
    /// Padding, or a value worked out later: a length, a count, a checksum.
    #[default]
    Empty,
    /// A field's or checksum's raw value.
    Value(Raw),
    /// An array's elements: a scalar element's `Value`, a record's `Record`.
    Elements(Vec<Slot>),
    /// A record's entries, by its struct type's entry index.
    Record(Vec<Slot>),
}

impl Slot {
    fn raw(&self) -> Option<&Raw> {
        match self {
            Slot::Value(raw) => Some(raw),
            _ => None,
        }
    }
}

/// Where a field's raw value comes from.
enum Source<'a> {
    /// One argument value, in the argument's unit.
    Argument(&'a Checked, Option<&'a str>),
    Constant(&'a Scalar),
    Target(Option<&'a NativeId>),
}

/// Each entry's raw values, by entry index; padding and checksums have
/// none yet.
fn raw_values(
    packet: &CommandPacket,
    types: &BTreeMap<String, StructType>,
    arguments: &Arguments<'_>,
    target: Option<&NativeId>,
    binary: bool,
) -> Result<Vec<Slot>, EncodeError> {
    let fail = |name: &str, problem| EncodeError::Field {
        packet: packet.name.clone(),
        field: name.to_string(),
        problem,
    };
    let mut slots = Vec::with_capacity(packet.entries.len());
    for entry in &packet.entries {
        let slot = match entry {
            Entry::Field(field) => {
                let source = if let Some(name) = &field.argument {
                    let (checked, unit) = arguments
                        .get(name.as_str())
                        .ok_or_else(|| fail(&field.name, LAYOUT_ARGUMENT))?;
                    Source::Argument(checked, *unit)
                } else if let Some(value) = &field.value {
                    Source::Constant(value)
                } else if field.target == Some(true) {
                    Source::Target(target)
                } else {
                    // A length or a count, filled in below from what it
                    // measures.
                    slots.push(Slot::Empty);
                    continue;
                };
                let raw = scalar_raw(field, source, binary)
                    .map_err(|problem| fail(&field.name, problem))?;
                Slot::Value(raw)
            }
            Entry::Array(array) => array_slot(array, types, arguments, binary)
                .map_err(|problem| fail(&array.name, problem))?,
            _ => Slot::Empty,
        };
        slots.push(slot);
    }
    if binary {
        fill_lengths(packet, &mut slots).map_err(|(name, problem)| fail(name, problem))?;
        fill_counts(packet, &mut slots).map_err(|(name, problem)| fail(name, problem))?;
    }
    Ok(slots)
}

const LAYOUT_ARGUMENT: FieldProblem = FieldProblem::Layout("an argument the command doesn't have");

/// A field's raw value from its source, and in a binary payload the bytes
/// a string or bytes value writes.
fn scalar_raw(field: &Field, source: Source<'_>, binary: bool) -> Result<Raw, FieldProblem> {
    let value = match source {
        Source::Argument(checked, unit) => from_argument(field, checked, unit, binary)?,
        Source::Constant(value) => constant(field, value, binary)?,
        Source::Target(id) => target_value(field, id.ok_or(FieldProblem::MissingTarget)?, binary)?,
    };
    let bytes = if binary {
        run_bytes(field, &value)?
    } else {
        Vec::new()
    };
    Ok(Raw { value, bytes })
}

/// An array's elements: one per value of its list argument, or one record
/// per value of the list arguments its records' fields take, which have
/// the same length.
fn array_slot(
    array: &ArrayEntry,
    types: &BTreeMap<String, StructType>,
    arguments: &Arguments<'_>,
    binary: bool,
) -> Result<Slot, FieldProblem> {
    let list = |name: &str| match arguments.get(name) {
        Some((Checked::List(values), unit)) => Ok((values.as_slice(), *unit)),
        _ => Err(FieldProblem::Layout("a list argument")),
    };
    let mut elements = Vec::new();
    match &array.element {
        Element::Scalar(element) => {
            let name = array.argument.as_deref().ok_or(FieldProblem::Layout(
                "an array that encodes a list argument",
            ))?;
            let (values, unit) = list(name)?;
            let field = element.as_field(&array.name);
            for (index, value) in values.iter().enumerate() {
                let raw = scalar_raw(&field, Source::Argument(value, unit), binary)
                    .map_err(|problem| in_element(index, None, problem))?;
                elements.push(Slot::Value(raw));
            }
        }
        Element::Struct { name } => {
            let record = types
                .get(name)
                .ok_or(FieldProblem::Layout("a struct type the set has"))?;
            let mut lists: Vec<(&str, &[Checked], Option<&str>)> = Vec::new();
            for entry in &record.entries {
                if let Entry::Field(Field {
                    argument: Some(argument),
                    ..
                }) = entry
                {
                    let (values, unit) = list(argument)?;
                    lists.push((argument, values, unit));
                }
            }
            let Some(&(first, first_values, _)) = lists.first() else {
                return Err(FieldProblem::Layout("records that take a list argument"));
            };
            let count = first_values.len();
            if let Some((other, values, _)) = lists.iter().find(|(_, v, _)| v.len() != count) {
                return Err(FieldProblem::BadCount {
                    count: values.len(),
                    reason: format!(
                        "list {other} has {} and list {first} {count}, and lists that fill the \
                         same records have the same length",
                        count_of(values.len())
                    ),
                });
            }
            for index in 0..count {
                let mut slots = Vec::with_capacity(record.entries.len());
                for entry in &record.entries {
                    let Entry::Field(field) = entry else {
                        slots.push(Slot::Empty);
                        continue;
                    };
                    let source = match (&field.argument, &field.value) {
                        (Some(argument), _) => {
                            let (_, values, unit) = lists
                                .iter()
                                .find(|(name, _, _)| name == argument)
                                .ok_or(FieldProblem::Layout("a list argument"))?;
                            let value = values
                                .get(index)
                                .ok_or(FieldProblem::Layout("lists of one length"))?;
                            Source::Argument(value, *unit)
                        }
                        (None, Some(value)) => Source::Constant(value),
                        (None, None) => {
                            return Err(FieldProblem::Layout("a record field with a value"));
                        }
                    };
                    let raw = scalar_raw(field, source, binary)
                        .map_err(|problem| in_element(index, Some(&field.name), problem))?;
                    slots.push(Slot::Value(raw));
                }
                elements.push(Slot::Record(slots));
            }
        }
    }
    Ok(Slot::Elements(elements))
}

/// A problem with one element of an array, or one field of its record.
fn in_element(index: usize, field: Option<&str>, problem: FieldProblem) -> FieldProblem {
    FieldProblem::Element {
        index,
        field: field.map(str::to_string),
        problem: Box::new(problem),
    }
}

/// Each length field's raw value, from the field that takes its length:
/// `(length - add) / multiply`, which must be a whole number of at least 0.
fn fill_lengths<'p>(
    packet: &'p CommandPacket,
    slots: &mut [Slot],
) -> Result<(), (&'p str, FieldProblem)> {
    for (data, entry) in packet.entries.iter().enumerate() {
        let Entry::Field(field) = entry else {
            continue;
        };
        let Some(ByteLength::Field(from)) = &field.length else {
            continue;
        };
        let length = slots
            .get(data)
            .and_then(Slot::raw)
            .map_or(0, |raw| raw.bytes.len());
        let (length_at, value) =
            length_raw(packet, from, length).map_err(|problem| (field.name.as_str(), problem))?;
        if let Some(slot) = slots.get_mut(length_at) {
            *slot = Slot::Value(Raw {
                value,
                bytes: Vec::new(),
            });
        }
    }
    Ok(())
}

/// Each array's count: exactly its fixed count, or its count field's raw
/// value `(count - add) / multiply`, which every array it counts must give
/// the same.
fn fill_counts<'p>(
    packet: &'p CommandPacket,
    slots: &mut [Slot],
) -> Result<(), (&'p str, FieldProblem)> {
    let mut given: BTreeMap<usize, (&str, Value<'static>)> = BTreeMap::new();
    for (index, entry) in packet.entries.iter().enumerate() {
        let Entry::Array(array) = entry else {
            continue;
        };
        let count = match slots.get(index) {
            Some(Slot::Elements(elements)) => elements.len(),
            _ => 0,
        };
        let fail = |problem| (array.name.as_str(), problem);
        match &array.count {
            Some(Count::Fixed { count: fixed }) if usize::try_from(*fixed) != Ok(count) => {
                return Err(fail(FieldProblem::BadCount {
                    count,
                    reason: format!("the array holds exactly {fixed}"),
                }));
            }
            Some(Count::Field(from)) => {
                let (at, value) = count_raw(packet, from, count).map_err(fail)?;
                match given.get(&at) {
                    Some((first, earlier)) if *earlier != value => {
                        return Err(fail(FieldProblem::BadCount {
                            count,
                            reason: format!(
                                "{} counts array {first} too, which gives it {}, and this array \
                                 gives it {}; arrays that share a count hold as many values",
                                from.field,
                                show_value(earlier),
                                show_value(&value)
                            ),
                        }));
                    }
                    Some(_) => {}
                    None => {
                        given.insert(at, (array.name.as_str(), value.clone()));
                        if let Some(slot) = slots.get_mut(at) {
                            *slot = Slot::Value(Raw {
                                value,
                                bytes: Vec::new(),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn show_value(value: &Value<'_>) -> String {
    match value {
        Value::Uint(n) => n.to_string(),
        Value::Int(n) => n.to_string(),
        _ => "a value".to_string(),
    }
}

/// The raw value of the length field `from` names, for a value of `length`
/// bytes, and that field's entry index.
pub(crate) fn length_raw(
    packet: &CommandPacket,
    from: &FromField,
    length: usize,
) -> Result<(usize, Value<'static>), FieldProblem> {
    measure(packet, from, length, "length", |reason| {
        FieldProblem::BadLength { length, reason }
    })
}

/// The raw value of the count field `from` names, for an array of `count`
/// elements, and that field's entry index.
fn count_raw(
    packet: &CommandPacket,
    from: &FromField,
    count: usize,
) -> Result<(usize, Value<'static>), FieldProblem> {
    measure(packet, from, count, "count", |reason| {
        FieldProblem::BadCount { count, reason }
    })
}

/// The raw value `(n - add) / multiply` of the field `from` names, which
/// must be a whole number of at least 0 that fits it, and that field's
/// entry index. `noun` is what `n` is; `bad` says why it doesn't fit.
fn measure(
    packet: &CommandPacket,
    from: &FromField,
    n: usize,
    noun: &str,
    bad: impl Fn(String) -> FieldProblem,
) -> Result<(usize, Value<'static>), FieldProblem> {
    let found = packet
        .entries
        .iter()
        .enumerate()
        .find_map(|(at, entry)| match entry {
            Entry::Field(field) if field.name == from.field => Some((at, field)),
            _ => None,
        });
    let Some((at, measuring)) = found else {
        return Err(FieldProblem::Layout(
            "a length or count from a field the packet has",
        ));
    };
    let multiply = i128::from(from.multiply.unwrap_or(1));
    if multiply < 1 {
        return Err(FieldProblem::Layout("a multiply of at least 1"));
    }
    let add = i128::from(from.add.unwrap_or(0));
    let stored = i128::try_from(n).map_or(-1, |n| n - add);
    if stored < 0 || stored % multiply != 0 {
        let base = match add.cmp(&0) {
            Ordering::Greater => format!("({noun} - {add})"),
            Ordering::Less => format!("({noun} + {})", -add),
            Ordering::Equal => noun.to_string(),
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
        integer_raw(measuring, stored / multiply, true).map_err(|problem| match problem {
            FieldProblem::RawRange { holds, .. } => bad(format!("{} holds {holds}", from.field)),
            problem => problem,
        })?;
    Ok((at, value))
}

/// An argument's raw value in its field: unconverted, or through the
/// inverse of the field's conversion, rounded to the nearest integer (ties
/// away from zero) for an integer field. A number goes from the argument's
/// `unit` into the field's first.
fn from_argument(
    field: &Field,
    value: &Checked,
    unit: Option<&str>,
    binary: bool,
) -> Result<Value<'static>, FieldProblem> {
    let cannot = FieldProblem::Layout("an argument this field can't encode");
    let converted = |x: f64| {
        convert_between(x, unit, field.unit.as_deref())
            .and_then(|x| inverse(field, x).number())
            .ok_or(cannot.clone())
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
                let raw = integer_value(&state.raw).ok_or(FieldProblem::Layout(
                    "a state whose raw value is an integer",
                ))?;
                integer_raw(field, raw, binary)
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

/// The raw value an f64 argument of `x`, in `unit`, becomes in `field`
/// before it's checked against the field: into the field's unit, through
/// the inverse conversion, then rounded as §3.1 rounds it (to an integer,
/// ties away from zero, for an integer field; to the nearest binary32 for a
/// 32-bit float field). Infinite where the arithmetic overflows; `None` for
/// a field that can't encode a number.
pub(crate) fn raw_unchecked(
    field: &Field,
    x: f64,
    unit: Option<&str>,
    binary: bool,
) -> Option<f64> {
    let x = convert_between(x, unit, field.unit.as_deref())?;
    let raw = inverse(field, x).number()?;
    Some(match field.ty {
        ScalarType::Uint | ScalarType::Int => raw.round(),
        ScalarType::Float if binary && field.size_bits == Some(32) => f64::from(raw as f32),
        _ => raw,
    })
}

/// Why `field` of `packet` can't write `literal`, a value the catalog gives
/// `argument` (its default, one value of a list default, a state's name),
/// as encoding would find it: §3.1 to §3.4, then §3.5 for a field whose
/// length another field holds. `None` when it can, and when the catalog
/// doesn't allow the value, which validation reports on the catalog.
pub(crate) fn write_problem(
    packet: &CommandPacket,
    field: &Field,
    argument: &Argument,
    literal: &Literal,
    binary: bool,
) -> Option<FieldProblem> {
    let value = from_literal(argument, literal).ok()?;
    let checked = check(argument, &value).ok()?;
    let source = Source::Argument(&checked, argument.unit.as_deref());
    let raw = match scalar_raw(field, source, binary) {
        Ok(raw) => raw,
        Err(problem) => return Some(problem),
    };
    match (&field.length, binary) {
        (Some(ByteLength::Field(from)), true) => length_raw(packet, from, raw.bytes.len()).err(),
        _ => None,
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
        Value::Str(text) => text::encode(text, field.text).ok_or(FieldProblem::InvalidText {
            encoding: text::name(field.text),
        })?,
        Value::Bytes(bytes) => bytes.clone(),
        _ => return Ok(Vec::new()),
    };
    match &field.length {
        Some(ByteLength::Fixed {
            bytes: size,
            terminator: None,
        }) if bytes.len() != *size as usize => Err(FieldProblem::BadLength {
            length: bytes.len(),
            reason: format!("the field is exactly {size} bytes"),
        }),
        Some(ByteLength::Fixed {
            bytes: size,
            terminator: Some(_),
        }) if bytes.len() > *size as usize => Err(FieldProblem::BadLength {
            length: bytes.len(),
            reason: format!("the field holds at most {size} bytes"),
        }),
        Some(
            ByteLength::Terminator { byte }
            | ByteLength::Fixed {
                terminator: Some(byte),
                ..
            },
        ) if bytes.contains(byte) => Err(FieldProblem::ContainsTerminator { byte: *byte }),
        _ => Ok(bytes),
    }
}

#[cfg(test)]
mod tests;
