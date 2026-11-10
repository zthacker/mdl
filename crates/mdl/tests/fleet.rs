//! Random fleets (`mdl_testkit::layout::fleet`): families of several sets
//! and assets, each message routed by its alias and its version or its
//! asset's binding at its ground time (bindings change set, retire and
//! reuse native ids), identified by one key or two (the more specific
//! packet winning), or routed to the error its fault calls for, with boot
//! time tied to instants by a correlator, keyed by family and asset, that
//! is saved and restored halfway, as a restarting consumer would.

mod common;

use std::collections::BTreeMap;

use mdl::bindings::Bindings;
use mdl::decode::Correlator;
use mdl::message::{Message, MetaValue, Payload};
use mdl::route::FamilySets;
use mdl::time::Timestamp;
use mdl_testkit::layout::{Meta, fleet};
use proptest::prelude::*;

proptest! {
    #[test]
    fn a_fleet_routes_decodes_and_correlates_every_message(case in fleet()) {
        let families: BTreeMap<&str, FamilySets> = case
            .families
            .iter()
            .map(|family| Ok((family.as_str(), common::load(&case.bundle, family)?)))
            .collect::<Result<_, TestCaseError>>()?;
        let bindings = Bindings::from_toml(&case.bindings).map_err(|error| {
            TestCaseError::fail(format!("bindings: {error}\n{}", case.bindings))
        })?;
        let mut correlator = Correlator::new();
        let half = case.messages.len() / 2;
        for (index, sent) in case.messages.iter().enumerate() {
            if index == half {
                let mut restored = Correlator::new();
                for family in &case.families {
                    for asset in &case.assets {
                        if let Some(booted) = correlator.boot_time(family, asset) {
                            restored.set_boot_time(family, asset, booted);
                        }
                    }
                }
                correlator = restored;
            }
            let message = &sent.message;
            let metadata: Vec<(&str, MetaValue<'_>)> = sent
                .metadata
                .iter()
                .map(|(name, value)| {
                    let value = match value {
                        Meta::Uint(n) => MetaValue::Uint(*n),
                        Meta::Text(text) => MetaValue::Text(text),
                    };
                    (name.as_str(), value)
                })
                .collect();
            let received = Timestamp::from_unix_nanos(i64::try_from(message.received).unwrap());
            let input =
                Message::new(Payload::Binary(&message.payload), received).with_metadata(&metadata);
            let result = families[sent.family.as_str()].decode(&input, &bindings);
            if let Some(code) = sent.error {
                prop_assert_eq!(result.err().map(|error| error.code()), Some(code), "{:?}", sent);
                continue;
            }
            let decoded = result.map_err(|error| {
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
