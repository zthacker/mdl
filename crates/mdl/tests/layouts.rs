//! Random valid layouts (`mdl_testkit::layout`): each validates, builds
//! artifacts that read back with their digests, and, loaded from those,
//! decodes every message packed for it to exactly the raw values, samples
//! and event time the testkit worked out from the spec on its own. A
//! damaged message fails with exactly the error the testkit worked out.

mod common;

use mdl::bindings::Bindings;
use mdl::decode::{DecodeError, Problem};
use mdl::ir::TextEncoding;
use mdl::message::{Message, Payload};
use mdl::time::Timestamp;
use mdl_testkit::layout::{self, telemetry};
use proptest::prelude::*;

proptest! {
    #[test]
    fn a_valid_layout_decodes_what_was_packed_for_it(case in telemetry()) {
        let sets = common::load(&case.bundle, &case.family, &[])?;
        let bindings = bindings(&case);
        for message in &case.messages {
            let result = sets.decode(&message_of(message), &bindings);
            // An event time that can't be read is the message's error.
            if message.event_time == layout::EventTime::Invalid {
                prop_assert_eq!(result.err().map(|error| error.code()), Some("invalid_time"));
                continue;
            }
            let decoded =
                result.map_err(|error| TestCaseError::fail(format!("{error}: {message:?}")))?;
            common::check(&decoded, message)?;
        }
    }

    /// Faults planted in valid messages (`mdl_testkit::layout::damaged`):
    /// each damaged message fails with the error the testkit worked out,
    /// at the entry it names, with what that error reports.
    #[test]
    fn a_damaged_message_fails_where_the_spec_says(case in layout::damaged()) {
        let sets = common::load(&case.bundle, &case.family, &[])?;
        let bindings = bindings(&case);
        for message in &case.messages {
            let result = sets.decode(&message_of(message), &bindings);
            match &message.damage {
                Some(damage) => {
                    let error = result.err();
                    let failure = error.as_ref().and_then(failure);
                    prop_assert_eq!(
                        failure.as_ref(),
                        Some(&damage.failure),
                        "{}: decoding gave {:?}, payload {:02x?}",
                        damage.planted,
                        error.map(|error| error.to_string()),
                        message.payload
                    );
                }
                None if message.event_time == layout::EventTime::Invalid => {
                    prop_assert_eq!(result.err().map(|error| error.code()), Some("invalid_time"));
                }
                None => {
                    let decoded = result
                        .map_err(|error| TestCaseError::fail(format!("{error}: {message:?}")))?;
                    common::check(&decoded, message)?;
                }
            }
        }
    }
}

/// The binding of the case's native id to its set.
fn bindings(case: &layout::Telemetry) -> Bindings {
    Bindings::from_toml(&format!(
        "mdl_version = \"2.0\"\n\n[[binding]]\nfamily = \"{}\"\nnative_id = \"{}\"\n\
         asset = \"ASSET\"\nset = \"{}\"\neffective_from = \"1980-01-06T00:00:00Z\"\n",
        case.family, case.native_id, case.set
    ))
    .unwrap()
}

/// A message as a link delivers it.
fn message_of(message: &layout::Message) -> Message<'_> {
    let received = Timestamp::from_unix_nanos(i64::try_from(message.received).unwrap());
    Message::new(Payload::Binary(&message.payload), received)
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
        _ => return None,
    };
    Some(layout::Failure {
        path: path.clone(),
        problem,
    })
}
