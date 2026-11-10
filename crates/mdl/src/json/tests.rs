use proptest::prelude::*;
use serde_json::Value;

use super::*;
use crate::source::{Location, Sources};
use crate::validate::validate;

const BUNDLE: &str = r#"{
  "mdl_version": "2.0",
  "catalog": {
    "params": {
      "eps.bus_voltage": { "type": "f64", "unit": "V" }
    }
  },
  "families": [
    {
      "name": "vendor_x",
      "encoding": "binary",
      "byte_order": "big",
      "alias": { "kind": "bits", "offset_bits": 0, "size_bits": 16 }
    }
  ],
  "sets": [
    {
      "name": "vendor_x_v1",
      "family": "vendor_x",
      "packets": [
        {
          "name": "HK",
          "length": { "kind": "exact", "bytes": 4 },
          "entries": [
            {
              "kind": "field",
              "name": "bus",
              "type": "uint",
              "offset_bits": 16,
              "size_bits": 16,
              "conversion": { "kind": "linear", "scale": 0.001, "offset": 0 },
              "unit": "V",
              "canonical": "eps.bus_voltage"
            }
          ]
        }
      ]
    }
  ]
}
"#;

/// Read `text` as the only file of a fresh `Sources`.
fn read_text(text: &str) -> Result<Parsed, Vec<SourceDiagnostic>> {
    let mut sources = Sources::new();
    let id = sources.add("bundle.json", text);
    read(sources.get(id).unwrap())
}

fn scan_text(text: &str) -> Result<SourceMap, Stop> {
    let mut sources = Sources::new();
    let id = sources.add("bundle.json", text);
    scan(sources.get(id).unwrap())
}

/// The one error reading `text` gives, and where it is.
fn error_at(text: &str) -> (SourceDiagnostic, Option<Location>) {
    let mut sources = Sources::new();
    let id = sources.add("bundle.json", text);
    let mut errors = read(sources.get(id).unwrap()).unwrap_err();
    assert_eq!(errors.len(), 1, "{errors:?}");
    let error = errors.remove(0);
    let location = error.span.and_then(|span| sources.location(span));
    (error, location)
}

/// The line and column of the first `needle` in `text`.
fn place_of(text: &str, needle: &str) -> Location {
    let mut sources = Sources::new();
    let id = sources.add("t", text);
    sources
        .get(id)
        .unwrap()
        .location(text.find(needle).unwrap())
}

#[test]
fn a_bundle_reads_with_a_span_for_every_value() {
    let parsed = read_text(BUNDLE).unwrap();
    assert_eq!(parsed.bundle.sets[0].packets[0].name, "HK");
    assert!(parsed.warnings.is_empty());
    let root = parsed.map.get("").unwrap();
    assert_eq!((root.start, root.end), (0, BUNDLE.trim_end().len()));
    let unit = parsed.map.get("/sets/0/packets/0/entries/0/unit").unwrap();
    assert_eq!(&BUNDLE[unit.start..unit.end], "\"V\"");
    assert!(validate(parsed.bundle).is_ok());
}

#[test]
fn a_syntax_error_points_where_parsing_stopped() {
    let text = BUNDLE.replace("\"bytes\": 4", "\"bytes\": x");
    let (error, location) = error_at(&text);
    assert_eq!(error.code, "invalid_json");
    assert!(error.message.starts_with("invalid JSON: expected value"));
    assert!(!error.message.contains("line"), "{}", error.message);
    assert_eq!(location, Some(place_of(&text, "x }")));
}

#[test]
fn text_that_ends_early_points_at_its_last_character() {
    let (error, location) = error_at("{\"mdl_version\": \"2.0\"");
    assert_eq!(error.code, "invalid_json");
    assert!(error.message.contains("EOF"), "{}", error.message);
    assert_eq!(
        location,
        Some(Location {
            line: 1,
            column: 21
        })
    );
}

#[test]
fn text_that_ends_after_a_line_break_points_at_its_end() {
    let text = "{\"mdl_version\": \"2.0\",\n";
    let (error, location) = error_at(text);
    assert_eq!(error.code, "invalid_json");
    assert_eq!(location, Some(Location { line: 2, column: 1 }));
    let span = error.span.unwrap();
    assert_eq!((span.start, span.end), (text.len(), text.len()));
}

#[test]
fn a_byte_order_mark_is_refused_with_a_reason() {
    let (error, location) = error_at(&format!("\u{feff}{BUNDLE}"));
    assert_eq!(error.code, "invalid_json");
    assert!(
        error.message.contains("byte order mark"),
        "{}",
        error.message
    );
    assert_eq!(location, Some(Location { line: 1, column: 1 }));
}

#[test]
fn a_duplicate_key_points_at_the_second_and_names_the_first() {
    let text = BUNDLE.replace(
        "\"name\": \"HK\",",
        "\"name\": \"HK\",\n          \"name\": \"HK2\",",
    );
    let (error, location) = error_at(&text);
    assert_eq!(error.code, "duplicate_key");
    assert_eq!(error.path, "/sets/0/packets/0/name");
    assert_eq!(error.message, "duplicate key \"name\" in one object");
    let mut sources = Sources::new();
    let id = sources.add("bundle.json", text.as_str());
    let first = error.first.map(|span| Span::new(id, span.start, span.end));
    assert_eq!(
        first.and_then(|span| sources.location(span)),
        Some(place_of(&text, "\"name\": \"HK\""))
    );
    assert_eq!(location, Some(place_of(&text, "\"name\": \"HK2\"")));
}

#[test]
fn an_integer_without_an_exact_double_points_at_the_number() {
    let text = BUNDLE.replace("\"offset_bits\": 16", "\"offset_bits\": 9007199254740993");
    let (error, location) = error_at(&text);
    assert_eq!(error.code, "inexact_integer");
    assert_eq!(error.path, "/sets/0/packets/0/entries/0/offset_bits");
    assert!(!error.message.contains("/sets"), "{}", error.message);
    assert_eq!(location, Some(place_of(&text, "9007199254740993")));
}

#[test]
fn a_shape_error_points_at_what_it_is_about() {
    let text = BUNDLE.replace(
        "\"name\": \"HK\",",
        "\"name\": \"HK\", \"colour\": \"red\",",
    );
    let (error, location) = error_at(&text);
    assert_eq!(error.code, "wrong_shape");
    assert!(
        error.message.contains("unknown field `colour`"),
        "{}",
        error.message
    );
    assert_eq!(
        location.map(|l| l.line),
        Some(place_of(&text, "colour").line)
    );
}

#[test]
fn a_version_error_points_at_mdl_version() {
    let text = BUNDLE.replace("\"2.0\"", "\"3.0\"");
    let (error, location) = error_at(&text);
    assert_eq!(error.code, "unsupported_version");
    assert_eq!(error.path, "/mdl_version");
    assert_eq!(location, Some(place_of(&text, "\"3.0\"")));
}

#[test]
fn validation_diagnostics_point_into_the_file() {
    let text = BUNDLE.replace("\"unit\": \"V\",\n", "\"unit\": \"A\",\n");
    let mut sources = Sources::new();
    let id = sources.add("bundle.json", text.as_str());
    let parsed = read(sources.get(id).unwrap()).unwrap();
    let diagnostics = validate(parsed.bundle).unwrap_err();
    let located = parsed.map.locate_all(&diagnostics);
    let mismatch = located.iter().find(|d| d.code == "unit_mismatch").unwrap();
    let span = mismatch.span.unwrap();
    assert_eq!(&text[span.start..span.end], "\"A\"");
    let rendered = mismatch.display(&sources).to_string();
    let place = place_of(&text, "\"A\"");
    assert!(
        rendered.contains(&format!("--> bundle.json:{}:{}", place.line, place.column)),
        "{rendered}"
    );
}

#[test]
fn the_scan_finds_the_first_duplicate_the_parser_refuses() {
    for text in [
        r#"{"a":1,"a":2}"#,
        r#"[{"x":{"k":1,"b":[],"k":1}}]"#,
        r#"{"ab":1,"ab":2}"#,
    ] {
        assert!(matches!(
            canonical::parse_strict(text.as_bytes()),
            Err(CanonicalError::DuplicateKey { .. })
        ));
        let Err(Stop::Duplicate(duplicate)) = scan_text(text) else {
            panic!("{text}: no duplicate found");
        };
        assert!(duplicate.first.start < duplicate.second.start, "{text}");
    }
}

/// Every JSON value, nested a few levels, with keys and strings that need
/// escaping in JSON and in JSON Pointers.
fn json_value() -> impl Strategy<Value = Value> {
    let text = "[a-z/~\"\\\\é😀 \n]{0,5}";
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::from),
        text.prop_map(Value::String),
    ];
    leaf.prop_recursive(4, 40, 5, move |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..5).prop_map(Value::Array),
            prop::collection::btree_map(text, inner, 0..5)
                .prop_map(|members| Value::Object(members.into_iter().collect())),
        ]
    })
}

/// Every pointer in a value, with the value there.
fn pointers(value: &Value, pointer: String, out: &mut Vec<(String, Value)>) {
    out.push((pointer.clone(), value.clone()));
    match value {
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                pointers(item, format!("{pointer}/{index}"), out);
            }
        }
        Value::Object(members) => {
            for (key, item) in members {
                pointers(
                    item,
                    format!("{pointer}/{}", escape_pointer_token(key)),
                    out,
                );
            }
        }
        _ => {}
    }
}

proptest! {
    #[test]
    fn every_value_maps_to_the_text_that_spells_it(value in json_value(), pretty in any::<bool>()) {
        let text = if pretty {
            serde_json::to_string_pretty(&value).unwrap()
        } else {
            serde_json::to_string(&value).unwrap()
        };
        let map = scan_text(&text).unwrap();
        let mut all = Vec::new();
        pointers(&value, String::new(), &mut all);
        prop_assert_eq!(map.len(), all.len());
        for (pointer, expected) in all {
            let span = map.get(&pointer).unwrap();
            let spelled: Value = serde_json::from_str(&text[span.start..span.end]).unwrap();
            prop_assert_eq!(spelled, expected, "{}", pointer);
        }
    }

    #[test]
    fn reading_any_text_never_panics(text in any::<String>()) {
        let _ = read_text(&text);
    }

    #[test]
    fn reading_broken_bundles_never_panics(cut in 0..BUNDLE.len(), insert in any::<char>()) {
        let mut text: String = BUNDLE.chars().take(cut).collect();
        text.push(insert);
        text.extend(BUNDLE.chars().skip(cut));
        let _ = read_text(&text);
        let _ = read_text(&BUNDLE[..cut]);
    }
}
