//! Random fleets (`mdl_testkit::layout::fleet`): several sets and assets of
//! one family, each message routed by its alias and its version or its
//! asset's binding (which can change partway), identified by one key or
//! two (the more specific packet winning), with boot time tied to instants
//! by a correlator that is saved and restored halfway, as a restarting
//! consumer would.

mod common;

use mdl::bindings::Bindings;
use mdl::decode::Correlator;
use mdl::message::{Message, MetaValue, Payload};
use mdl::time::Timestamp;
use mdl_testkit::layout::fleet;
use proptest::prelude::*;

proptest! {
    #[test]
    fn a_fleet_routes_decodes_and_correlates_every_message(case in fleet()) {
        let sets = common::load(&case.bundle, &case.family)?;
        let bindings = Bindings::from_toml(&case.bindings).map_err(|error| {
            TestCaseError::fail(format!("bindings: {error}\n{}", case.bindings))
        })?;
        let mut correlator = Correlator::new();
        let half = case.messages.len() / 2;
        for (index, sent) in case.messages.iter().enumerate() {
            if index == half {
                let mut restored = Correlator::new();
                for asset in &case.assets {
                    if let Some(booted) = correlator.boot_time(&case.family, asset) {
                        restored.set_boot_time(&case.family, asset, booted);
                    }
                }
                correlator = restored;
            }
            let message = &sent.message;
            let metadata: Vec<(&str, MetaValue<'_>)> = sent
                .metadata
                .iter()
                .map(|(name, value)| (name.as_str(), MetaValue::Uint(*value)))
                .collect();
            let received = Timestamp::from_unix_nanos(i64::try_from(message.received).unwrap());
            let input =
                Message::new(Payload::Binary(&message.payload), received).with_metadata(&metadata);
            let decoded = sets.decode(&input, &bindings).map_err(|error| {
                TestCaseError::fail(format!("{error}: {sent:?}\n{}", case.bindings))
            })?;
            prop_assert_eq!(&decoded.route.binding.asset, &sent.asset);
            prop_assert_eq!(&decoded.route.set.name, &sent.set);
            common::check(&decoded, message)?;
            prop_assert_eq!(correlator.observe(&decoded), sent.teaches);
            prop_assert_eq!(correlator.absolute(&decoded).map(common::nanos), sent.absolute);
        }
    }
}
