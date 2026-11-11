use mdl::ir::{Argument, RangeEnds};
use mdl::source::{Location, Sources};
use mdl::validate::validate;
use proptest::prelude::*;
use serde_json::{Value, json};

use super::*;

const CATALOG: &str = r#"mdl_version = "2.0"

[catalog.params."eps.bus_voltage"]
type = "f64"
unit = "V"
"#;

const FAMILY: &str = r#"mdl_version = "2.0"

[[families]]
name = "vendor_x"
encoding = "binary"
byte_order = "big"
alias = { kind = "bits", offset_bits = 0, size_bits = 16 }
"#;

fn set(name: &str, unit: &str) -> String {
    format!(
        r#"mdl_version = "2.0"

[[sets]]
name = "{name}"
family = "vendor_x"

[[sets.packets]]
name = "HK"
length = {{ kind = "exact", bytes = 4 }}

[[sets.packets.entries]]
kind = "field"
name = "bus"
type = "uint"
offset_bits = 16
size_bits = 16
conversion = {{ kind = "linear", scale = 0.001, offset = 0 }}
unit = "{unit}"
canonical = "eps.bus_voltage"
"#
    )
}

fn sources(files: &[(&str, &str)]) -> Sources {
    let mut sources = Sources::new();
    for (name, text) in files {
        sources.add(*name, *text);
    }
    sources
}

type Place = Option<(String, Location)>;

fn place(sources: &Sources, span: Option<Span>) -> Place {
    let span = span?;
    Some((
        sources.name(span.source)?.to_string(),
        sources.location(span)?,
    ))
}

/// The errors reading `files` gives, each with its file and position.
fn errors(files: &[(&str, &str)]) -> Vec<(SourceDiagnostic, Place)> {
    errors_and_firsts(files)
        .into_iter()
        .map(|(error, at, _)| (error, at))
        .collect()
}

/// The same, with where each duplicate's first definition is.
fn errors_and_firsts(files: &[(&str, &str)]) -> Vec<(SourceDiagnostic, Place, Place)> {
    let sources = sources(files);
    read(sources.iter())
        .unwrap_err()
        .into_iter()
        .map(|error| {
            let at = place(&sources, error.span);
            let first = place(&sources, error.first);
            (error, at, first)
        })
        .collect()
}

fn location(text: &str, needle: &str) -> Location {
    let mut sources = Sources::new();
    let id = sources.add("t", text);
    sources
        .get(id)
        .unwrap()
        .location(text.find(needle).unwrap())
}

#[test]
fn files_merge_into_one_bundle_that_validates() {
    let a = set("vendor_x_v1", "V");
    let b = set("vendor_x_v2", "mV");
    let sources = sources(&[
        ("catalog.toml", CATALOG),
        ("family.toml", FAMILY),
        ("a.toml", &a),
        ("b.toml", &b),
    ]);
    let parsed = read(sources.iter()).unwrap();
    let names: Vec<_> = parsed.bundle.sets.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["vendor_x_v1", "vendor_x_v2"]);
    // The second file's set is /sets/1, and its spans are in that file.
    let unit = parsed.map.get("/sets/1/packets/0/entries/0/unit").unwrap();
    assert_eq!(sources.name(unit.source), Some("b.toml"));
    assert_eq!(&b[unit.start..unit.end], "\"mV\"");
    // A list built from two files has no span; from one file, it has.
    assert_eq!(parsed.map.get("/sets"), None);
    assert!(parsed.map.get("/families").is_some());
    assert!(parsed.map.get("/catalog/params").is_some());
    assert!(validate(parsed.bundle).is_ok());
}

#[test]
fn validation_diagnostics_point_into_the_file_that_says_it() {
    let bad = set("vendor_x_v1", "A");
    let sources = sources(&[
        ("catalog.toml", CATALOG),
        ("family.toml", FAMILY),
        ("set.toml", &bad),
    ]);
    let parsed = read(sources.iter()).unwrap();
    let diagnostics = validate(parsed.bundle).unwrap_err();
    let mismatch = parsed
        .map
        .locate_all(&diagnostics)
        .into_iter()
        .find(|d| d.code == "unit_mismatch")
        .unwrap();
    let span = mismatch.span.unwrap();
    assert_eq!(sources.name(span.source), Some("set.toml"));
    assert_eq!(sources.location(span), Some(location(&bad, "\"A\"")));
}

#[test]
fn a_default_a_command_packet_cant_write_is_reported_where_the_packet_takes_it() {
    let text = r#"mdl_version = "2.0"

[catalog.commands."sink.label"]
arguments = [{ name = "label", type = "str", default = "OFF" }]

[[families]]
name = "sink"
encoding = "binary"
byte_order = "big"
alias = { kind = "fixed", value = "S1" }

[[sets]]
name = "sink_v1"
family = "sink"

[[sets.commands]]
name = "LABEL"
canonical = "sink.label"

[[sets.commands.entries]]
kind = "field"
name = "label"
type = "string"
length = { kind = "fixed", bytes = 2 }
argument = "label"
"#;
    let sources = sources(&[("sink.toml", text)]);
    let parsed = read(sources.iter()).unwrap();
    let diagnostics = validate(parsed.bundle).unwrap_err();
    let located = parsed.map.locate_all(&diagnostics);
    let codes: Vec<&str> = located.iter().map(|d| d.code).collect();
    assert_eq!(codes, ["default_value"]);
    let span = located[0].span.unwrap();
    assert_eq!(&text[span.start..span.end], "\"label\"");
    let taken = text.find("argument = \"label\"").unwrap() + "argument = ".len();
    assert_eq!(span.start, taken);
}

#[test]
fn a_param_in_two_files_is_a_conflict_naming_the_first() {
    let again =
        "mdl_version = \"2.0\"\n\n[catalog.params]\n\"eps.bus_voltage\" = { type = \"i64\" }\n";
    let found = errors_and_firsts(&[("catalog.toml", CATALOG), ("more.toml", again)]);
    assert_eq!(found.len(), 1);
    let (error, at, first) = &found[0];
    assert_eq!(error.code, "toml.conflict");
    assert_eq!(error.path, "/catalog/params/eps.bus_voltage");
    assert_eq!(
        error.message,
        "catalog.params.\"eps.bus_voltage\" is defined in two files"
    );
    assert_eq!(
        at,
        &Some(("more.toml".to_string(), location(again, "{ type")))
    );
    assert_eq!(
        first,
        &Some((
            "catalog.toml".to_string(),
            location(CATALOG, "[catalog.params")
        ))
    );
}

#[test]
fn one_family_in_two_files_names_the_first_file_and_line() {
    let sources = sources(&[
        ("catalog.toml", CATALOG),
        ("a.toml", FAMILY),
        ("b.toml", FAMILY),
    ]);
    let parsed = read(sources.iter()).unwrap();
    let diagnostics = validate(parsed.bundle).unwrap_err();
    let duplicate = parsed
        .map
        .locate_all(&diagnostics)
        .into_iter()
        .find(|d| d.code == "duplicate_name")
        .unwrap();
    assert_eq!(duplicate.message, "family vendor_x is defined twice");
    let header = location(FAMILY, "[[families]]");
    assert_eq!(
        place(&sources, duplicate.span),
        Some(("b.toml".to_string(), header))
    );
    assert_eq!(
        place(&sources, duplicate.first),
        Some(("a.toml".to_string(), header))
    );
    let rendered = duplicate.display(&sources).to_string();
    assert!(
        rendered.ends_with(&format!(
            "= the first is at a.toml:{}:{}\n",
            header.line, header.column
        )),
        "{rendered}"
    );
}

#[test]
fn a_list_and_a_whole_value_at_one_key_conflict() {
    let whole = "mdl_version = \"2.0\"\nfamilies = \"none\"\n";
    let found = errors(&[("family.toml", FAMILY), ("other.toml", whole)]);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0.code, "toml.conflict");
    assert!(
        found[0]
            .0
            .message
            .starts_with("families is defined in two files")
    );
    // And the other way around.
    let found = errors(&[("other.toml", whole), ("family.toml", FAMILY)]);
    assert_eq!(found[0].0.code, "toml.conflict");
    assert_eq!(found[0].1.as_ref().unwrap().0, "family.toml");
}

#[test]
fn every_file_states_a_version_of_one_major() {
    let found = errors(&[
        ("catalog.toml", CATALOG),
        ("bare.toml", "[catalog.params]\n"),
    ]);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0.code, "toml.version");
    assert_eq!(
        found[0].1,
        Some(("bare.toml".to_string(), Location { line: 1, column: 1 }))
    );

    let three = "mdl_version = \"3.0\"\n";
    let found = errors(&[("catalog.toml", CATALOG), ("three.toml", three)]);
    assert_eq!(found[0].0.code, "toml.version");
    assert!(
        found[0].0.message.contains("an earlier one is 2.0"),
        "{}",
        found[0].0.message
    );
    let found = errors_and_firsts(&[("catalog.toml", CATALOG), ("three.toml", three)]);
    assert_eq!(
        found[0].2,
        Some(("catalog.toml".to_string(), location(CATALOG, "\"2.0\"")))
    );

    for bad in ["mdl_version = 2\n", "mdl_version = \"2\"\n"] {
        let found = errors(&[("v.toml", bad)]);
        assert_eq!(found[0].0.code, "toml.version", "{bad}");
    }
}

#[test]
fn the_bundle_has_the_highest_version_and_the_reader_checks_it() {
    let newer = "mdl_version = \"2.1\"\n";
    let found = errors(&[("catalog.toml", CATALOG), ("newer.toml", newer)]);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0.code, "unsupported_version");
    assert_eq!(
        found[0].1,
        Some(("newer.toml".to_string(), location(newer, "\"2.1\"")))
    );
}

#[test]
fn syntax_errors_are_reported_for_every_file_in_order() {
    let found = errors(&[
        ("a.toml", "mdl_version = \"2.0\"\nx = \n"),
        ("b.toml", CATALOG),
        ("c.toml", "mdl_version = \"2.0\"\nk = 1\nk = 2\n"),
    ]);
    let codes: Vec<_> = found
        .iter()
        .map(|(e, p)| (e.code, p.as_ref().unwrap().0.as_str()))
        .collect();
    assert_eq!(
        codes,
        [("toml.syntax", "a.toml"), ("toml.syntax", "c.toml")]
    );
    assert_eq!(
        found[1].1.as_ref().unwrap().1,
        Location { line: 3, column: 1 }
    );
}

#[test]
fn unknown_keys_are_the_readers_shape_errors_located_in_toml() {
    let text = "mdl_version = \"2.0\"\n[catalog]\ncolour = \"red\"\n";
    let found = errors(&[("c.toml", text)]);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0.code, "wrong_shape");
    assert!(found[0].0.message.contains("unknown field `colour`"));
    assert_eq!(
        found[0].1,
        Some(("c.toml".to_string(), location(text, "\"red\"")))
    );
}

/// One TOML file's top-level table as JSON, with its errors.
fn converted(text: &str) -> (Value, Vec<SourceDiagnostic>, SourceMap) {
    let mut sources = Sources::new();
    let id = sources.add("v.toml", text);
    let file = sources.get(id).unwrap();
    let table = DeTable::parse(text).unwrap();
    let whole = Spanned::new(table.span(), DeValue::Table(table.into_inner()));
    let mut map = SourceMap::new();
    let mut errors = Vec::new();
    let value = Converter {
        file,
        map: &mut map,
        errors: &mut errors,
    }
    .convert(&whole, &mut String::new());
    (value, errors, map)
}

#[test]
fn integers_beyond_what_json_reads_exactly_become_decimal_strings() {
    let (value, errors, _) = converted(
        "apid = 0x064\nmask = 0b1010\nsafe = 9007199254740991\nwide = 9007199254740992\n\
         low = -9007199254740992\nall = 0x7FFFFFFFFFFFFFFF\nsigned = -42\n",
    );
    assert!(errors.is_empty());
    assert_eq!(
        value,
        json!({
            "apid": 100,
            "mask": 10,
            "safe": 9007199254740991u64,
            "wide": "9007199254740992",
            "low": "-9007199254740992",
            "all": "9223372036854775807",
            "signed": -42
        })
    );
}

/// A range end past ±(2^53 - 1) is written as a plain TOML integer, and
/// read as the decimal string the IR writes; past i64, where TOML has no
/// integer, as that string itself.
#[test]
fn a_wide_range_end_is_a_plain_toml_integer() {
    let command = r#"mdl_version = "2.0"

[catalog.commands."eps.stamp"]
arguments = [
    { name = "low", type = "u64", range = { min = 1, max = 1152921504606846975 } },
    { name = "high", type = "u64", range = { min = 0, max = "18446744073709551615" } },
]
"#;
    let sources = sources(&[("command.toml", command)]);
    let parsed = read(sources.iter()).unwrap();
    let arguments = &parsed.bundle.catalog.commands["eps.stamp"].arguments;
    let ranges: Vec<_> = arguments.iter().map(Argument::range_ends).collect();
    assert_eq!(
        ranges,
        [
            Some(RangeEnds::Integers(Some(1), Some((1 << 60) - 1))),
            Some(RangeEnds::Integers(Some(0), Some(i128::from(u64::MAX)))),
        ]
    );
    assert!(validate(parsed.bundle).is_ok());
}

#[test]
fn numbers_json_cant_hold_are_errors_at_the_number() {
    let text = "a = 99999999999999999999\nb = nan\nc = -inf\nd = 1.5\n";
    let (value, errors, _) = converted(text);
    let codes: Vec<_> = errors.iter().map(|e| e.code).collect();
    assert_eq!(codes, ["toml.number"; 3]);
    assert_eq!(value["d"], json!(1.5));
    let span = errors[0].span.unwrap();
    assert_eq!(&text[span.start..span.end], "99999999999999999999");
}

#[test]
fn utc_datetimes_are_instants_in_mdls_spelling() {
    let (value, errors, _) = converted(
        "a = 2000-01-01T00:00:00Z\nb = 2026-06-01T12:00:00.250Z\nc = 2026-06-01 12:00:00+00:00\n\
         d = 2026-06-01T12:00:00.000000001z\ne = 2026-06-01T12:00:00.000Z\n",
    );
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
        value,
        json!({
            "a": "2000-01-01T00:00:00Z",
            "b": "2026-06-01T12:00:00.25Z",
            "c": "2026-06-01T12:00:00Z",
            "d": "2026-06-01T12:00:00.000000001Z",
            "e": "2026-06-01T12:00:00Z"
        })
    );
}

#[test]
fn other_dates_and_times_are_errors() {
    let (_, errors, _) = converted(
        "a = 2026-06-01T12:00:00+02:00\nb = 2026-06-01T12:00:00\nc = 2026-06-01\nd = 12:00:00\n",
    );
    let codes: Vec<_> = errors.iter().map(|e| e.code).collect();
    assert_eq!(codes, ["toml.datetime"; 4]);
    assert!(errors[0].message.contains("isn't in UTC"));
    assert!(errors[1].message.contains("isn't an instant"));
}

#[test]
fn leap_seconds_and_year_zero_are_not_instants() {
    let (_, errors, _) =
        converted("a = 1998-12-31T23:59:60Z\nb = 0000-01-01T00:00:00Z\nc = 0001-01-01T00:00:00Z\n");
    let messages: Vec<_> = errors
        .iter()
        .map(|e| (e.code, e.message.as_str()))
        .collect();
    assert_eq!(
        messages,
        [
            (
                "toml.datetime",
                "1998-12-31T23:59:60Z is a leap second, and MDL instants have none"
            ),
            (
                "toml.datetime",
                "0000-01-01T00:00:00Z is in year 0; MDL instants start at year 0001"
            ),
        ]
    );
}

#[test]
fn every_code_is_listed_once_under_the_frontends_name() {
    let codes: std::collections::BTreeSet<_> = Code::ALL.iter().map(|c| c.as_str()).collect();
    assert_eq!(codes.len(), Code::ALL.len());
    assert!(codes.iter().all(|code| code.starts_with("toml.")));
}

#[test]
fn toml_keys_are_quoted_when_they_arent_bare() {
    assert_eq!(toml_key(&["catalog", "params"]), "catalog.params");
    assert_eq!(
        toml_key(&["catalog", "params", "eps.v"]),
        "catalog.params.\"eps.v\""
    );
    assert_eq!(toml_key(&["a b", "", "x-y_1"]), "\"a b\".\"\".x-y_1");
    assert_eq!(toml_key(&["q\"uote"]), "\"q\\\"uote\"");
}

/// JSON values TOML can spell: no nulls, integers JSON reads exactly, keys
/// and strings that need escaping in TOML and in JSON Pointers.
fn toml_value() -> impl Strategy<Value = Value> {
    let text = "[a-z/~.\"\\\\é😀 \n-]{0,5}";
    let leaf = prop_oneof![
        any::<bool>().prop_map(Value::Bool),
        (-(1i64 << 53) + 1..(1i64 << 53)).prop_map(Value::from),
        text.prop_map(Value::String),
    ];
    leaf.prop_recursive(4, 40, 5, move |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::btree_map(text, inner, 0..4)
                .prop_map(|members| Value::Object(members.into_iter().collect())),
        ]
    })
}

/// Every pointer in a value.
fn pointers(value: &Value, at: String, out: &mut Vec<String>) {
    out.push(at.clone());
    match value {
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                pointers(item, format!("{at}/{index}"), out);
            }
        }
        Value::Object(members) => {
            for (key, item) in members {
                pointers(item, format!("{at}/{}", escape_pointer_token(key)), out);
            }
        }
        _ => {}
    }
}

proptest! {
    #[test]
    fn toml_text_converts_to_the_value_it_spells(
        members in prop::collection::btree_map("[a-z/~. -]{1,5}", toml_value(), 0..5)
    ) {
        let value = Value::Object(members.into_iter().collect());
        let text = toml::to_string(&value);
        prop_assume!(text.is_ok());
        let text = text.unwrap();
        let (converted, errors, map) = converted(&text);
        prop_assert!(errors.is_empty());
        prop_assert_eq!(&converted, &value);
        let mut all = Vec::new();
        pointers(&value, String::new(), &mut all);
        for at in all {
            prop_assert!(map.get(&at).is_some(), "no span for {}", at);
        }
    }

    #[test]
    fn reading_any_text_never_panics(text in any::<String>(), second in any::<String>()) {
        let sources = sources(&[("a.toml", &text), ("b.toml", &second)]);
        let _ = read(sources.iter());
    }
}
