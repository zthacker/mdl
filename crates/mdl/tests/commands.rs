//! Random command layouts (`mdl_testkit::layout::command`): each request
//! encodes to exactly the payload, raw values and defaulted arguments the
//! testkit worked out from `spec/encoding.md` on its own; the payload
//! decodes back to the command, every argument in catalog order and the
//! target, as §7.4 says; and the decoded request encodes to the payload the
//! testkit expects of it, the same one wherever each inverse undoes its
//! conversion. A request with faults planted in it is refused with the
//! code, and naming the argument or field, of the first problem the spec
//! reaches; a payload with a fault planted in it fails to decode with the
//! code the fault gives.

#[path = "common/artifacts.rs"]
mod artifacts;

use mdl::bindings::NativeId;
use mdl::decode::{CommandDecodeError, DecodeError};
use mdl::encode::{ArgumentValue, CommandRequest, EncodeError};
use mdl::message::{Message, MetaValue, Payload};
use mdl::time::Timestamp;
use mdl_testkit::layout::command::{Arg, Encoding, Given, Target, commands};
use proptest::prelude::*;

proptest! {
    #[test]
    fn a_command_encodes_and_decodes_as_the_spec_says(case in commands()) {
        let sets = artifacts::load(&case.bundle, &case.family, &[])?;
        for request in &case.requests {
            let encoded = sets
                .encode(&sent(&case.set, &request.command, &request.arguments, request.target.as_ref())?)
                .map_err(|error| TestCaseError::fail(format!("{error}: {request:#?}")))?;
            prop_assert_eq!(&encoded.payload, &request.payload, "{:#?}", request);
            prop_assert_eq!(&artifacts::record(&encoded.record), &request.record);
            prop_assert_eq!(&names(&encoded.defaulted), &request.defaulted);
            let metadata = encoded.metadata();
            prop_assert_eq!(&meta(&metadata), &request.metadata);

            let message = Message::new(Payload::Binary(&encoded.payload), Timestamp::from_unix_nanos(0))
                .with_metadata(&metadata);
            let decoded = sets
                .decode_command(&case.set, &message)
                .map_err(|error| TestCaseError::fail(format!("{error}: {request:#?}")))?;
            prop_assert_eq!(decoded.command(), request.command.as_str());
            let arguments: Vec<(String, Arg)> = decoded
                .arguments
                .iter()
                .map(|(name, value)| (name.to_string(), arg(value)))
                .collect();
            prop_assert_eq!(&arguments, &request.decoded.arguments, "{:#?}", request);
            prop_assert_eq!(&names(&decoded.defaulted), &request.decoded.defaulted);
            prop_assert_eq!(&decoded.target.as_ref().map(target), &request.decoded.target);
            prop_assert_eq!(&artifacts::record(&decoded.record), &request.record);

            let again = sets
                .encode(&decoded.request())
                .map_err(|error| TestCaseError::fail(format!("again: {error}: {request:#?}")))?;
            prop_assert_eq!(&again.payload, &request.again, "{:#?}", request);
        }
        for refused in &case.refused {
            let request = sent(&refused.set, &refused.command, &refused.arguments, refused.target.as_ref())?;
            let error = match sets.encode(&request) {
                Ok(encoded) => {
                    return Err(TestCaseError::fail(format!("encoded {:?}: {refused:#?}", encoded.payload)));
                }
                Err(error) => error,
            };
            prop_assert_eq!(error.code(), refused.code, "{}: {:#?}", error, refused);
            prop_assert_eq!(named(&error), refused.at.clone(), "{}: {:#?}", error, refused);
        }
        for garbled in &case.garbled {
            let metadata: Vec<(&str, MetaValue<'_>)> = garbled
                .metadata
                .iter()
                .map(|(name, n)| (name.as_str(), MetaValue::Int(*n)))
                .collect();
            let payload = match garbled.encoding {
                Encoding::Json => Payload::Json(&garbled.payload),
                Encoding::Binary => Payload::Binary(&garbled.payload),
            };
            let message = Message::new(payload, Timestamp::from_unix_nanos(0)).with_metadata(&metadata);
            let error = match sets.decode_command(&garbled.set, &message) {
                Ok(decoded) => {
                    return Err(TestCaseError::fail(format!("decoded {:?}: {garbled:#?}", decoded.arguments)));
                }
                Err(error) => error,
            };
            prop_assert_eq!(error.code(), garbled.code, "{}: {:#?}", error, garbled);
            prop_assert_eq!(decode_named(&error), garbled.at.clone(), "{}: {:#?}", error, garbled);
        }
    }
}

/// The request as a caller makes it.
fn sent(
    set: &str,
    command: &str,
    arguments: &[(String, Given)],
    target: Option<&Target>,
) -> Result<CommandRequest, TestCaseError> {
    let mut sent = CommandRequest::new(set, command);
    for (name, value) in arguments {
        let value = match value {
            Given::Bool(b) => ArgumentValue::Bool(*b),
            Given::Int(n) => ArgumentValue::Int(*n),
            Given::Uint(n) => ArgumentValue::Uint(*n),
            Given::Float(x) => ArgumentValue::Float(*x),
            Given::Text(text) => ArgumentValue::Text(text.clone()),
            Given::Bytes(bytes) => ArgumentValue::Bytes(bytes.clone()),
        };
        sent = sent.with_argument(name.as_str(), value);
    }
    if let Some(target) = target {
        sent = sent.with_target(match target {
            Target::Int(n) => match (i64::try_from(*n), u64::try_from(*n)) {
                (Ok(n), _) => NativeId::from(n),
                (_, Ok(n)) => NativeId::from(n),
                _ => return Err(TestCaseError::fail(format!("no native id is {n}"))),
            },
            Target::Text(text) => NativeId::from_text(text.clone()),
        });
    }
    Ok(sent)
}

/// The argument, constant or entry a decoding error names.
fn decode_named(error: &CommandDecodeError) -> Option<String> {
    match error {
        CommandDecodeError::Constant { field, .. } => Some(field.clone()),
        CommandDecodeError::Argument { argument, .. } => Some(argument.clone()),
        CommandDecodeError::Decode(DecodeError::Entry { path, .. }) => Some(path.clone()),
        _ => None,
    }
}

/// The argument or field an encoding error names.
fn named(error: &EncodeError) -> Option<String> {
    match error {
        EncodeError::Argument { argument, .. } => Some(argument.clone()),
        EncodeError::Field { field, .. } => Some(field.clone()),
        _ => None,
    }
}

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

fn meta(metadata: &[(&str, MetaValue<'_>)]) -> Vec<(String, i64)> {
    metadata
        .iter()
        .map(|(name, value)| {
            let value = match value {
                MetaValue::Int(n) => *n,
                other => panic!("not generated: {other:?}"),
            };
            (name.to_string(), value)
        })
        .collect()
}

fn arg(value: &ArgumentValue) -> Arg {
    match value {
        ArgumentValue::Bool(b) => Arg::Bool(*b),
        ArgumentValue::Int(n) => Arg::I64(*n),
        ArgumentValue::Uint(n) => Arg::U64(*n),
        ArgumentValue::Float(x) => Arg::F64(*x),
        ArgumentValue::Text(text) => Arg::Text(text.clone()),
        ArgumentValue::Bytes(bytes) => Arg::Bytes(bytes.clone()),
        other => Arg::Text(format!("not generated: {other:?}")),
    }
}

fn target(id: &NativeId) -> Target {
    match (id.as_int(), id.as_text()) {
        (Some(n), _) => Target::Int(n),
        (None, Some(text)) => Target::Text(text.to_string()),
        (None, None) => Target::Text(String::new()),
    }
}
