use std::collections::BTreeSet;

use proptest::strategy::{Strategy, ValueTree};
use proptest::test_runner::TestRunner;

use super::bundle;
use crate::coverage::{REQUIRED, features};

/// The generator reaches every feature of the IR, so a property over its
/// bundles is a property over all of the IR, not the parts it happens to
/// reach.
#[test]
fn bundles_reach_every_feature_of_the_ir() {
    let mut runner = TestRunner::deterministic();
    let strategy = bundle();
    let mut found = BTreeSet::new();
    for _ in 0..500 {
        let tree = strategy.new_tree(&mut runner).unwrap();
        found.extend(features(&tree.current()));
    }
    let missing: Vec<&str> = REQUIRED
        .iter()
        .copied()
        .filter(|feature| !found.contains(*feature))
        .collect();
    assert!(missing.is_empty(), "no generated bundle uses {missing:?}");
}
