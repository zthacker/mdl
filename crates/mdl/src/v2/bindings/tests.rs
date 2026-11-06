use proptest::prelude::*;

use super::*;
use crate::v2::read::read_bundle;
use crate::v2::validate::validate;

const FULL_BUNDLE: &str = include_str!("../../../tests/fixtures/v2/full_bundle.json");

fn fixture() -> Validated {
    validate(read_bundle(FULL_BUNDLE.as_bytes()).unwrap().value).unwrap()
}

fn at(text: &str) -> Timestamp {
    text.parse().unwrap()
}

fn id(text: &str) -> NativeId {
    NativeId::from_text(text)
}

fn shown(binding: Option<Arc<Binding>>) -> Option<(String, Option<String>)> {
    binding.map(|b| (b.asset.clone(), b.set.clone()))
}

fn bound(asset: &str, set: &str) -> Option<(String, Option<String>)> {
    Some((asset.to_string(), Some(set.to_string())))
}

fn codes(diagnostics: Diagnostics) -> Vec<(Code, String)> {
    diagnostics
        .iter()
        .map(|d| (d.code, d.path.clone()))
        .collect()
}

fn expected(pairs: &[(Code, &str)]) -> Vec<(Code, String)> {
    pairs
        .iter()
        .map(|(code, path)| (*code, path.to_string()))
        .collect()
}

#[test]
fn a_timeline_answers_with_the_row_in_effect() {
    let px9 = id("PX-9");
    // Out of order on purpose: rows are sorted into a timeline.
    let bindings = Bindings::new(vec![
        Row::retired("vendor_j", px9.clone(), at("2027-01-01T00:00:00Z")),
        Row::bound(
            "vendor_j",
            px9.clone(),
            at("2026-01-01T00:00:00Z"),
            Binding::new("SAT-57").with_set("vendor_j_v1"),
        ),
        Row::bound(
            "vendor_j",
            px9.clone(),
            at("2028-01-01T00:00:00Z"),
            Binding::new("SAT-99").with_set("vendor_j_v1"),
        ),
        Row::bound(
            "vendor_j",
            px9.clone(),
            at("2026-06-01T00:00:00Z"),
            Binding::new("SAT-57").with_set("vendor_j_v2"),
        ),
    ])
    .unwrap();
    let ask = |t: &str| shown(bindings.resolve("vendor_j", &px9, at(t)));
    assert_eq!(ask("2025-12-31T23:59:59.999999999Z"), None);
    assert_eq!(ask("2026-01-01T00:00:00Z"), bound("SAT-57", "vendor_j_v1"));
    assert_eq!(ask("2026-05-31T23:59:59Z"), bound("SAT-57", "vendor_j_v1"));
    assert_eq!(ask("2026-06-01T00:00:00Z"), bound("SAT-57", "vendor_j_v2"));
    assert_eq!(ask("2027-01-01T00:00:00Z"), None, "retired");
    assert_eq!(ask("2027-12-31T23:59:59Z"), None, "still retired");
    assert_eq!(ask("2028-01-01T00:00:00Z"), bound("SAT-99", "vendor_j_v1"));

    let t = at("2026-06-01T00:00:00Z");
    assert_eq!(bindings.resolve("vendor_x", &px9, t), None);
    assert_eq!(bindings.resolve("vendor_j", &id("PX-8"), t), None);
}

#[test]
fn native_ids_compare_by_value() {
    let t = at("2026-01-01T00:00:00Z");
    let bindings = Bindings::new(vec![Row::bound(
        "vendor_x",
        id("42"),
        t,
        Binding::new("SAT-42"),
    )])
    .unwrap();
    assert!(
        bindings
            .resolve("vendor_x", &NativeId::from(42_u64), t)
            .is_some()
    );
    assert!(
        bindings
            .resolve("vendor_x", &NativeId::from(&Scalar::Int(42)), t)
            .is_some()
    );
    assert!(bindings.resolve("vendor_x", &id("042"), t).is_none());

    // Every 64-bit integer is an integer id; beyond that range, text.
    assert_eq!(id("18446744073709551615"), NativeId::from(u64::MAX));
    assert_eq!(id("-9223372036854775808"), NativeId::from(i64::MIN));
    assert_eq!(
        id("18446744073709551616").as_text(),
        Some("18446744073709551616")
    );
    assert_eq!(id("-0").as_text(), Some("-0"));
    assert_eq!(NativeId::from(42), NativeId::from(42_u64));
}

#[test]
fn a_resolver_can_be_shared_across_threads() {
    fn shared<T: Send + Sync>(_: &T) {}
    let t = at("2026-01-01T00:00:00Z");
    let bindings = Bindings::new(vec![Row::bound(
        "vendor_x",
        NativeId::from(1),
        t,
        Binding::new("SAT-1"),
    )])
    .unwrap();
    let resolver: Arc<dyn Resolver> = Arc::new(bindings.clone());
    shared(&resolver);
    let worker = {
        let resolver = Arc::clone(&resolver);
        std::thread::spawn(move || shown(resolver.resolve("vendor_x", &NativeId::from(1), t)))
    };
    assert_eq!(worker.join().unwrap(), Some(("SAT-1".to_string(), None)));
    // References, boxes and Arcs of a resolver are resolvers.
    fn ask(resolver: impl Resolver, t: Timestamp) -> bool {
        resolver
            .resolve("vendor_x", &NativeId::from(1), t)
            .is_some()
    }
    assert!(ask(&bindings, t));
    assert!(ask(Box::new(bindings.clone()), t));
    assert!(ask(Arc::new(bindings), t));
}

proptest! {
    #[test]
    fn each_id_has_one_spelling(
        n in any::<i64>(),
        m in any::<u64>(),
        text in "\\PC{0,12}",
        digits in "-?[0-9]{1,21}",
    ) {
        for native_id in [NativeId::from(n), NativeId::from(m), id(&text), id(&digits)] {
            prop_assert_eq!(id(&native_id.to_string()), native_id);
        }
    }
}

#[test]
fn rows_are_checked_on_their_own() {
    let t = at("2026-01-01T00:00:00Z");
    let found = Bindings::new(vec![
        Row::bound("vendor_x", NativeId::from(1_u64), t, Binding::new("SAT-1")),
        Row::bound("Vendor X", NativeId::from(2_u64), t, Binding::new("")),
        Row::bound(
            "vendor_x",
            id(""),
            t,
            Binding::new("SAT-3").with_set("Bad-Set"),
        ),
        Row::retired("vendor_x", NativeId::from(1_u64), t),
    ])
    .unwrap_err();
    assert_eq!(
        codes(found),
        expected(&[
            (Code::Empty, "/binding/1/asset"),
            (Code::InvalidName, "/binding/1/family"),
            (Code::Empty, "/binding/2/native_id"),
            (Code::InvalidName, "/binding/2/set"),
            (Code::DuplicateBinding, "/binding/3/effective_from"),
        ])
    );
}

const FILE: &str = r#"
mdl_version = "2.0"

[[binding]]
family = "vendor_x"
native_id = 42
asset = "SAT-42"
effective_from = "2026-01-01T00:00:00Z"

[[binding]]
family = "vendor_j"
native_id = "PX-9"
asset = "SAT-57"
set = "vendor_j_v1"
effective_from = "2026-06-01T00:00:00Z"

[[binding]]
family = "vendor_j"
native_id = "PX-9"
retired = true
effective_from = "2027-01-01T00:00:00Z"

[[binding]]
family = "legacy"
native_id = "box-1"
asset = "BOX-1"
set = "legacy_v1"
effective_from = "2020-01-01T00:00:00Z"
"#;

#[test]
fn a_bindings_file_reads_into_the_same_timelines_as_rows() {
    let from_file = Bindings::from_toml(FILE).unwrap();
    let from_rows = Bindings::new(vec![
        Row::bound(
            "vendor_x",
            NativeId::from(42_u64),
            at("2026-01-01T00:00:00Z"),
            Binding::new("SAT-42"),
        ),
        Row::bound(
            "vendor_j",
            id("PX-9"),
            at("2026-06-01T00:00:00Z"),
            Binding::new("SAT-57").with_set("vendor_j_v1"),
        ),
        Row::retired("vendor_j", id("PX-9"), at("2027-01-01T00:00:00Z")),
        Row::bound(
            "legacy",
            id("box-1"),
            at("2020-01-01T00:00:00Z"),
            Binding::new("BOX-1").with_set("legacy_v1"),
        ),
    ])
    .unwrap();
    assert_eq!(from_file, from_rows);
    assert_eq!(from_file.check(&fixture()), Ok(()));
    assert_eq!(
        Bindings::from_toml("mdl_version = \"2.0\"\n"),
        Ok(Bindings::default())
    );
}

#[test]
fn whole_file_rejections_have_codes() {
    let code = |text: &str| Bindings::from_toml(text).unwrap_err().code();
    assert_eq!(code("mdl_version = "), Some("invalid_toml"));
    assert_eq!(code("[[binding]]"), Some("missing_version"));
    assert_eq!(code("mdl_version = \"two\""), Some("invalid_version"));
    assert_eq!(code("mdl_version = \"3.0\""), Some("unsupported_version"));
    assert_eq!(
        code("mdl_version = \"2.0\"\nbindings = []"),
        Some("wrong_shape")
    );
    // The version is checked before anything else in the file.
    assert_eq!(
        code("mdl_version = \"2.1\"\nconfidence = 0.9"),
        Some("unsupported_version")
    );
    assert_eq!(code("mdl_version = 2026-01-01"), Some("invalid_version"));
    assert_eq!(code("x = 1.5"), Some("missing_version"));
    let invalid = "mdl_version = \"2.0\"\n[[binding]]\nfamily = \"x\"\nnative_id = 1\neffective_from = \"2026-01-01T00:00:00Z\"";
    assert_eq!(code(invalid), None);
}

#[test]
fn a_row_that_cant_be_built_still_reports_its_own_problems() {
    let text = r#"
mdl_version = "2.0"

[[binding]]
family = "Bad-Name"
native_id = 1
asset = ""
effective_from = "2026-01-01"

[[binding]]
family = "Also Bad"
native_id = 2
effective_from = "2026-01-01T00:00:00Z"

[[binding]]
family = "vendor_x"
native_id = 3
asset = "A"
retired = false
effective_from = "2026-01-01T00:00:00Z"

[[binding]]
family = "vendor_x"
native_id = 3
asset = "B"
effective_from = "2026-01-01T00:00:00Z"
"#;
    let found = match Bindings::from_toml(text) {
        Err(BindingsError::Invalid(diagnostics)) => codes(diagnostics),
        other => panic!("expected diagnostics, got {other:?}"),
    };
    assert_eq!(
        found,
        expected(&[
            (Code::Empty, "/binding/0/asset"),
            (Code::InvalidInstant, "/binding/0/effective_from"),
            (Code::InvalidName, "/binding/0/family"),
            (Code::BindingShape, "/binding/1"),
            (Code::InvalidName, "/binding/1/family"),
            // retired = false with an asset is still a binding, so row 3
            // repeats it.
            (Code::ExplicitDefault, "/binding/2/retired"),
            (Code::DuplicateBinding, "/binding/3/effective_from"),
        ])
    );
}

fn read_error(text: &str) -> ReadError {
    match Bindings::from_toml(text) {
        Err(BindingsError::Read(err)) => err,
        other => panic!("expected a read error, got {other:?}"),
    }
}

fn shape_path(text: &str) -> String {
    match read_error(text) {
        ReadError::Shape { path, .. } => path,
        other => panic!("expected a shape error, got {other:?}"),
    }
}

const ROW: &str = "family = \"vendor_x\"\nasset = \"A\"\n";

#[test]
fn a_bindings_file_has_one_shape() {
    assert!(matches!(
        Bindings::from_toml("mdl_version = "),
        Err(BindingsError::Syntax(_))
    ));
    assert_eq!(
        read_error("[[binding]]\nfamily = \"vendor_x\""),
        ReadError::MissingVersion
    );
    assert!(matches!(
        read_error("mdl_version = \"3.0\""),
        ReadError::UnsupportedVersion { .. }
    ));
    let typo = format!(
        "mdl_version = \"2.0\"\n[[binding]]\n{ROW}native_id = 1\nefective_from = \"2026-01-01T00:00:00Z\""
    );
    match read_error(&typo) {
        ReadError::Shape { path, message } => {
            assert_eq!(path, "/binding/0/efective_from");
            assert!(message.contains("unknown field"), "{message}");
        }
        other => panic!("expected a shape error, got {other:?}"),
    }
    let unquoted = format!(
        "mdl_version = \"2.0\"\n[[binding]]\n{ROW}native_id = 1\neffective_from = 2026-01-01T00:00:00Z"
    );
    assert_eq!(shape_path(&unquoted), "/binding/0/effective_from");
    let fractional = format!(
        "mdl_version = \"2.0\"\n[[binding]]\n{ROW}native_id = 1.0\neffective_from = \"2026-01-01T00:00:00Z\""
    );
    assert_eq!(shape_path(&fractional), "/binding/0/native_id");
}

#[test]
fn every_problem_in_a_bindings_file_is_reported_at_once() {
    let text = r#"
mdl_version = "2.0"

[[binding]]
family = "vendor_x"
native_id = 1
asset = "A"
effective_from = "2026-01-01T00:00:00.50Z"

[[binding]]
family = "vendor_x"
native_id = "2"
asset = "B"
effective_from = "2026-01-01T00:00:00Z"

[[binding]]
family = "vendor_x"
native_id = 3
asset = "C"
retired = false
effective_from = "2026-01-01T00:00:00Z"

[[binding]]
family = "vendor_j"
native_id = "PX-1"
asset = "D"
set = "vendor_j_v1"
retired = true
effective_from = "2026-01-01T00:00:00Z"

[[binding]]
family = "vendor_j"
native_id = "PX-2"
effective_from = "2026-01-01T00:00:00Z"

[[binding]]
family = "vendor_x"
native_id = 9007199254740993
asset = "E"
effective_from = "2026-01-01T00:00:00Z"

[[binding]]
family = "vendor_x"
native_id = 2
asset = "F"
effective_from = "2026-01-01T00:00:00Z"
"#;
    let found = match Bindings::from_toml(text) {
        Err(BindingsError::Invalid(diagnostics)) => codes(diagnostics),
        other => panic!("expected diagnostics, got {other:?}"),
    };
    assert_eq!(
        found,
        expected(&[
            (Code::InvalidInstant, "/binding/0/effective_from"),
            (Code::IntegerAsString, "/binding/1/native_id"),
            (Code::ExplicitDefault, "/binding/2/retired"),
            (Code::BindingShape, "/binding/3/asset"),
            (Code::BindingShape, "/binding/3/set"),
            (Code::BindingShape, "/binding/4"),
            (Code::UnsafeInteger, "/binding/5/native_id"),
            // "2" and 2 are one id, so row 6 repeats row 1's instant.
            (Code::DuplicateBinding, "/binding/6/effective_from"),
        ])
    );
}

#[test]
fn bindings_are_checked_against_the_definitions() {
    let t = at("2026-01-01T00:00:00Z");
    let bindings = Bindings::new(vec![
        Row::bound("nope", NativeId::from(1_u64), t, Binding::new("A")),
        Row::bound("vendor_x", NativeId::from(70_000_u64), t, Binding::new("B")),
        Row::bound("vendor_x", id("PX-9"), t, Binding::new("C")),
        Row::bound(
            "vendor_x",
            NativeId::from(7_u64),
            t,
            Binding::new("D").with_set("vendor_x_v2"),
        ),
        Row::bound("vendor_j", id("PX-9"), t, Binding::new("E")),
        Row::bound(
            "vendor_j",
            id("PX-8"),
            t,
            Binding::new("F").with_set("vendor_j_v9"),
        ),
        Row::bound(
            "vendor_j",
            id("PX-7"),
            t,
            Binding::new("G").with_set("legacy_v1"),
        ),
        Row::bound(
            "legacy",
            id("box-2"),
            t,
            Binding::new("H").with_set("legacy_v1"),
        ),
        // A retirement names no set, but its native id is still checked.
        Row::retired("vendor_x", NativeId::from(70_001_u64), t),
        Row::retired("vendor_j", id("PX-9"), at("2027-01-01T00:00:00Z")),
        Row::bound("vendor_x", NativeId::from(65_535_u64), t, Binding::new("I")),
        Row::bound(
            "legacy",
            id("box-1"),
            t,
            Binding::new("J").with_set("legacy_v1"),
        ),
    ])
    .unwrap();
    assert_eq!(
        codes(bindings.check(&fixture()).unwrap_err()),
        expected(&[
            (Code::UnknownFamily, "/binding/0/family"),
            (Code::NativeId, "/binding/1/native_id"),
            (Code::NativeId, "/binding/2/native_id"),
            (Code::BindingSet, "/binding/3/set"),
            (Code::BindingSet, "/binding/4"),
            (Code::UnknownSet, "/binding/5/set"),
            (Code::UnknownSet, "/binding/6/set"),
            (Code::NativeId, "/binding/7/native_id"),
            (Code::NativeId, "/binding/8/native_id"),
        ])
    );
}
