//! Elixir terms read as the runtime's inputs (`spec/api.md` §2). A term of
//! the wrong shape raises `ArgumentError`, naming what was expected.

use mdl::bindings::NativeId;
use mdl::encode::{ArgumentValue, CommandRequest};
use mdl::message::{Message, MetaValue, Payload};
use mdl::time::Timestamp;
use rustler::types::MapIterator;
use rustler::types::tuple::get_tuple;
use rustler::{Atom, Binary, Error, NifResult, Term, TermType};

use crate::output::{self, atoms};

/// A message as `MDL.decode/3` takes it: a map of `:payload`, a binary;
/// `:metadata`, a map of names to integers or strings, optional; and
/// `:ground_time`, integer nanoseconds since the Unix epoch.
pub(crate) struct MessageInput<'a> {
    payload: Binary<'a>,
    metadata: Vec<(String, Meta)>,
    ground_time: Timestamp,
}

/// A metadata value, owned until the message borrows it.
enum Meta {
    Uint(u64),
    Int(i64),
    Text(String),
}

impl<'a> MessageInput<'a> {
    /// The metadata as a message borrows it.
    pub(crate) fn metadata(&self) -> Vec<(&str, MetaValue<'_>)> {
        self.metadata
            .iter()
            .map(|(name, meta)| {
                let value = match meta {
                    Meta::Uint(n) => MetaValue::Uint(*n),
                    Meta::Int(n) => MetaValue::Int(*n),
                    Meta::Text(text) => MetaValue::Text(text),
                };
                (name.as_str(), value)
            })
            .collect()
    }

    /// The payload's bytes, which the runtime reads in the family's
    /// encoding (`Runtime::payload`).
    pub(crate) fn bytes(&self) -> &'a [u8] {
        self.payload.as_slice()
    }

    /// The message, with `payload` (these bytes, typed by the family).
    pub(crate) fn message<'m>(
        &self,
        payload: Payload<'m>,
        metadata: &'m [(&'m str, MetaValue<'m>)],
    ) -> Message<'m> {
        Message::new(payload, self.ground_time).with_metadata(metadata)
    }
}

/// A message map.
pub(crate) fn message(term: Term<'_>) -> NifResult<MessageInput<'_>> {
    const SHAPE: &str = "a message is a map of :payload (a binary), :ground_time (integer \
                         nanoseconds since the Unix epoch) and, optionally, :metadata (a map \
                         of names to integers or strings)";
    let mut payload = None;
    let mut metadata = Vec::new();
    let mut ground_time = None;
    for (key, value) in fields(term, SHAPE)? {
        if key == atoms::payload() {
            if value.get_type() != TermType::Binary {
                return Err(bad("a message's :payload is a binary"));
            }
            payload = Some(value.decode::<Binary>()?);
        } else if key == atoms::metadata() {
            metadata = metadata_map(value)?;
        } else if key == atoms::ground_time() {
            ground_time = Some(instant(value, "a message's :ground_time")?);
        } else {
            return Err(bad(SHAPE));
        }
    }
    match (payload, ground_time) {
        (Some(payload), Some(ground_time)) => Ok(MessageInput {
            payload,
            metadata,
            ground_time,
        }),
        _ => Err(bad(SHAPE)),
    }
}

/// Metadata: names, strings as the link names them, to integers or text.
fn metadata_map(term: Term<'_>) -> NifResult<Vec<(String, Meta)>> {
    const SHAPE: &str = "a message's :metadata is a map of names (strings, such as \"apid\") \
                         to integers or strings";
    let Some(entries) = MapIterator::new(term) else {
        return Err(bad(SHAPE));
    };
    let mut metadata = Vec::new();
    for (key, value) in entries {
        let name = text(key).ok_or_else(|| bad(SHAPE))?;
        let meta = match value.get_type() {
            TermType::Integer => {
                if let Ok(n) = value.decode::<u64>() {
                    Meta::Uint(n)
                } else if let Ok(n) = value.decode::<i64>() {
                    Meta::Int(n)
                } else {
                    return Err(bad(format!(
                        "metadata {name}: an integer metadata value is from -2^63 to 2^64 - 1"
                    )));
                }
            }
            TermType::Binary => {
                Meta::Text(text(value).ok_or_else(|| bad(format!("metadata {name} isn't UTF-8")))?)
            }
            _ => return Err(bad(SHAPE)),
        };
        metadata.push((name, meta));
    }
    Ok(metadata)
}

/// A command request map.
pub(crate) fn request(term: Term<'_>) -> NifResult<CommandRequest> {
    const SHAPE: &str = "a command request is a map of :set and :command (strings) and, \
                         optionally, :arguments (a map of argument names, strings, to values) \
                         and :native_id (an integer or a string)";
    let mut set = None;
    let mut command = None;
    let mut arguments = Vec::new();
    let mut target = None;
    for (key, value) in fields(term, SHAPE)? {
        if key == atoms::set() {
            set = Some(text(value).ok_or_else(|| bad("a command request's :set is a string"))?);
        } else if key == atoms::command() {
            command =
                Some(text(value).ok_or_else(|| bad("a command request's :command is a string"))?);
        } else if key == atoms::arguments() {
            arguments = arguments_map(value)?;
        } else if key == atoms::native_id() {
            if !is_nil(value) {
                target = Some(native_id(value)?);
            }
        } else {
            return Err(bad(SHAPE));
        }
    }
    let (Some(set), Some(command)) = (set, command) else {
        return Err(bad(SHAPE));
    };
    let mut request = CommandRequest::new(set, command);
    request.arguments = arguments;
    request.target = target;
    Ok(request)
}

/// Arguments by name.
fn arguments_map(term: Term<'_>) -> NifResult<Vec<(String, ArgumentValue)>> {
    const SHAPE: &str =
        "a command request's :arguments is a map of argument names (strings) to values";
    let Some(entries) = MapIterator::new(term) else {
        return Err(bad(SHAPE));
    };
    let mut arguments = Vec::new();
    for (key, value) in entries {
        let name = text(key).ok_or_else(|| bad(SHAPE))?;
        let value = argument(value, &name)?;
        arguments.push((name, value));
    }
    Ok(arguments)
}

/// An argument's value (`spec/api.md` §2.4): a scalar, or a list of
/// scalars. A list's values are read without recursing, so no input, however
/// deeply nested, can overflow the scheduler's stack.
fn argument(term: Term<'_>, name: &str) -> NifResult<ArgumentValue> {
    if term.get_type() != TermType::List {
        return scalar(term, name);
    }
    term.decode::<Vec<Term>>()?
        .into_iter()
        .map(|element| {
            if element.get_type() == TermType::List {
                Err(bad(format!(
                    "argument {name}: a list argument's values are scalars, not lists"
                )))
            } else {
                scalar(element, name)
            }
        })
        .collect::<NifResult<Vec<ArgumentValue>>>()
        .map(ArgumentValue::List)
}

/// A scalar argument value: an integer, a float or one of `:nan`,
/// `:infinity` and `:neg_infinity`, a boolean, text (a string), or bytes
/// (`{:bytes, binary}`).
fn scalar(term: Term<'_>, name: &str) -> NifResult<ArgumentValue> {
    match term.get_type() {
        TermType::Integer => {
            if let Ok(n) = term.decode::<u64>() {
                Ok(ArgumentValue::Uint(n))
            } else if let Ok(n) = term.decode::<i64>() {
                Ok(ArgumentValue::Int(n))
            } else {
                Err(bad(format!(
                    "argument {name}: an integer argument is from -2^63 to 2^64 - 1"
                )))
            }
        }
        TermType::Float => Ok(ArgumentValue::Float(term.decode::<f64>()?)),
        TermType::Atom => {
            let atom = term.decode::<Atom>()?;
            if atom == atoms::true_() {
                Ok(ArgumentValue::Bool(true))
            } else if atom == atoms::false_() {
                Ok(ArgumentValue::Bool(false))
            } else if atom == atoms::nan() {
                Ok(ArgumentValue::Float(f64::NAN))
            } else if atom == atoms::infinity() {
                Ok(ArgumentValue::Float(f64::INFINITY))
            } else if atom == atoms::neg_infinity() {
                Ok(ArgumentValue::Float(f64::NEG_INFINITY))
            } else {
                Err(bad(format!(
                    "argument {name}: the only atoms an argument takes are true, false, :nan, \
                     :infinity and :neg_infinity; text and state names are strings"
                )))
            }
        }
        TermType::Binary => text(term).map(ArgumentValue::Text).ok_or_else(|| {
            bad(format!(
                "argument {name}: a string argument is UTF-8 text; bytes are {{:bytes, binary}}"
            ))
        }),
        TermType::Tuple => {
            let elements = get_tuple(term)?;
            match elements.as_slice() {
                [tag, bytes]
                    if tag.decode::<Atom>().ok() == Some(atoms::bytes())
                        && bytes.get_type() == TermType::Binary =>
                {
                    Ok(ArgumentValue::Bytes(
                        bytes.decode::<Binary>()?.as_slice().to_vec(),
                    ))
                }
                _ => Err(bad(format!(
                    "argument {name}: the only tuple an argument takes is {{:bytes, binary}}"
                ))),
            }
        }
        _ => Err(bad(format!(
            "argument {name}: a value is an integer, a float, a boolean, a string, \
             {{:bytes, binary}} or a list of them"
        ))),
    }
}

/// A native id: an integer from -2^63 to 2^64 - 1, or a string.
pub(crate) fn native_id(term: Term<'_>) -> NifResult<NativeId> {
    const SHAPE: &str = "a native id is an integer from -2^63 to 2^64 - 1, or a string";
    match term.get_type() {
        TermType::Integer => {
            if let Ok(n) = term.decode::<u64>() {
                Ok(NativeId::from(n))
            } else if let Ok(n) = term.decode::<i64>() {
                Ok(NativeId::from(n))
            } else {
                Err(bad(SHAPE))
            }
        }
        TermType::Binary => text(term)
            .map(NativeId::from_text)
            .ok_or_else(|| bad(SHAPE)),
        _ => Err(bad(SHAPE)),
    }
}

/// An instant: integer nanoseconds since 1970-01-01T00:00:00Z, within 64
/// bits (years 1677 to 2262), as clocks and links carry ground time.
pub(crate) fn instant(term: Term<'_>, what: &str) -> NifResult<Timestamp> {
    if term.get_type() != TermType::Integer {
        return Err(bad(format!(
            "{what} is an integer: nanoseconds since 1970-01-01T00:00:00Z"
        )));
    }
    term.decode::<i64>()
        .map(Timestamp::from_unix_nanos)
        .map_err(|_| {
            bad(format!(
                "{what} is nanoseconds since 1970-01-01T00:00:00Z within 64 bits \
                 (years 1677 to 2262)"
            ))
        })
}

/// A map's entries, each key an atom.
fn fields<'a>(term: Term<'a>, shape: &str) -> NifResult<Vec<(Atom, Term<'a>)>> {
    let Some(entries) = MapIterator::new(term) else {
        return Err(bad(shape));
    };
    entries
        .map(|(key, value)| {
            key.decode::<Atom>()
                .map(|key| (key, value))
                .map_err(|_| bad(shape))
        })
        .collect()
}

/// A binary's text, if it's UTF-8.
fn text(term: Term<'_>) -> Option<String> {
    (term.get_type() == TermType::Binary)
        .then(|| term.decode::<String>().ok())
        .flatten()
}

fn is_nil(term: Term<'_>) -> bool {
    term.decode::<Atom>().ok() == Some(atoms::nil())
}

/// `ArgumentError` with `message`, raised from the NIF.
fn bad(message: impl Into<String>) -> Error {
    output::raise(atoms::argument_error(), message)
}
