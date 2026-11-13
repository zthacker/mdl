//! Random valid layouts (`mdl_testkit::layout`), binary and JSON: each
//! validates, builds artifacts that read back with their digests, and,
//! loaded from those, decodes every message packed for it to exactly the
//! raw values, samples and event time the testkit worked out from the spec
//! on its own. A damaged message fails with exactly the error the testkit
//! worked out.

mod common;

use mdl::bindings::Bindings;
use mdl::decode::{DecodeError, JsonType, Problem};
use mdl::ir::{Encoding, ScalarType, TextEncoding};
use mdl::message::{Message, Payload};
use mdl::route::{LocatorRole, RouteError};
use mdl::time::Timestamp;
use mdl_testkit::layout::{self, JsonKind, telemetry};
use proptest::prelude::*;

proptest! {
    #[test]
    fn a_valid_layout_decodes_what_was_packed_for_it(case in telemetry()) {
        valid(&case, false)?;
    }

    /// Faults planted in valid messages (`mdl_testkit::layout::damaged`):
    /// each damaged message fails with the error the testkit worked out,
    /// at the entry it names, with what that error reports.
    #[test]
    fn a_damaged_message_fails_where_the_spec_says(case in layout::damaged()) {
        damaged(&case, false)?;
    }

    #[test]
    fn a_valid_json_layout_decodes_what_was_written_for_it(case in layout::json_telemetry()) {
        valid(&case, true)?;
    }

    /// Faults planted in valid JSON messages
    /// (`mdl_testkit::layout::json_damaged`).
    #[test]
    fn a_damaged_json_message_fails_where_the_spec_says(case in layout::json_damaged()) {
        damaged(&case, true)?;
    }
}

/// Every message of a valid case decodes to what the testkit worked out,
/// or, where its event time can't be read, fails with `invalid_time`.
fn valid(case: &layout::Telemetry, json: bool) -> Result<(), TestCaseError> {
    let sets = common::load(&case.bundle, &case.family, &[])?;
    let bindings = bindings(case);
    for message in &case.messages {
        let result = sets.decode(&message_of(message), &bindings);
        // An event time that can't be read is the message's error.
        if message.event_time == layout::EventTime::Invalid {
            prop_assert_eq!(
                result.err().map(|error| error.code()),
                Some("invalid_time"),
                "payload {}",
                shown(&message.payload, json)
            );
            continue;
        }
        let decoded = result.map_err(|error| {
            let payload = shown(&message.payload, json);
            TestCaseError::fail(format!("{error}: {payload}: {message:?}"))
        })?;
        common::check(&decoded, message)?;
    }
    Ok(())
}

/// Every damaged message of a case fails with exactly the error the
/// testkit worked out; the others decode as valid ones do.
fn damaged(case: &layout::Telemetry, json: bool) -> Result<(), TestCaseError> {
    let sets = common::load(&case.bundle, &case.family, &[])?;
    let bindings = bindings(case);
    for message in &case.messages {
        let result = sets.decode(&message_of(message), &bindings);
        match &message.damage {
            Some(damage) => {
                let error = result.err();
                let failure = error.as_ref().and_then(failure);
                prop_assert_eq!(
                    failure.as_ref(),
                    Some(&damage.failure),
                    "{}: decoding gave {:?}, payload {}",
                    damage.planted,
                    error.map(|error| error.to_string()),
                    shown(&message.payload, json)
                );
            }
            None if message.event_time == layout::EventTime::Invalid => {
                prop_assert_eq!(result.err().map(|error| error.code()), Some("invalid_time"));
            }
            None => {
                let decoded =
                    result.map_err(|error| TestCaseError::fail(format!("{error}: {message:?}")))?;
                common::check(&decoded, message)?;
            }
        }
    }
    Ok(())
}

/// A payload as a failing case shows it: JSON as its text.
fn shown(payload: &[u8], json: bool) -> String {
    if json {
        String::from_utf8_lossy(payload).into_owned()
    } else {
        format!("{payload:02x?}")
    }
}

/// The binding of the case's native id to its set: an integer one written
/// as an integer, as bindings files want it.
fn bindings(case: &layout::Telemetry) -> Bindings {
    let native_id = match case.native_id.parse::<i64>() {
        Ok(n) => n.to_string(),
        Err(_) => format!("\"{}\"", case.native_id),
    };
    Bindings::from_toml(&format!(
        "mdl_version = \"2.0\"\n\n[[binding]]\nfamily = \"{}\"\nnative_id = {native_id}\n\
         asset = \"ASSET\"\nset = \"{}\"\neffective_from = \"1980-01-06T00:00:00Z\"\n",
        case.family, case.set
    ))
    .unwrap()
}

/// A message as a link delivers it, in the encoding the testkit says.
fn message_of(message: &layout::Message) -> Message<'_> {
    let received = Timestamp::from_unix_nanos(i64::try_from(message.received).unwrap());
    let payload = match message.encoding {
        layout::Encoding::Json => Payload::Json(&message.payload),
        layout::Encoding::Binary => Payload::Binary(&message.payload),
    };
    Message::new(payload, received)
}

/// A decoding error as the testkit writes one: the entry it names and what
/// it reports. `None` for an error of a kind the testkit doesn't plant.
fn failure(error: &DecodeError) -> Option<layout::Failure> {
    // Routing and identification name no entry.
    let whole = |problem| {
        Some(layout::Failure {
            path: String::new(),
            problem,
        })
    };
    let (path, problem) = match error {
        DecodeError::Route(route) => {
            return whole(match route {
                RouteError::WrongEncoding {
                    expected, given, ..
                } => layout::Problem::WrongEncoding {
                    expected: encoding(*expected)?,
                    given: encoding(*given)?,
                },
                RouteError::InvalidJson { .. } => layout::Problem::InvalidJson,
                RouteError::UnreadableLocator { locator, .. } => {
                    layout::Problem::UnreadableLocator {
                        role: match locator {
                            LocatorRole::Alias => layout::LocatorRole::Alias,
                            LocatorRole::Version => layout::LocatorRole::Version,
                            _ => return None,
                        },
                    }
                }
                RouteError::UnknownAsset { .. } => layout::Problem::UnknownAsset,
                _ => return None,
            });
        }
        DecodeError::NoPacket { .. } => return whole(layout::Problem::NoPacket),
        // The packet's, so it names no entry.
        DecodeError::TooShort {
            needed,
            given,
            exact,
            ..
        } => {
            let problem = layout::Problem::TooShort {
                needed_bytes: *needed,
                given_bytes: *given,
                exact: *exact,
            };
            let path = String::new();
            return Some(layout::Failure { path, problem });
        }
        DecodeError::Entry { path, problem, .. } => (path, problem),
        _ => return None,
    };
    let problem = match problem {
        Problem::PastEnd {
            needed_bits,
            available_bits,
        } => layout::Problem::PastEnd {
            needed_bits: *needed_bits,
            available_bits: *available_bits,
            padding: false,
        },
        Problem::PaddingPastEnd {
            needed_bits,
            available_bits,
        } => layout::Problem::PastEnd {
            needed_bits: *needed_bits,
            available_bits: *available_bits,
            padding: true,
        },
        Problem::NoTerminator { byte } => layout::Problem::NoTerminator { byte: *byte },
        // As the IR spells it.
        Problem::InvalidText { encoding } => layout::Problem::InvalidText {
            encoding: match encoding {
                TextEncoding::Utf8 => "utf8",
                TextEncoding::Ascii => "ascii",
                TextEncoding::Latin1 => "latin1",
                _ => "an encoding the testkit doesn't know",
            },
        },
        Problem::BadLength { value } => layout::Problem::BadLength { value: *value },
        Problem::BadChecksum { stored, computed } => layout::Problem::BadChecksum {
            stored: *stored,
            computed: *computed,
        },
        Problem::Missing { pointer } => layout::Problem::Missing {
            pointer: pointer.clone(),
        },
        Problem::WrongType { expected, found } => layout::Problem::WrongType {
            expected: kind(*expected),
            found: kind(*found),
        },
        Problem::OutOfRange { value, expected } => layout::Problem::OutOfRange {
            signed: match expected {
                ScalarType::Uint => false,
                ScalarType::Int => true,
                _ => return None,
            },
            value: value.clone(),
        },
        _ => return None,
    };
    Some(layout::Failure {
        path: path.clone(),
        problem,
    })
}

/// An encoding as the testkit names it.
fn encoding(encoding: Encoding) -> Option<layout::Encoding> {
    Some(match encoding {
        Encoding::Binary => layout::Encoding::Binary,
        Encoding::Json => layout::Encoding::Json,
        _ => return None,
    })
}

/// A JSON type as the testkit names it. JSON has six, so a match on
/// them needs no fallback.
fn kind(ty: JsonType) -> JsonKind {
    match ty {
        JsonType::Null => JsonKind::Null,
        JsonType::Boolean => JsonKind::Bool,
        JsonType::Number => JsonKind::Number,
        JsonType::String => JsonKind::String,
        JsonType::Array => JsonKind::Array,
        JsonType::Object => JsonKind::Object,
    }
}
