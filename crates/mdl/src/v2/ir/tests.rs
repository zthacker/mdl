use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::*;
use crate::canonical::to_canonical_string;

/// A bundle that uses every IR shape at least once, written in normal form
/// (optional values omitted rather than null, empty collections omitted).
/// The fixture doubles as a reference document for other implementations.
const FULL_BUNDLE: &str = include_str!("../../../tests/fixtures/v2/full_bundle.json");

fn canonical<T: Serialize>(value: &T) -> String {
    to_canonical_string(&serde_json::to_value(value).unwrap()).unwrap()
}

fn rejects<T: DeserializeOwned + fmt::Debug>(value: Value) -> String {
    match serde_json::from_value::<T>(value) {
        Ok(parsed) => panic!("expected an error, parsed {parsed:?}"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn a_bundle_round_trips_to_the_same_canonical_text() {
    let input: Value = serde_json::from_str(FULL_BUNDLE).unwrap();
    let bundle: Bundle = serde_json::from_value(input.clone()).unwrap();
    // Compared as canonical text, the form identity is defined over:
    // serde_json's own equality would call `0` and `0.0` different.
    assert_eq!(canonical(&bundle), to_canonical_string(&input).unwrap());
}

#[test]
fn absent_values_are_omitted_not_written_as_null() {
    let mut field = Field::new("bus_mv", ScalarType::Uint);
    field.size_bits = Some(16);
    assert_eq!(
        serde_json::to_value(Entry::Field(field)).unwrap(),
        json!({ "kind": "field", "name": "bus_mv", "type": "uint", "size_bits": 16 })
    );
    assert_eq!(
        serde_json::to_value(Bundle::new(Catalog::default())).unwrap(),
        json!({ "mdl_version": "2.0", "catalog": {} })
    );
    assert_eq!(
        serde_json::to_value(Count::remaining()).unwrap(),
        json!({ "kind": "remaining" })
    );
}

#[test]
fn readers_accept_null_for_absent_values_and_empty_collections() {
    let entry: Entry = serde_json::from_value(json!({
        "kind": "field", "name": "x", "type": "uint", "size_bits": 8,
        "unit": null, "canonical": null, "when": null, "invalid": null
    }))
    .unwrap();
    let Entry::Field(field) = entry else {
        panic!("expected a field");
    };
    assert_eq!(field.unit, None);
    assert!(field.invalid.is_empty());

    let bundle: Bundle = serde_json::from_value(json!({
        "mdl_version": "2.0", "catalog": { "params": null }, "families": null, "sets": null
    }))
    .unwrap();
    assert_eq!(bundle, Bundle::new(Catalog::default()));
}

#[test]
fn non_finite_numbers_are_refused_when_writing() {
    let limits = Limits {
        red_high: Some(f64::NAN),
        ..Limits::default()
    };
    let error = serde_json::to_value(limits).unwrap_err().to_string();
    assert!(error.contains("must be finite"), "{error}");
    for conversion in [
        Conversion::linear(f64::INFINITY, 0.0),
        Conversion::poly(vec![0.0, f64::NEG_INFINITY]),
    ] {
        let error = serde_json::to_value(conversion).unwrap_err().to_string();
        assert!(error.contains("must be finite"), "{error}");
    }
    let mut array = ArrayEntry::new("a", Element::struct_type("t"));
    array.sample_interval_ms = Some(f64::NAN);
    assert!(serde_json::to_value(Entry::Array(array)).is_err());
}

/// Every struct, every struct variant, every newtype variant and every
/// empty-struct variant rejects a field it doesn't know.
#[test]
fn unknown_fields_are_rejected_everywhere() {
    let digest = "a".repeat(64);
    let mut failures = Vec::new();
    let mut check = |what: &str, error: String| {
        if !error.contains("unknown field") {
            failures.push(format!("{what}: {error}"));
        }
    };

    check(
        "bundle",
        rejects::<Bundle>(json!({ "mdl_version": "2.0", "catalog": {}, "x": 1 })),
    );
    check(
        "catalog command",
        rejects::<Catalog>(json!({ "commands": { "eps.go": { "argz": [] } } })),
    );
    check(
        "param",
        rejects::<Param>(json!({ "type": "f64", "units": "V" })),
    );
    check("limits", rejects::<Limits>(json!({ "red": 1.0 })));
    check(
        "artifact catalog",
        rejects::<Artifact>(
            json!({ "artifact": "catalog", "mdl_version": "2.0", "catalog": {}, "x": 1 }),
        ),
    );
    check(
        "artifact family",
        rejects::<Artifact>(json!({ "artifact": "family", "mdl_version": "2.0", "x": 1,
            "family": { "name": "f", "encoding": "json", "alias": { "kind": "path", "pointer": "/a" } } })),
    );
    check(
        "artifact set",
        rejects::<Artifact>(json!({ "artifact": "set", "mdl_version": "2.0", "x": 1,
            "catalog": digest, "family": digest, "set": { "name": "s", "family": "f" } })),
    );
    check(
        "family",
        rejects::<Family>(json!({ "name": "f", "encoding": "json", "endian": "big",
            "alias": { "kind": "path", "pointer": "/a" } })),
    );
    for locator in [
        json!({ "kind": "bits", "offset_bits": 0, "size_bits": 8, "signed": true }),
        json!({ "kind": "metadata", "name": "sysid", "x": 1 }),
        json!({ "kind": "path", "pointer": "/a", "x": 1 }),
        json!({ "kind": "fixed", "value": "a", "x": 1 }),
    ] {
        check("locator", rejects::<Locator>(locator));
    }
    check(
        "set",
        rejects::<Set>(json!({ "name": "s", "family": "f", "aliases": {} })),
    );
    for time in [
        json!({ "kind": "cuc", "offset_bits": 0, "coarse_bytes": 4, "fine_bytes": 2,
                "epoch": "2000-01-01T00:00:00Z", "leap": true }),
        json!({ "kind": "iso8601", "pointer": "/t", "x": 1 }),
        json!({ "kind": "boot", "unit": "ms", "x": 1, "correlate":
                { "packet": "p", "unix_field": "u", "unix_unit": "us", "boot_field": "b" } }),
    ] {
        check("time", rejects::<Time>(time));
    }
    check(
        "correlate",
        rejects::<Correlate>(json!({ "packet": "p", "unix_field": "u", "unix_unit": "us",
            "boot_field": "b", "x": 1 })),
    );
    check(
        "packet",
        rejects::<Packet>(json!({ "name": "p", "length_bytes": 4 })),
    );
    check(
        "length exact",
        rejects::<Length>(json!({ "kind": "exact", "bytes": 4, "x": 1 })),
    );
    check(
        "length at_least",
        rejects::<Length>(json!({ "kind": "at_least", "bytes": 4, "x": 1 })),
    );
    check(
        "struct type",
        rejects::<StructType>(json!({ "entries": [], "x": 1 })),
    );
    for entry in [
        json!({ "kind": "field", "name": "x", "type": "uint", "size_bits": 8, "scale": 2 }),
        json!({ "kind": "padding", "size_bits": 3, "fill": 0 }),
        json!({ "kind": "struct", "name": "s", "struct": "t", "at": 0 }),
        json!({ "kind": "array", "name": "a", "element": { "kind": "struct", "name": "t" }, "x": 1 }),
        json!({ "kind": "array", "name": "a",
                "element": { "kind": "scalar", "type": "uint", "bits": 8 } }),
        json!({ "kind": "array", "name": "a",
                "element": { "kind": "struct", "name": "t", "x": 1 } }),
    ] {
        check("entry", rejects::<Entry>(entry));
    }
    for count in [
        json!({ "kind": "fixed", "count": 2, "x": 1 }),
        json!({ "kind": "field", "field": "n", "x": 1 }),
        json!({ "kind": "remaining", "count": 7 }),
    ] {
        check("count", rejects::<Count>(count));
    }
    for length in [
        json!({ "kind": "fixed", "bytes": 4, "pad": 0 }),
        json!({ "kind": "field", "field": "n", "x": 1 }),
        json!({ "kind": "terminator", "byte": 0, "x": 1 }),
        json!({ "kind": "remaining", "byte": 0 }),
    ] {
        check("byte length", rejects::<ByteLength>(length));
    }
    for condition in [
        json!({ "kind": "compare", "field": "f", "op": "eq", "value": 1, "mask": 3 }),
        json!({ "kind": "in", "field": "f", "values": [1], "x": 1 }),
        json!({ "kind": "all", "conditions": [], "x": 1 }),
        json!({ "kind": "any", "conditions": [], "x": 1 }),
        json!({ "kind": "not", "condition": { "kind": "all", "conditions": [] }, "x": 1 }),
    ] {
        check("condition", rejects::<Condition>(condition));
    }
    for conversion in [
        json!({ "kind": "linear", "scale": 1.0, "offset": 0.0, "unit": "V" }),
        json!({ "kind": "poly", "coefficients": [1.0], "x": 1 }),
        json!({ "kind": "states", "states": [], "x": 1 }),
    ] {
        check("conversion", rejects::<Conversion>(conversion));
    }
    check(
        "state",
        rejects::<State>(json!({ "raw": 0, "name": "SAFE", "x": 1 })),
    );
    check(
        "composite",
        rejects::<Composite>(json!({ "canonical": "c", "frame": "ECEF", "members": [], "x": 1 })),
    );

    assert!(
        failures.is_empty(),
        "accepted unknown fields:\n{}",
        failures.join("\n")
    );
}

#[test]
fn unknown_kinds_name_the_choices() {
    let error = rejects::<Entry>(json!({ "kind": "union", "name": "u" }));
    assert!(error.contains("unknown variant `union`"), "{error}");
    assert!(error.contains("`field`"), "{error}");
}

#[test]
fn artifacts_are_tagged_and_sets_carry_their_dependencies() {
    let digest = |c: char| c.to_string().repeat(64);
    let artifact = json!({
        "artifact": "set",
        "mdl_version": "2.0",
        "catalog": digest('a'),
        "family": digest('b'),
        "set": { "name": "s", "family": "f" }
    });
    let parsed: Artifact = serde_json::from_value(artifact.clone()).unwrap();
    assert_eq!(parsed.mdl_version(), IrVersion::CURRENT);
    let Artifact::Set { catalog, set, .. } = &parsed else {
        panic!("expected a set artifact");
    };
    assert_eq!(catalog.as_str(), digest('a'));
    assert_eq!(set.family, "f");
    assert_eq!(serde_json::to_value(&parsed).unwrap(), artifact);
}

#[test]
fn digests_are_64_lowercase_hex_digits() {
    assert!("a".repeat(64).parse::<Digest>().is_ok());
    for bad in [
        "A".repeat(64),
        "a".repeat(63),
        "g".repeat(64),
        String::new(),
    ] {
        assert!(bad.parse::<Digest>().is_err(), "{bad}");
    }
    let error = rejects::<Digest>(json!("ABC"));
    assert!(error.contains("64 lowercase hex digits"), "{error}");
    assert_eq!(
        Digest::of_canonical("{}").as_str(),
        "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
    );
}

#[test]
fn scalars_are_integers_or_strings() {
    let parse = |v: Value| serde_json::from_value::<Scalar>(v);
    assert_eq!(parse(json!(-7)).unwrap(), Scalar::Int(-7));
    assert_eq!(parse(json!("PWR")).unwrap(), Scalar::Str("PWR".into()));
    assert_eq!(
        parse(json!("18446744073709551615")).unwrap(),
        Scalar::Str("18446744073709551615".into())
    );
    assert_eq!(
        parse(json!(1.5)).unwrap_err().to_string(),
        "expected an integer or a string, got a number with a fraction (1.5)"
    );
    assert_eq!(
        parse(json!(u64::MAX)).unwrap_err().to_string(),
        "integer 18446744073709551615 is too large for a JSON number here; write it as the \
         string \"18446744073709551615\""
    );
    assert!(parse(json!(true)).is_err());
    assert_eq!(serde_json::to_value(Scalar::Int(3)).unwrap(), json!(3));
}

#[test]
fn ir_versions_have_exactly_one_spelling() {
    let v: IrVersion = "2.0".parse().unwrap();
    assert_eq!(v, IrVersion::CURRENT);
    assert_eq!(v.to_string(), "2.0");
    assert_eq!(
        "10.12".parse::<IrVersion>().unwrap(),
        IrVersion::new(10, 12)
    );
    for bad in [
        "2", "2.0.1", "02.0", "2.00", "v2.0", "+2.0", "2.", ".0", "", "2.-1",
    ] {
        assert!(bad.parse::<IrVersion>().is_err(), "{bad:?}");
    }
    assert_eq!(serde_json::to_value(v).unwrap(), json!("2.0"));
    assert!(serde_json::from_value::<IrVersion>(json!(2.0)).is_err());
}

#[test]
fn a_reader_reads_older_minors_of_its_major_only() {
    let reader = IrVersion::new(2, 1);
    assert!(reader.can_read(IrVersion::new(2, 0)));
    assert!(reader.can_read(IrVersion::new(2, 1)));
    assert!(!reader.can_read(IrVersion::new(2, 2)));
    assert!(!reader.can_read(IrVersion::new(1, 0)));
    assert!(!reader.can_read(IrVersion::new(3, 0)));
}

#[test]
fn constructors_build_minimal_values() {
    let mut set = Set::new("vendor_x_v2", "vendor_x");
    set.version = Some(2.into());
    set.time = Some(Time::boot(
        TimeUnit::Ms,
        Correlate::new("TIME", "unix_us", TimeUnit::Us, "boot_ms"),
    ));
    let mut packet = Packet::new("HK");
    packet.identify.insert("apid".into(), 100.into());
    packet.length = Some(Length::at_least(12));
    let mut element = ScalarElement::new(ScalarType::Int);
    element.size_bits = Some(16);
    let mut samples = ArrayEntry::new("samples", Element::Scalar(element));
    samples.count = Some(Count::fixed(4));
    samples.when = Some(Condition::negate(Condition::compare("n", CompareOp::Eq, 0)));
    packet.entries.push(Entry::Array(samples));
    packet
        .entries
        .push(Entry::Struct(StructEntry::new("pos", "vec3_i32")));
    packet.entries.push(Entry::Padding(Padding::new(4)));
    set.packets.push(packet);

    let json = serde_json::to_value(&set).unwrap();
    assert_eq!(json["version"], json!(2));
    assert_eq!(json["packets"][0]["identify"], json!({ "apid": 100 }));
    assert_eq!(
        json["packets"][0]["entries"][0]["count"],
        json!({ "kind": "fixed", "count": 4 })
    );
    let back: Set = serde_json::from_value(json).unwrap();
    assert_eq!(back, set);
}

mod properties {
    use super::*;
    use proptest::prelude::*;

    /// Canonical text typed the way the reader types it, so the result
    /// doesn't depend on serde_json's feature flags.
    fn reread<T: DeserializeOwned>(text: &str) -> T {
        let value = crate::v2::read::value_from_canonical(text).unwrap();
        crate::v2::read::typed(&value).unwrap()
    }

    fn name() -> impl Strategy<Value = String> {
        "[a-z][a-z0-9_]{0,6}"
    }

    fn scalar() -> impl Strategy<Value = Scalar> {
        prop_oneof![
            (-(1_i64 << 53) + 1..(1_i64 << 53)).prop_map(Scalar::Int),
            ".{0,8}".prop_map(Scalar::Str),
        ]
    }

    fn finite() -> impl Strategy<Value = f64> {
        any::<f64>().prop_filter("finite", |n| n.is_finite())
    }

    fn condition() -> impl Strategy<Value = Condition> {
        let op = prop_oneof![
            Just(CompareOp::Eq),
            Just(CompareOp::Ne),
            Just(CompareOp::Lt),
            Just(CompareOp::Ge),
            Just(CompareOp::MaskAny),
            Just(CompareOp::MaskAll),
        ];
        let leaf = prop_oneof![
            (name(), op, scalar()).prop_map(|(f, op, v)| Condition::compare(f, op, v)),
            (name(), prop::collection::vec(scalar(), 1..4))
                .prop_map(|(f, vs)| Condition::is_in(f, vs)),
        ];
        leaf.prop_recursive(3, 16, 3, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 1..3).prop_map(Condition::all),
                prop::collection::vec(inner.clone(), 1..3).prop_map(Condition::any),
                inner.prop_map(Condition::negate),
            ]
        })
    }

    fn conversion() -> impl Strategy<Value = Conversion> {
        prop_oneof![
            (finite(), finite()).prop_map(|(s, o)| Conversion::linear(s, o)),
            prop::collection::vec(finite(), 1..4).prop_map(Conversion::poly),
            prop::collection::vec(
                (-1000_i64..1000, name()).prop_map(|(r, n)| State::new(r, n)),
                1..4
            )
            .prop_map(Conversion::states),
        ]
    }

    fn scalar_type() -> impl Strategy<Value = ScalarType> {
        prop_oneof![
            Just(ScalarType::Uint),
            Just(ScalarType::Int),
            Just(ScalarType::Float),
            Just(ScalarType::Bool),
            Just(ScalarType::String),
            Just(ScalarType::Bytes),
        ]
    }

    fn byte_length() -> impl Strategy<Value = ByteLength> {
        prop_oneof![
            any::<u32>().prop_map(ByteLength::fixed),
            name().prop_map(ByteLength::field),
            any::<u8>().prop_map(ByteLength::terminator),
            Just(ByteLength::remaining()),
        ]
    }

    fn field() -> impl Strategy<Value = Field> {
        (
            (
                name(),
                scalar_type(),
                any::<Option<u32>>(),
                any::<Option<u32>>(),
            ),
            (
                prop::option::of(byte_length()),
                prop::option::of(conversion()),
                prop::option::of(name()),
            ),
            (
                prop::collection::vec(scalar(), 0..3),
                prop::option::of(condition()),
            ),
        )
            .prop_map(
                |((n, ty, offset, size), (length, conversion, unit), (invalid, when))| {
                    let mut f = Field::new(n, ty);
                    f.offset_bits = offset;
                    f.size_bits = size;
                    f.length = length;
                    f.conversion = conversion;
                    f.unit = unit;
                    f.invalid = invalid;
                    f.when = when;
                    f
                },
            )
    }

    fn entry() -> impl Strategy<Value = Entry> {
        let count = prop_oneof![
            any::<u32>().prop_map(Count::fixed),
            name().prop_map(Count::field),
            Just(Count::remaining()),
        ];
        prop_oneof![
            field().prop_map(Entry::Field),
            (any::<u32>(), prop::option::of(condition())).prop_map(|(bits, when)| {
                let mut p = Padding::new(bits);
                p.when = when;
                Entry::Padding(p)
            }),
            (name(), name()).prop_map(|(n, t)| Entry::Struct(StructEntry::new(n, t))),
            (
                name(),
                prop::option::of(count),
                scalar_type(),
                prop::option::of(1.0_f64..1000.0)
            )
                .prop_map(|(n, count, ty, interval)| {
                    let mut a = ArrayEntry::new(n, Element::Scalar(ScalarElement::new(ty)));
                    a.count = count;
                    a.sample_interval_ms = interval;
                    Entry::Array(a)
                }),
        ]
    }

    proptest! {
        /// Typed value → JSON → canonical text → parsed back: the same
        /// typed value, and the same canonical text.
        #[test]
        fn entries_round_trip_through_canonical_json(entry in entry()) {
            let text = canonical(&entry);
            let back: Entry = reread(&text);
            prop_assert_eq!(&back, &entry);
            prop_assert_eq!(canonical(&back), text);
        }

        #[test]
        fn conditions_round_trip(condition in condition()) {
            let text = canonical(&condition);
            let back: Condition = reread(&text);
            prop_assert_eq!(back, condition);
        }

        #[test]
        fn scalars_round_trip(scalar in scalar()) {
            let back: Scalar = reread(&canonical(&scalar));
            prop_assert_eq!(back, scalar);
        }
    }
}

#[test]
fn literals_read_as_canonical_json_writes_them() {
    let read = |value: Value| serde_json::from_value::<Literal>(value);
    assert_eq!(read(json!(true)).unwrap(), Literal::Bool(true));
    assert_eq!(read(json!(3)).unwrap(), Literal::Int(3));
    // A number with no fraction is an integer, as canonical JSON writes it.
    assert_eq!(read(json!(3.0)).unwrap(), Literal::Int(3));
    assert_eq!(read(json!(-0.5)).unwrap(), Literal::Float(-0.5));
    assert_eq!(read(json!("0a1b")).unwrap(), Literal::Str("0a1b".into()));
    let error = read(json!(u64::MAX)).unwrap_err().to_string();
    assert!(error.contains("write it as the string"), "{error}");
    assert!(read(json!(null)).is_err());
    assert!(read(json!([1])).is_err());
    assert_eq!(
        canonical(&Literal::Float(0.25)),
        "0.25",
        "a fraction is written as a number"
    );
    assert!(serde_json::to_value(Literal::Float(f64::NAN)).is_err());
    // Literals compare by value, as their canonical JSON does.
    assert_eq!(Literal::Float(2.0), Literal::Int(2));
    assert_ne!(Literal::Float(2.5), Literal::Int(2));
    assert_ne!(Literal::Float(9.3e18), Literal::Int(i64::MAX));
}
