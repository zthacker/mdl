//! Random valid layouts (`mdl_testkit::layout`): each validates, builds
//! artifacts that read back with their digests, and, loaded from those,
//! decodes every message packed for it to exactly the raw values, samples
//! and event time the testkit worked out from the spec on its own.

mod common;

use mdl::bindings::Bindings;
use mdl::message::{Message, Payload};
use mdl::time::Timestamp;
use mdl_testkit::layout::{self, telemetry};
use proptest::prelude::*;

proptest! {
    #[test]
    fn a_valid_layout_decodes_what_was_packed_for_it(case in telemetry()) {
        let sets = common::load(&case.bundle, &case.family)?;
        let bindings = Bindings::from_toml(&format!(
            "mdl_version = \"2.0\"\n\n[[binding]]\nfamily = \"{}\"\nnative_id = \"{}\"\n\
             asset = \"ASSET\"\nset = \"{}\"\neffective_from = \"1980-01-06T00:00:00Z\"\n",
            case.family, case.native_id, case.set
        ))
        .unwrap();
        for message in &case.messages {
            let received = Timestamp::from_unix_nanos(i64::try_from(message.received).unwrap());
            let result =
                sets.decode(&Message::new(Payload::Binary(&message.payload), received), &bindings);
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
}
