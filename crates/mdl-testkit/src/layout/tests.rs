//! The packer against hand-worked bytes, so the oracle doesn't rest only
//! on agreeing with `mdl`. The whole-byte vectors are what Python's
//! `struct.pack` gives (`<H`, `>H`, `<Q`, `>f`, `<f`).

use super::{convert_unit, fit, put_bits, reference, sign_extend, spline, table};

fn packed(length: usize, offset: u64, size: u32, value: u64, little: bool) -> Vec<u8> {
    let mut payload = vec![0; length];
    put_bits(&mut payload, offset, size, value, little);
    payload
}

#[test]
fn whole_bytes_are_written_in_their_order() {
    // struct.pack('<H', 0x1234) and ('>H', 0x1234), from byte 1.
    assert_eq!(packed(3, 8, 16, 0x1234, true), [0x00, 0x34, 0x12]);
    assert_eq!(packed(3, 8, 16, 0x1234, false), [0x00, 0x12, 0x34]);
    // struct.pack('<Q', 0x0102030405060708).
    assert_eq!(
        packed(8, 0, 64, 0x0102_0304_0506_0708, true),
        [8, 7, 6, 5, 4, 3, 2, 1]
    );
    // struct.pack('>f', 1.5) and ('<f', 1.5).
    let bits = u64::from(1.5_f32.to_bits());
    assert_eq!(packed(4, 0, 32, bits, false), [0x3f, 0xc0, 0x00, 0x00]);
    assert_eq!(packed(4, 0, 32, bits, true), [0x00, 0x00, 0xc0, 0x3f]);
}

#[test]
fn bits_are_numbered_from_the_top_of_byte_0() {
    // 0xABC in 12 bits from bit 4: 0000 1010 | 1011 1100.
    assert_eq!(packed(2, 4, 12, 0xabc, false), [0x0a, 0xbc]);
    // Inside one byte a little-endian value is written as big-endian:
    // 0b101 at bit 2 is 0010 1000.
    assert_eq!(packed(1, 2, 3, 0b101, true), [0x28]);
    assert_eq!(packed(1, 2, 3, 0b101, false), [0x28]);
}

#[test]
fn a_signed_value_is_twos_complement_in_its_size() {
    // -3 in 5 bits is 11101, from bit 0: 1110 1000.
    let raw = fit(-3_i64 as u64, 5);
    assert_eq!(raw, 0b11101);
    assert_eq!(packed(1, 0, 5, raw, false), [0xe8]);
    assert_eq!(sign_extend(raw, 5), -3);
    assert_eq!(sign_extend(fit(i64::MIN as u64, 64), 64), i64::MIN);
}

#[test]
fn writing_leaves_every_other_bit_as_it_was() {
    let mut payload = vec![0xff; 2];
    put_bits(&mut payload, 4, 8, 0, false);
    assert_eq!(payload, [0xf0, 0x0f]);
}

/// Generated layouts reach every meaning the oracle computes: each
/// conversion kind and option, units, sentinels, and every quality a
/// binary field can have.
#[test]
fn layouts_reach_every_meaning() {
    use std::collections::BTreeSet;

    use proptest::strategy::{Strategy, ValueTree};
    use proptest::test_runner::TestRunner;

    use super::telemetry;
    use crate::coverage::features;

    let mut runner = TestRunner::deterministic();
    let strategy = telemetry();
    let mut found = BTreeSet::new();
    let mut qualities = BTreeSet::new();
    for _ in 0..300 {
        let case = strategy.new_tree(&mut runner).unwrap().current();
        found.extend(features(&case.bundle));
        for message in &case.messages {
            qualities.extend(message.samples.iter().map(|sample| sample.quality));
        }
    }
    for feature in [
        "conversion:linear",
        "conversion:poly",
        "conversion:states",
        "conversion:table",
        "conversion:table/step",
        "conversion:table/extrapolate",
        "conversion:spline",
        "conversion:spline/extrapolate",
        "key:entry.unit",
        "key:entry.invalid",
        "key:entry.byte_order",
        "text:ascii",
        "text:latin1",
        "length:fixed/terminator",
        "byte_order:little",
        "byte_order:big",
    ] {
        assert!(found.contains(feature), "no layout uses {feature}");
    }
    for quality in [
        "good",
        "invalid",
        "non_finite",
        "out_of_calibration",
        "unknown_state",
    ] {
        assert!(qualities.contains(quality), "no sample is {quality}");
    }
}

/// Equal to the reference within what two different algorithms' rounding
/// leaves at `scale`: a relative 1e-10 of it, or of 1 near zero.
fn close(got: f64, want: f64, scale: f64) -> bool {
    (got - want).abs() <= 1e-10 * scale.abs().max(want.abs()).max(1.0)
}

/// A curve's scale: its largest value. SciPy evaluates its polynomials even
/// at the points, which the spec gives exactly, so it rounds there at this
/// scale.
fn scale(points: &[(f64, f64)]) -> f64 {
    points
        .iter()
        .map(|(_, value)| value.abs())
        .fold(0.0, f64::max)
}

/// The oracle's splines, tables and unit conversions agree with SciPy's and
/// pint's (`reference/generate.py`), so its arithmetic doesn't rest only on
/// agreeing with `mdl`.
#[test]
fn the_oracle_agrees_with_third_party_libraries() {
    for curve in reference::SPLINES {
        for &(x, want) in curve.values {
            let got = spline(curve.points, true, x);
            assert!(
                got.is_some_and(|got| close(got, want, scale(curve.points))),
                "spline {:?} at {x}: {got:?}, SciPy {want}",
                curve.points
            );
        }
    }
    for curve in reference::TABLES {
        for &(x, want) in curve.values {
            let got = table(curve.points, false, true, x);
            assert!(
                got.is_some_and(|got| close(got, want, scale(curve.points))),
                "table {:?} at {x}: {got:?}, SciPy {want}",
                curve.points
            );
        }
    }
    for &(from, to, value, want) in reference::UNITS {
        let got = convert_unit(value, from, to);
        assert!(
            close(got, want, want),
            "{value} {from} in {to}: {got}, pint {want}"
        );
    }
}
