//! Targets for messages against the kitchen sink: telemetry decoded,
//! command payloads decoded, and command requests encoded. Each documents
//! the layout it reads its input in (see `input.rs`).

use std::collections::BTreeSet;
use std::fmt::{Debug, Display};

use mdl::bindings::NativeId;
use mdl::decode::{Decoded, DecodedCommand};
use mdl::encode::{ArgumentValue, CommandRequest};
use mdl::message::{Message, MetaValue, Payload};
use mdl::route::FamilySets;
use mdl::time::Timestamp;

use crate::input::Input;
use crate::kitchen::{Argument, kitchen_sink};

/// 2026-01-01T00:00:00Z in Unix seconds: ground times count from it.
const EPOCH: i64 = 1_767_225_600;

/// Telemetry, decoded by one of the kitchen sink's families, resolved by
/// its bindings. Layout:
/// - the family: a byte, its index modulo the five; from 128, the payload
///   is delivered in the other encoding;
/// - the ground time: four bytes, signed seconds from 2026-01-01T00:00:00Z,
///   little-endian, so binding timelines are crossed;
/// - metadata, as `metadata` reads it;
/// - the payload, the rest.
///
/// Decoding is stateless, so decoding a message twice gives the same; what
/// decodes passes `check_decoded`.
pub fn decode(data: &[u8]) {
    let sink = kitchen_sink();
    let mut input = Input::new(data);
    let choice = input.byte();
    let family = &sink.families[usize::from(choice & 0x7f) % sink.families.len()];
    let json = family.json != (choice >= 0x80);
    let Some(ground) = Timestamp::new(EPOCH + i64::from(input.i32()), 0) else {
        return;
    };
    let pairs = metadata(&mut input, &family.metadata);
    let metadata = borrowed(&pairs);
    let message = Message::new(payload(json, input.rest()), ground).with_metadata(&metadata);
    let first = family.sets.decode(&message, &sink.bindings);
    let second = family.sets.decode(&message, &sink.bindings);
    // Debug, so a NaN decoded twice is the same.
    assert_eq!(
        format!("{first:?}"),
        format!("{second:?}"),
        "the same message decoded twice"
    );
    match first {
        Ok(decoded) => check_decoded(&decoded, &sink.params),
        Err(error) => check_error(error.code(), &error),
    }
}

/// A command payload, decoded by one of the kitchen sink's command sets.
/// Layout:
/// - the set: a byte, its index modulo the four; from 128, the payload is
///   delivered in the other encoding;
/// - metadata, as `metadata` reads it;
/// - the payload, the rest.
///
/// What decodes passes `check_command_payload`.
pub fn decode_command(data: &[u8]) {
    let sink = kitchen_sink();
    let mut input = Input::new(data);
    let choice = input.byte();
    let set = &sink.command_sets[usize::from(choice & 0x7f) % sink.command_sets.len()];
    let family = &sink.families[set.family];
    let json = family.json != (choice >= 0x80);
    let pairs = metadata(&mut input, &family.metadata);
    let metadata = borrowed(&pairs);
    let message = Message::new(payload(json, input.rest()), epoch()).with_metadata(&metadata);
    check_command_payload(&family.sets, &set.name, family.json, &message);
}

/// A command request for one of the kitchen sink's command sets. Layout:
/// - the set: a byte, its index modulo the four;
/// - the command: a byte, modulo the set's commands and one more, a name
///   the catalog hasn't got;
/// - how many arguments: a byte, modulo 8; then each one's name, a byte
///   modulo the command's arguments and one more, a name it hasn't got,
///   and its value, as `value` reads it;
/// - the target: a byte, modulo 4, for none, an integer of eight bytes,
///   the decimal digits of one, or any text, as `text` reads it.
///
/// A request that encodes gives a payload that decodes, with its
/// metadata, to the same command with every argument the catalog gives it
/// (`spec/encoding.md` §7.4), a bool or an enum's state as it was given
/// (no conversion rounds them, §3.1), and the request that gives encodes
/// again.
pub fn encode(data: &[u8]) {
    let sink = kitchen_sink();
    let mut input = Input::new(data);
    let set = &sink.command_sets[input.choose(sink.command_sets.len())];
    let family = &sink.families[set.family];
    let command = set.commands.get(input.choose(set.commands.len() + 1));
    let arguments: &[Argument] = command.map_or(&[], |command| &command.arguments);
    let name = command.map_or("undeclared.command", |command| command.name.as_str());
    let mut request = CommandRequest::new(set.name.as_str(), name);
    for _ in 0..input.choose(8) {
        let argument = arguments.get(input.choose(arguments.len() + 1));
        let states = argument.map_or(&[][..], |argument| argument.states.as_slice());
        let name = argument.map_or("undeclared", |argument| argument.name.as_str());
        request
            .arguments
            .push((name.to_string(), value(&mut input, states, true)));
    }
    request.target = match input.choose(4) {
        0 => None,
        1 => Some(NativeId::from(input.u64())),
        2 => Some(NativeId::from_text(input.u64().cast_signed().to_string())),
        _ => Some(NativeId::from_text(text(&mut input))),
    };
    let encoded = match family.sets.encode(&request) {
        Ok(encoded) => encoded,
        Err(error) => {
            check_error(error.code(), &error);
            return;
        }
    };
    let metadata = encoded.metadata();
    let message =
        Message::new(payload(family.json, &encoded.payload), epoch()).with_metadata(&metadata);
    let decoded = match family.sets.decode_command(&set.name, &message) {
        Ok(decoded) => decoded,
        Err(error) => panic!("{request:?} encoded to a payload that didn't decode: {error}"),
    };
    assert_eq!(decoded.command(), request.command, "the command encoded");
    let catalog: Vec<&str> = arguments
        .iter()
        .map(|argument| argument.name.as_str())
        .collect();
    assert_eq!(
        names(&decoded),
        catalog,
        "{request:?}: the arguments decoded"
    );
    for (name, given) in &request.arguments {
        let state = matches!(given, ArgumentValue::Text(_))
            && arguments
                .iter()
                .any(|argument| argument.name == *name && !argument.states.is_empty());
        if matches!(given, ArgumentValue::Bool(_)) || state {
            let back = decoded
                .arguments
                .iter()
                .find(|(decoded, _)| decoded == name)
                .map(|(_, value)| value);
            assert_eq!(back, Some(given), "{request:?}: argument {name}");
        }
    }
    if let Err(error) = family.sets.encode(&decoded.request()) {
        panic!("{request:?} decoded to a request that didn't encode: {error}");
    }
}

/// A decoding or encoding error, for a definition that passed validation:
/// it has a code and a message, and isn't `invalid_layout`, which MDL
/// gives only for a definition validation refuses.
pub(crate) fn check_error(code: &str, error: &(impl Display + Debug)) {
    assert!(
        !code.is_empty() && !error.to_string().is_empty(),
        "{error:?}"
    );
    assert_ne!(
        code, "invalid_layout",
        "a valid definition refused as invalid: {error}"
    );
}

/// A decoded message: each sample is a catalog param's, and each
/// engineering value listed is found again at its path.
pub(crate) fn check_decoded(decoded: &Decoded<'_>, params: &BTreeSet<String>) {
    for sample in &decoded.samples {
        assert!(
            params.contains(sample.param),
            "a sample of {}, which isn't a catalog param",
            sample.param
        );
    }
    for (path, value) in decoded.engineering_values() {
        let found = decoded.engineering(&path);
        // Debug, so a NaN found is the NaN listed.
        assert_eq!(
            format!("{found:?}"),
            format!("{:?}", Some(value)),
            "the engineering value at {path}"
        );
    }
}

/// A command payload, decoded by the named set. One that decodes gives a
/// request that encodes, since what decoding gives the catalog allows
/// (`spec/encoding.md` §7.4), and that payload decodes to the same
/// command, every argument but a float the same: only a conversion and its
/// inverse round, and they round floats.
pub(crate) fn check_command_payload(
    sets: &FamilySets,
    set: &str,
    json: bool,
    message: &Message<'_>,
) {
    let decoded = match sets.decode_command(set, message) {
        Ok(decoded) => decoded,
        Err(error) => {
            check_error(error.code(), &error);
            return;
        }
    };
    let request = decoded.request();
    let encoded = match sets.encode(&request) {
        Ok(encoded) => encoded,
        Err(error) => panic!("{request:?}, decoded from a payload, didn't encode: {error}"),
    };
    let metadata = encoded.metadata();
    let again = Message::new(payload(json, &encoded.payload), epoch()).with_metadata(&metadata);
    match sets.decode_command(set, &again) {
        Ok(back) => {
            assert_eq!(
                back.command(),
                decoded.command(),
                "{request:?}: the command"
            );
            for ((name, value), (_, again)) in decoded.arguments.iter().zip(&back.arguments) {
                if exact(value) {
                    assert_eq!(again, value, "{request:?}: argument {name}");
                }
            }
        }
        Err(error) => panic!("{request:?} encoded to a payload that didn't decode: {error}"),
    }
}

/// Whether a value decoded comes back as it is when encoded and decoded
/// again: anything but a float, or a list holding one.
fn exact(value: &ArgumentValue) -> bool {
    match value {
        ArgumentValue::Float(_) => false,
        ArgumentValue::List(items) => items.iter().all(exact),
        _ => true,
    }
}

/// A metadata value as an input gives it.
pub(crate) enum Meta {
    Uint(u64),
    Int(i64),
    Text(String),
}

/// Metadata pairs. Layout: how many, a byte modulo 4; then each pair's
/// name, a byte modulo `names` and one more, a name nobody declares; and
/// its value: a kind, a byte modulo 4, then eight bytes, little-endian, as
/// a uint, an int or the decimal digits of an int, or any text, as `text`
/// reads it.
fn metadata(input: &mut Input<'_>, names: &[String]) -> Vec<(String, Meta)> {
    (0..input.choose(4))
        .map(|_| {
            let name = names
                .get(input.choose(names.len() + 1))
                .map_or("undeclared", String::as_str)
                .to_string();
            let meta = match input.choose(4) {
                0 => Meta::Uint(input.u64()),
                1 => Meta::Int(input.u64().cast_signed()),
                2 => Meta::Text(input.u64().cast_signed().to_string()),
                _ => Meta::Text(text(input)),
            };
            (name, meta)
        })
        .collect()
}

/// Any text: a length byte, modulo 32, then that many bytes as UTF-8,
/// what isn't being replaced.
fn text(input: &mut Input<'_>) -> String {
    let length = input.choose(32);
    String::from_utf8_lossy(input.take(length)).into_owned()
}

/// Metadata as a message carries it.
pub(crate) fn borrowed(pairs: &[(String, Meta)]) -> Vec<(&str, MetaValue<'_>)> {
    pairs
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

/// An argument's value. Layout: its kind, a byte modulo 8, then:
/// - a bool: a byte, odd for true;
/// - a uint, an int or a float: eight bytes, a float any bits, NaN and the
///   infinities among them;
/// - a small int: a signed byte;
/// - text: a length byte; below 128, that many bytes as UTF-8, what isn't
///   being replaced; from 128, one of `states` by index, if there are any;
/// - bytes: a length byte, modulo 64, then that many;
/// - a list (only where `list`; a small int otherwise): a length byte,
///   modulo 6, then that many values, none of them a list.
fn value(input: &mut Input<'_>, states: &[String], list: bool) -> ArgumentValue {
    match input.choose(8) {
        0 => ArgumentValue::Bool(input.byte() % 2 == 1),
        1 => ArgumentValue::Uint(input.u64()),
        2 => ArgumentValue::Int(input.u64().cast_signed()),
        3 => ArgumentValue::Float(f64::from_bits(input.u64())),
        5 => {
            let length = input.byte();
            match states.get(usize::from(length & 0x7f) % states.len().max(1)) {
                Some(state) if length >= 0x80 => ArgumentValue::Text(state.clone()),
                _ => {
                    let bytes = input.take(usize::from(length & 0x7f));
                    ArgumentValue::Text(String::from_utf8_lossy(bytes).into_owned())
                }
            }
        }
        6 => {
            let length = input.choose(64);
            ArgumentValue::Bytes(input.take(length).to_vec())
        }
        7 if list => {
            let length = input.choose(6);
            ArgumentValue::List((0..length).map(|_| value(input, states, false)).collect())
        }
        _ => ArgumentValue::Int(i64::from(input.byte().cast_signed())),
    }
}

/// The names of a decoded command's arguments, in the order decoded.
fn names<'a>(decoded: &'a DecodedCommand<'_>) -> Vec<&'a str> {
    decoded.arguments.iter().map(|(name, _)| *name).collect()
}

fn payload(json: bool, bytes: &[u8]) -> Payload<'_> {
    if json {
        Payload::Json(bytes)
    } else {
        Payload::Binary(bytes)
    }
}

/// The ground time of a command payload, which nothing resolves at.
fn epoch() -> Timestamp {
    Timestamp::from_unix_nanos(EPOCH * 1_000_000_000)
}
