//! Random valid layouts (`mdl_testkit::layout`), binary and JSON: each
//! validates, builds artifacts that read back with their digests, and,
//! loaded from those, decodes every message packed for it to exactly the
//! raw values, samples and event time the testkit worked out from the spec
//! on its own. A damaged message fails with exactly the error the testkit
//! worked out.

mod common;

use mdl::bindings::Bindings;
use mdl::decode::{DecodeError, Problem};
use mdl::ir::TextEncoding;
use mdl::message::{Message, Payload};
use mdl::time::Timestamp;
use mdl_testkit::layout::{self, JsonKind, Wanted, telemetry};
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
        let result = sets.decode(&message_of(message, json), &bindings);
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
        let result = sets.decode(&message_of(message, json), &bindings);
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

/// A message as a link delivers it.
fn message_of(message: &layout::Message, json: bool) -> Message<'_> {
    let received = Timestamp::from_unix_nanos(i64::try_from(message.received).unwrap());
    let payload = if json {
        Payload::Json(&message.payload)
    } else {
        Payload::Binary(&message.payload)
    };
    Message::new(payload, received)
}

/// A decoding error as the testkit writes one: the entry it names and what
/// it reports. `None` for an error of a kind the testkit doesn't plant.
fn failure(error: &DecodeError) -> Option<layout::Failure> {
    let (path, problem) = match error {
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
            wanted: wanted(expected)?,
            found: found_kind(found)?,
        },
        Problem::OutOfRange { expected, .. } => layout::Problem::OutOfRange {
            signed: wanted(expected)? == Wanted::Int,
        },
        _ => return None,
    };
    Some(layout::Failure {
        path: path.clone(),
        problem,
    })
}

/// What a JSON entry takes, as MDL's `wrong_type` and `out_of_range` say
/// it.
fn wanted(expected: &str) -> Option<Wanted> {
    Some(match expected {
        "an unsigned 64-bit integer" => Wanted::Uint,
        "a signed 64-bit integer" => Wanted::Int,
        "a number" => Wanted::Float,
        "a boolean" => Wanted::Bool,
        "a string" => Wanted::String,
        "an array" => Wanted::Array,
        "an object" => Wanted::Object,
        _ => return None,
    })
}

/// A JSON value's kind, as MDL's `wrong_type` says what it found.
fn found_kind(found: &str) -> Option<JsonKind> {
    Some(match found {
        "null" => JsonKind::Null,
        "a boolean" => JsonKind::Bool,
        "a number" => JsonKind::Number,
        "a string" => JsonKind::String,
        "an array" => JsonKind::Array,
        "an object" => JsonKind::Object,
        _ => return None,
    })
}
