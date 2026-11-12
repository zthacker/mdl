//! The body generator against itself: what it generates lays out whole.

use proptest::strategy::{Strategy, ValueTree};
use proptest::test_runner::TestRunner;

use super::{body_spec, lay_out};

/// Every composite finds its members once its body is laid out, in either
/// byte order, so none drops out of the bundle unnoticed.
#[test]
fn every_composite_finds_its_members() {
    let mut runner = TestRunner::deterministic();
    let strategy = body_spec();
    let mut composites = 0;
    for case in 0..400 {
        let spec = strategy.new_tree(&mut runner).unwrap().current();
        let wanted = spec.composites.len();
        let body = lay_out(spec, case % 2 == 0, 8, "P0", "p0");
        assert_eq!(body.composites.len(), wanted, "a composite didn't resolve");
        composites += wanted;
    }
    assert!(composites > 100, "only {composites} composites generated");
}
