//! The IR used from outside the crate, the way other crates use it.
//!
//! The `compile_fail` doctests in `ir.rs` prove struct literals don't
//! compile, but stable rustdoc doesn't check which error they fail with.
//! This file uses the same paths successfully, so if a type moves or is
//! renamed, this fails instead of those doctests silently passing.

use mdl::ir::{
    ArrayEntry, ByteLength, CompareOp, Condition, Conversion, Count, Element, Entry, Field, Length,
    Locator, Padding, ScalarElement, ScalarType, State, Time, TimeScale, TimeUnit,
};

#[test]
fn every_variant_is_built_with_its_constructor() {
    let mut field = Field::new("label", ScalarType::String);
    field.length = Some(ByteLength::remaining());
    field.when = Some(Condition::negate(Condition::compare("n", CompareOp::Eq, 0)));
    let mut element = ScalarElement::new(ScalarType::Int);
    element.size_bits = Some(16);
    element.conversion = Some(Conversion::states(vec![State::new(0, "SAFE")]));
    let mut array = ArrayEntry::new("samples", Element::Scalar(element));
    array.count = Some(Count::remaining());

    let entries = [
        Entry::Field(field),
        Entry::Array(array),
        Entry::Padding(Padding::new(3)),
    ];
    assert_eq!(entries.len(), 3);
    assert_eq!(Length::exact(4), Length::exact(4));
    assert_ne!(Locator::bits(0, 8), Locator::metadata("sysid"));
    assert_ne!(Conversion::linear(1.0, 0.0), Conversion::poly(vec![1.0]));
    assert_ne!(
        Time::iso8601("/t"),
        Time::cuc(0, 4, 2, "2000-01-01T00:00:00Z", TimeScale::Utc)
    );
    let _ = TimeUnit::Ms;
}

#[test]
fn variant_fields_are_matched_with_a_rest_pattern() {
    let Conversion::Linear { scale, .. } = Conversion::linear(0.5, 1.0) else {
        panic!("expected linear");
    };
    assert_eq!(scale, 0.5);
    assert!(matches!(Count::remaining(), Count::Remaining { .. }));
}
