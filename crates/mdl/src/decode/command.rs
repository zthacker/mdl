//! Decoding commands: a command payload back into its catalog command and
//! arguments, as `spec/encoding.md` §7 describes. What a simulated asset
//! does with a command it receives, and what command history records.

use std::collections::BTreeMap;

use thiserror::Error;

use super::{DecodeError, Problem, Record, Value};
use crate::bindings::NativeId;
use crate::convert::{Converted, convert};
use std::cmp::Ordering;

use crate::encode::{
    ArgumentProblem, ArgumentValue, CommandRequest, allowed, allowed_count, compare, default_of,
    raw_unchecked,
};
use crate::ir::{
    Argument, ArgumentRange, ArrayEntry, CommandPacket, Conversion, Digest, Element, Entry, Field,
    ParamType, Scalar, ScalarElement, Set, StructType,
};
use crate::message::Message;
use crate::route::FamilySets;
use crate::units::convert_between;
use crate::validate::rules::integer_value;

/// A decoded command.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct DecodedCommand<'a> {
    pub set: &'a Set,
    /// The set artifact's digest.
    pub def_version: &'a Digest,
    pub packet: &'a CommandPacket,
    /// Every argument of the catalog command, in catalog order, in
    /// engineering units. Together with the target they're a request that
    /// encodes to the same payload.
    pub arguments: Vec<(&'a str, ArgumentValue)>,
    /// Arguments the set doesn't encode, which have their defaults.
    pub defaulted: Vec<&'a str>,
    /// The target's native id, for a packet that holds it.
    pub target: Option<NativeId>,
    /// Each field's and checksum's raw value.
    pub record: Record<'a>,
}

impl DecodedCommand<'_> {
    /// The canonical command.
    pub fn command(&self) -> &str {
        &self.packet.canonical
    }

    /// The request these arguments and target make. It encodes to the same
    /// payload (anything past the packet aside) when each converted field's
    /// inverse undoes its conversion, as `spec/encoding.md` §7.4 says;
    /// otherwise to the raw values the decoded arguments mean.
    pub fn request(&self) -> CommandRequest {
        let mut request = CommandRequest::new(self.set.name.as_str(), self.command());
        for (name, value) in &self.arguments {
            request = request.with_argument(*name, value.clone());
        }
        request.target = self.target.clone();
        request
    }
}

/// Why a command payload can't be decoded.
#[derive(Debug, Clone, PartialEq, Error)]
#[non_exhaustive]
pub enum CommandDecodeError {
    #[error("family {family} has no set {set}")]
    UnknownSet { family: String, set: String },
    #[error("no command packet of set {set} matches the payload")]
    NoPacket { set: String },
    /// The payload can't be read, or an entry of the packet can't be.
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error("command packet {packet}, {field}: it holds {found}, not its constant {expected}")]
    Constant {
        packet: String,
        field: String,
        expected: String,
        found: String,
    },
    #[error("command {command}, argument {argument}: {problem}")]
    Argument {
        command: String,
        argument: String,
        problem: ArgumentProblem,
    },
}

impl CommandDecodeError {
    /// The stable error code, as `spec/encoding.md` lists them.
    pub fn code(&self) -> &'static str {
        match self {
            CommandDecodeError::UnknownSet { .. } => "unknown_set",
            CommandDecodeError::NoPacket { .. } => "no_packet",
            CommandDecodeError::Decode(error) => error.code(),
            CommandDecodeError::Constant { .. } => "wrong_constant",
            CommandDecodeError::Argument { problem, .. } => problem.code(),
        }
    }
}

impl FamilySets {
    /// Decodes a command payload with the set the caller names: which of
    /// its command packets the payload is, by their constants and metadata,
    /// then each argument. A value the catalog doesn't allow is refused, as
    /// encoding refuses it.
    pub fn decode_command(
        &self,
        set: &str,
        message: &Message<'_>,
    ) -> Result<DecodedCommand<'_>, CommandDecodeError> {
        let loaded = self
            .loaded(set)
            .ok_or_else(|| CommandDecodeError::UnknownSet {
                family: self.family().name.clone(),
                set: set.to_string(),
            })?;
        let readable = self.readable(message).map_err(DecodeError::from)?;
        let found =
            loaded
                .program
                .decode_command(&loaded.set, &self.family().name, message, &readable)?;
        let (packet, record) = found.ok_or_else(|| CommandDecodeError::NoPacket {
            set: set.to_string(),
        })?;
        check_constants(packet, &loaded.set.types, &record)?;
        let invalid = |reason| {
            CommandDecodeError::Decode(DecodeError::Entry {
                packet: packet.name.clone(),
                path: String::new(),
                problem: Problem::Layout(reason),
            })
        };
        let command = loaded
            .catalog
            .commands
            .get(&packet.canonical)
            .ok_or_else(|| invalid("a command the catalog has"))?;
        let binary = self.byte_order().is_some();
        let mut arguments = Vec::with_capacity(command.arguments.len());
        let mut defaulted = Vec::new();
        for argument in &command.arguments {
            let refused = |problem| CommandDecodeError::Argument {
                command: packet.canonical.clone(),
                argument: argument.name.clone(),
                problem,
            };
            let value = match encoder(packet, &loaded.set.types, &argument.name) {
                Some(Encoder::Field(field)) => {
                    let raw = record
                        .get(&field.name)
                        .ok_or_else(|| invalid("a value for every field"))?;
                    argument_value(field, argument, raw, binary).map_err(refused)?
                }
                Some(Encoder::Elements(array, element)) => {
                    let field = element.as_field(&array.name);
                    let items = elements(&record, array).ok_or_else(|| invalid("an array"))?;
                    list_value(argument, items, |raw| {
                        argument_value(&field, argument, raw, binary)
                    })
                    .map_err(refused)?
                }
                Some(Encoder::Records(array, field)) => {
                    let items = elements(&record, array).ok_or_else(|| invalid("an array"))?;
                    list_value(argument, items, |item| match item {
                        Value::Record(fields) => {
                            let raw =
                                fields.get(&field.name).ok_or(ArgumentProblem::WrongType {
                                    given: show(item),
                                    expected: "a record with every field",
                                })?;
                            argument_value(field, argument, raw, binary)
                        }
                        _ => Err(ArgumentProblem::WrongType {
                            given: show(item),
                            expected: "a record",
                        }),
                    })
                    .map_err(refused)?
                }
                None => {
                    defaulted.push(argument.name.as_str());
                    default_of(argument)
                        .ok_or_else(|| invalid("a default for an argument the set leaves out"))?
                        .map_err(refused)?
                }
            };
            arguments.push((argument.name.as_str(), value));
        }
        Ok(DecodedCommand {
            set: &loaded.set,
            def_version: &loaded.def_version,
            packet,
            arguments,
            defaulted,
            target: target(packet, &record),
            record,
        })
    }
}

/// What encodes an argument in a packet.
enum Encoder<'p> {
    Field(&'p Field),
    /// An array whose scalar elements are the argument's values.
    Elements(&'p ArrayEntry, &'p ScalarElement),
    /// An array of records whose field holds the argument's values.
    Records(&'p ArrayEntry, &'p Field),
}

/// What encodes an argument, if the packet does.
fn encoder<'p>(
    packet: &'p CommandPacket,
    types: &'p BTreeMap<String, StructType>,
    argument: &str,
) -> Option<Encoder<'p>> {
    let named = |name: &Option<String>| name.as_deref() == Some(argument);
    packet.entries.iter().find_map(|entry| match entry {
        Entry::Field(field) if named(&field.argument) => Some(Encoder::Field(field)),
        Entry::Array(array) => match &array.element {
            Element::Scalar(element) if named(&array.argument) => {
                Some(Encoder::Elements(array, element))
            }
            Element::Struct { name } => {
                types
                    .get(name)?
                    .entries
                    .iter()
                    .find_map(|entry| match entry {
                        Entry::Field(field) if named(&field.argument) => {
                            Some(Encoder::Records(array, field))
                        }
                        _ => None,
                    })
            }
            Element::Scalar(_) => None,
        },
        _ => None,
    })
}

/// An array's decoded elements.
fn elements<'r>(record: &'r Record<'_>, array: &ArrayEntry) -> Option<&'r [Value<'r>]> {
    match record.get(&array.name)? {
        Value::Array(items) => Some(items),
        _ => None,
    }
}

/// A list argument from its elements: a length the list allows, then each
/// element through `one`, named by its index when refused.
fn list_value<'v>(
    argument: &Argument,
    items: &'v [Value<'v>],
    one: impl Fn(&'v Value<'v>) -> Result<ArgumentValue, ArgumentProblem>,
) -> Result<ArgumentValue, ArgumentProblem> {
    allowed_count(argument, items.len())?;
    let mut values = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let value = one(item).map_err(|problem| ArgumentProblem::Item {
            index,
            problem: Box::new(problem),
        })?;
        values.push(value);
    }
    Ok(ArgumentValue::List(values))
}

/// Every constant field holds its value, those that identified the packet
/// and those that didn't (after a value of varying length, say), and in
/// every record of an array.
fn check_constants(
    packet: &CommandPacket,
    types: &BTreeMap<String, StructType>,
    record: &Record<'_>,
) -> Result<(), CommandDecodeError> {
    let wrong = |field: &Field, found: Option<&Value<'_>>, expected: &Scalar| {
        CommandDecodeError::Constant {
            packet: packet.name.clone(),
            field: field.name.clone(),
            expected: match expected {
                Scalar::Int(n) => n.to_string(),
                Scalar::Str(text) => format!("{text:?}"),
            },
            found: found.map_or_else(|| "nothing".to_string(), show),
        }
    };
    for entry in &packet.entries {
        match entry {
            Entry::Field(field) => {
                if let Some(constant) = &field.value
                    && !holds(record.get(&field.name), constant)
                {
                    return Err(wrong(field, record.get(&field.name), constant));
                }
            }
            Entry::Array(ArrayEntry {
                name,
                element: Element::Struct { name: type_name },
                ..
            }) => {
                let fields = types.get(type_name).map_or(&[][..], |t| &t.entries);
                let items = match record.get(name) {
                    Some(Value::Array(items)) => items.as_slice(),
                    _ => &[],
                };
                for item in items {
                    let Value::Record(values) = item else {
                        continue;
                    };
                    for entry in fields {
                        if let Entry::Field(field) = entry
                            && let Some(constant) = &field.value
                            && !holds(values.get(&field.name), constant)
                        {
                            return Err(wrong(field, values.get(&field.name), constant));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// A decoded value is a constant's.
fn holds(found: Option<&Value<'_>>, constant: &Scalar) -> bool {
    match (found, constant) {
        (Some(Value::Str(text)), Scalar::Str(expected)) => text == expected,
        (Some(Value::Uint(n)), _) => integer_value(constant) == Some(i128::from(*n)),
        (Some(Value::Int(n)), _) => integer_value(constant) == Some(i128::from(*n)),
        (Some(Value::Bool(b)), _) => integer_value(constant) == Some(i128::from(*b)),
        _ => false,
    }
}

/// An argument from its field's raw value: through the field's conversion,
/// or as it is. A converted f64 argument is in range when its raw value is
/// one encoding writes for some value in range (between what the range's
/// ends encode to), and it's the converted raw value held to the range.
fn argument_value(
    field: &Field,
    argument: &Argument,
    raw: &Value<'_>,
    binary: bool,
) -> Result<ArgumentValue, ArgumentProblem> {
    let wrong = || ArgumentProblem::WrongType {
        given: show(raw),
        expected: "a raw value this field's conversion takes",
    };
    let value = match (&field.conversion, raw) {
        (Some(Conversion::States { states }), Value::Uint(_) | Value::Int(_)) => {
            let n = integer(raw).ok_or_else(wrong)?;
            let state = states
                .iter()
                .find(|state| integer_value(&state.raw) == Some(n))
                .ok_or_else(|| ArgumentProblem::NoState { raw: n.to_string() })?;
            ArgumentValue::Text(state.name.clone())
        }
        (None, _) => unconverted(argument.ty, raw)?,
        (Some(conversion), _) => {
            let x = number(raw).ok_or_else(wrong)?;
            match convert(conversion, x) {
                Converted::Number(value) => ArgumentValue::Float(value),
                Converted::Outside => {
                    return Err(ArgumentProblem::OutsideCalibration { raw: show(raw) });
                }
                Converted::NotNumeric => return Err(wrong()),
            }
        }
    };
    if argument.ty != ParamType::F64 {
        allowed(argument, &value)?;
        return Ok(value);
    }
    // From the field's unit into the argument's, which validation found
    // compatible.
    let value = match value {
        ArgumentValue::Float(x) => ArgumentValue::Float(
            convert_between(x, field.unit.as_deref(), argument.unit.as_deref())
                .ok_or_else(wrong)?,
        ),
        other => other,
    };
    let (ArgumentValue::Float(x), Some(range)) = (&value, argument.range) else {
        // Without a range, any finite value is allowed.
        allowed(argument, &value)?;
        return Ok(value);
    };
    if number(raw).is_none_or(|n| !n.is_finite()) {
        return Err(ArgumentProblem::NonFinite { given: show(raw) });
    }
    in_raw_range(field, range, argument.unit.as_deref(), raw, binary)?;
    let held = range.min.map_or(*x, |min| x.max(min));
    let held = range.max.map_or(held, |max| held.min(max));
    let value = ArgumentValue::Float(held);
    allowed(argument, &value)?;
    Ok(value)
}

/// A raw value lies between what the range's ends encode to (an open end
/// as ±f64::MAX, which encodes to the field's far side), ends included, in
/// either order. An integer compares with the ends exactly.
fn in_raw_range(
    field: &Field,
    range: ArgumentRange,
    unit: Option<&str>,
    raw: &Value<'_>,
    binary: bool,
) -> Result<(), ArgumentProblem> {
    let end =
        |bound: Option<f64>, open: f64| raw_unchecked(field, bound.unwrap_or(open), unit, binary);
    let (Some(a), Some(b)) = (end(range.min, f64::MIN), end(range.max, f64::MAX)) else {
        return Err(ArgumentProblem::WrongType {
            given: show(raw),
            expected: "a raw value this field's conversion takes",
        });
    };
    let (low, high) = if a <= b { (a, b) } else { (b, a) };
    let inside = match raw {
        Value::Uint(_) | Value::Int(_) => integer(raw).is_some_and(|n| {
            compare(n, low) != Ordering::Less && compare(n, high) != Ordering::Greater
        }),
        Value::Float(x) => low <= *x && *x <= high,
        _ => false,
    };
    if inside {
        return Ok(());
    }
    let integral = matches!(raw, Value::Uint(_) | Value::Int(_));
    let end_text = |x: f64| {
        if integral && x.is_finite() && x.abs() < 1.7e38 {
            (x as i128).to_string()
        } else {
            x.to_string()
        }
    };
    Err(ArgumentProblem::RawOutOfRange {
        raw: show(raw),
        low: end_text(low),
        high: end_text(high),
    })
}

/// A raw value as an argument of this type, with no conversion: an
/// integer as a number for an f64 argument, 0 or 1 as a bool.
fn unconverted(ty: ParamType, raw: &Value<'_>) -> Result<ArgumentValue, ArgumentProblem> {
    let wrong = || ArgumentProblem::WrongType {
        given: show(raw),
        expected: "a raw value this argument's type takes",
    };
    Ok(match (ty, raw) {
        (ParamType::F64, _) => ArgumentValue::Float(number(raw).ok_or_else(wrong)?),
        (ParamType::Bool, Value::Bool(b)) => ArgumentValue::Bool(*b),
        (ParamType::Bool, Value::Uint(_) | Value::Int(_)) => match integer(raw) {
            Some(0) => ArgumentValue::Bool(false),
            Some(1) => ArgumentValue::Bool(true),
            _ => {
                return Err(ArgumentProblem::OutOfRange {
                    given: show(raw),
                    range: "0 or 1 for a bool".to_string(),
                });
            }
        },
        (_, Value::Uint(n)) => ArgumentValue::Uint(*n),
        (_, Value::Int(n)) => ArgumentValue::Int(*n),
        (_, Value::Str(text)) => ArgumentValue::Text(text.clone()),
        (_, Value::Bytes(bytes)) => ArgumentValue::Bytes(bytes.clone()),
        _ => return Err(wrong()),
    })
}

/// The target's native id, from the first target field.
fn target(packet: &CommandPacket, record: &Record<'_>) -> Option<NativeId> {
    let field = packet.entries.iter().find_map(|entry| match entry {
        Entry::Field(field) if field.target == Some(true) => Some(field),
        _ => None,
    })?;
    match record.get(&field.name)? {
        Value::Uint(n) => Some(NativeId::from(*n)),
        Value::Int(n) => Some(NativeId::from(*n)),
        Value::Str(text) => Some(NativeId::from_text(text.as_str())),
        _ => None,
    }
}

fn integer(raw: &Value<'_>) -> Option<i128> {
    match raw {
        Value::Uint(n) => Some(i128::from(*n)),
        Value::Int(n) => Some(i128::from(*n)),
        _ => None,
    }
}

/// A raw number as a double; integers become the nearest double.
fn number(raw: &Value<'_>) -> Option<f64> {
    match raw {
        Value::Uint(n) => Some(*n as f64),
        Value::Int(n) => Some(*n as f64),
        Value::Float(x) => Some(*x),
        _ => None,
    }
}

fn show(raw: &Value<'_>) -> String {
    match raw {
        Value::Uint(n) => n.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(x) => x.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Str(text) => format!("{text:?}"),
        Value::Bytes(bytes) => format!("{} bytes", bytes.len()),
        _ => "a composite value".to_string(),
    }
}
