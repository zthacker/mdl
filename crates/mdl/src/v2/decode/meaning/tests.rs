use super::*;

#[test]
fn sentinels_compare_by_value() {
    let invalid = [
        Scalar::Int(-9999),
        Scalar::Str("18446744073709551615".into()),
    ];
    assert!(is_sentinel(&invalid, &Value::Int(-9999)));
    assert!(is_sentinel(&invalid, &Value::Float(-9999.0)));
    assert!(is_sentinel(&invalid, &Value::Uint(u64::MAX)));
    assert!(!is_sentinel(&invalid, &Value::Float(-9999.5)));
    assert!(!is_sentinel(&invalid, &Value::Int(0)));
    let text = [Scalar::Str("N/A".into()), Scalar::Str("9999".into())];
    assert!(is_sentinel(&text, &Value::Str("N/A".into())));
    assert!(!is_sentinel(&text, &Value::Str("n/a".into())));
    // On a string field, a sentinel's digits are text.
    assert!(is_sentinel(&text, &Value::Str("9999".into())));
    assert!(!is_sentinel(&text, &Value::Str("09999".into())));
}

fn plain() -> ScalarMeaning<'static> {
    ScalarMeaning {
        conversion: None,
        invalid: &[],
        unit: None,
    }
}

#[test]
fn numbers_that_arent_finite_are_flagged_with_their_raw_value() {
    let f64_param = Param::new(ParamType::F64);
    let (value, quality) = sample_value(&f64_param, plain(), &Value::Float(f64::NAN)).unwrap();
    assert!(matches!(value, SampleValue::F64(x) if x.is_nan()));
    assert_eq!(quality, Quality::NonFinite);

    let huge = Conversion::linear(1e300, 0.0);
    let scaled = ScalarMeaning {
        conversion: Some(&huge),
        invalid: &[],
        unit: None,
    };
    assert_eq!(
        sample_value(&f64_param, scaled, &Value::Float(1e10)).unwrap(),
        (SampleValue::F64(1e10), Quality::NonFinite)
    );
    let engineering = engineering(scaled, &Value::Float(1e10)).unwrap();
    assert_eq!(
        (engineering.value, engineering.quality),
        (SampleValue::F64(1e10), Quality::NonFinite)
    );
    let engineering = engineering_of_plain(&Value::Float(f64::INFINITY));
    assert_eq!(engineering.quality, Quality::NonFinite);
}

fn engineering_of_plain(value: &Value<'_>) -> Engineering {
    engineering(plain(), value).unwrap()
}

#[test]
fn a_value_past_its_params_range_is_flagged_not_fatal() {
    let i64_param = Param::new(ParamType::I64);
    assert_eq!(
        sample_value(&i64_param, plain(), &Value::Uint(1 << 63)).unwrap(),
        (SampleValue::U64(1 << 63), Quality::OutOfRange)
    );
    assert_eq!(
        sample_value(&i64_param, plain(), &Value::Uint(7)).unwrap(),
        (SampleValue::I64(7), Quality::Good)
    );
}

#[test]
fn a_composite_has_its_worst_members_quality() {
    let composite = CompiledComposite {
        param: "nav.pos".to_string(),
        frame: Frame::Ecef,
        lineage: "x,y,z".to_string(),
        members: ["x", "y", "z"]
            .into_iter()
            .map(|name| Member {
                parts: vec![Part::Name(name.to_string())],
                conversion: None,
                invalid: vec![Scalar::Int(-1)],
                unit: None,
            })
            .collect(),
    };
    let mut catalog = Catalog::default();
    catalog
        .params
        .insert("nav.pos".to_string(), Param::new(ParamType::Vec3));
    let record = |values: [Value<'static>; 3]| {
        let mut record = Record::default();
        for (index, (name, value)) in ["x", "y", "z"].into_iter().zip(values).enumerate() {
            record.push(index, name, value);
        }
        record
    };
    let quality = |values| {
        composite
            .sample(&catalog, &record(values))
            .unwrap()
            .unwrap()
            .quality
    };
    assert_eq!(
        quality([Value::Float(1.0), Value::Float(2.0), Value::Float(3.0)]),
        Quality::Good
    );
    assert_eq!(
        quality([Value::Float(f64::NAN), Value::Float(2.0), Value::Float(3.0)]),
        Quality::NonFinite
    );
    assert_eq!(
        quality([Value::Float(f64::NAN), Value::Int(-1), Value::Float(3.0)]),
        Quality::Invalid
    );
}

#[test]
fn paths_have_one_spelling() {
    assert_eq!(
        parse_path("cells[2].volts"),
        Some(vec![
            Part::Name("cells".into()),
            Part::Index(2),
            Part::Name("volts".into())
        ])
    );
    for bad in ["", "a..b", "a[01]", "a[]", "a[-1]", "a[1", "[1]"] {
        assert_eq!(parse_path(bad), None, "{bad}");
    }
}
