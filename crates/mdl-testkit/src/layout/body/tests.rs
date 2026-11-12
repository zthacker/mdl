//! The body generator against itself: what it generates lays out whole.

use std::ops::Range;

use proptest::strategy::{Strategy, ValueTree};
use proptest::test_runner::TestRunner;

use super::{body_spec, lay_out, without};

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

/// Spans keep their bits before and after each cut, and none a cut covers.
#[test]
fn spans_lose_the_bits_taken() {
    let spans = |pairs: &[(u64, u64)]| -> Vec<Range<u64>> {
        pairs.iter().map(|&(start, end)| start..end).collect()
    };
    let cases = [
        (
            vec![(0, 8), (16, 24)],
            vec![(4, 20)],
            vec![(0, 4), (20, 24)],
        ),
        (vec![(0, 8)], vec![(8, 16)], vec![(0, 8)]),
        (vec![(8, 16)], vec![(0, 32)], vec![]),
        (
            vec![(0, 32)],
            vec![(8, 16), (20, 24)],
            vec![(0, 8), (16, 20), (24, 32)],
        ),
    ];
    for (kept, taken, left) in cases {
        assert_eq!(without(&spans(&kept), &spans(&taken)), spans(&left));
    }
}
