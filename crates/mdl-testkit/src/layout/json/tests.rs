//! The JSON oracle against itself: its documents read back as it meant
//! them, under serde_json's reading of RFC 8259 and §2.3's integer rule,
//! and its layouts and faults reach everything they're for.

use std::collections::{BTreeMap, BTreeSet};

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::TestRunner;
use serde_json::{Number, Value};

use super::doc::{self, Doc, Spell};
use super::{BOTCHED, json_damaged, json_telemetry};
use crate::coverage::features;
use crate::layout::{
    EventTime, Family, FaultKind, Gone, JsonKind, Problem, Raw, Route, Unfit, Wanted,
};

/// Any document: scalars of every kind, nested a few levels.
fn document() -> impl Strategy<Value = Doc> {
    let leaf = prop_oneof![
        Just(Doc::Null),
        any::<bool>().prop_map(Doc::Bool),
        any::<i64>().prop_map(|n| Doc::Int(i128::from(n))),
        any::<u64>().prop_map(|n| Doc::Int(i128::from(n))),
        (-1_i128 << 53..=1_i128 << 53).prop_map(Doc::Int),
        (-3_i128..=3).prop_map(Doc::Int),
        any::<f64>()
            .prop_filter("JSON numbers are finite", |x| x.is_finite())
            .prop_map(Doc::Float),
        Just(Doc::Float(-0.0)),
        any::<String>().prop_map(Doc::Str),
    ];
    leaf.prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            vec(inner.clone(), 0..4).prop_map(Doc::Array),
            vec((any::<String>(), inner), 0..4).prop_map(|mut members| {
                // Member names are unique (§2.3).
                let mut seen = std::collections::BTreeSet::new();
                members.retain(|(name, _)| seen.insert(name.clone()));
                Doc::Object(members)
            }),
        ]
    })
}

proptest! {
    /// Whatever spellings the writer picks, the text reads back as the
    /// document: an integer as the same integer by §2.3's rule, a float as
    /// the same double, bit for bit.
    #[test]
    fn documents_read_back_as_written(document in document(), seed in any::<u64>()) {
        let text = doc::write(&document, &mut Spell::new(seed));
        let read: Value = serde_json::from_str(&text)
            .map_err(|error| TestCaseError::fail(format!("{error}: {text}")))?;
        prop_assert!(same(&document, &read), "{}", text);
    }
}

fn same(document: &Doc, value: &Value) -> bool {
    match (document, value) {
        (Doc::Null, Value::Null) => true,
        (Doc::Bool(a), Value::Bool(b)) => a == b,
        (Doc::Int(n), Value::Number(number)) => integer(number) == Some(*n),
        (Doc::Float(x), Value::Number(number)) => {
            number.as_f64().is_some_and(|y| y.to_bits() == x.to_bits())
        }
        (Doc::Str(a), Value::String(b)) => a == b,
        (Doc::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same(x, y))
        }
        (Doc::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(name, x)| b.get(name).is_some_and(|y| same(x, y)))
        }
        _ => false,
    }
}

/// A number's integer value by §2.3: an integer literal within 64 bits,
/// or a number with no fractional part within ±(2^53 − 1).
fn integer(number: &Number) -> Option<i128> {
    if let Some(n) = number.as_u64() {
        return Some(i128::from(n));
    }
    if let Some(n) = number.as_i64() {
        return Some(i128::from(n));
    }
    let x = number.as_f64()?;
    (x.fract() == 0.0 && x.abs() <= 9_007_199_254_740_991.0).then_some(x as i128)
}

/// The numbers and strings of a JSON text, as written.
fn lexemes(text: &str) -> (Vec<&str>, Vec<&str>) {
    let bytes = text.as_bytes();
    let (mut numbers, mut strings) = (Vec::new(), Vec::new());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let start = i;
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(bytes.len());
                strings.push(&text[start..i]);
            }
            b'-' | b'0'..=b'9' => {
                let start = i;
                while i < bytes.len()
                    && matches!(bytes[i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                {
                    i += 1;
                }
                numbers.push(&text[start..i]);
            }
            _ => i += 1,
        }
    }
    (numbers, strings)
}

/// The escapes a string as written uses: each escaped character, `u` for
/// a `\\u` escape.
fn escapes(lexeme: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut chars = lexeme.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            continue;
        }
        match chars.next() {
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                found.push(format!("u{}", hex.to_ascii_lowercase()));
            }
            Some(escaped) => found.push(escaped.to_string()),
            None => {}
        }
    }
    found
}

/// What a JSON case shows of the generator's reach: its bundle's features,
/// its pointers and keys, and each message's spellings, absent entries,
/// `null` elements and event time.
fn labels(case: &crate::layout::Telemetry) -> BTreeSet<String> {
    let mut found = features(&case.bundle);
    let set = &case.bundle["sets"][0];
    let packets = set["packets"].as_array().cloned().unwrap_or_default();
    let iso8601 = set["time"]["kind"] == "iso8601";
    let mut pointers = Vec::new();
    for packet in &packets {
        match packet["identify"].as_object() {
            None => {
                found.insert("identify:catch_all".to_string());
            }
            Some(keys) => {
                if keys.len() > 1 {
                    found.insert("identify:nested".to_string());
                }
                for value in keys.values() {
                    found.insert(
                        if value.is_string() {
                            "identify:string"
                        } else {
                            "identify:integer"
                        }
                        .to_string(),
                    );
                }
            }
        }
        for entry in packet["entries"].as_array().into_iter().flatten() {
            if entry["name"] == "kind" {
                found.insert("identify:kind_field".to_string());
            }
            pointers.push(entry["pointer"].as_str().unwrap_or("").to_string());
        }
    }
    for ty in set["types"]
        .as_object()
        .into_iter()
        .flat_map(|types| types.values())
    {
        for entry in ty["entries"].as_array().into_iter().flatten() {
            pointers.push(entry["pointer"].as_str().unwrap_or("").to_string());
        }
    }
    for pointer in &pointers {
        if pointer.contains("~01") {
            found.insert("pointer:looks_escaped".to_string());
        } else if pointer.contains("~1") {
            found.insert("pointer:escaped_slash".to_string());
        }
        if pointer.contains("~0") && !pointer.contains("~01") {
            found.insert("pointer:escaped_tilde".to_string());
        }
        if pointer.matches('/').count() > 1 {
            found.insert("pointer:nested".to_string());
        }
        if !pointer.is_ascii() {
            found.insert("pointer:non_ascii".to_string());
        }
    }
    if case.native_id == "17" {
        found.insert("alias:number".to_string());
    }
    for message in &case.messages {
        let Ok(text) = std::str::from_utf8(&message.payload) else {
            continue;
        };
        let Ok(document) = serde_json::from_str::<Value>(text) else {
            continue;
        };
        let (numbers, strings) = lexemes(text);
        for number in numbers {
            let digits = number.trim_start_matches('-');
            if number.contains(['e', 'E']) {
                found.insert("number:exponent".to_string());
            } else if let Some((_, fraction)) = number.split_once('.') {
                if fraction.chars().all(|c| c == '0') {
                    found.insert("number:fraction_zeros".to_string());
                }
            } else if let Ok(n) = digits.parse::<u128>() {
                if n > 1 << 53 {
                    found.insert("number:past_safe_literal".to_string());
                }
                if n > i64::MAX as u128 && !number.starts_with('-') {
                    found.insert("number:past_i64_literal".to_string());
                }
            }
            if number.starts_with("-0") && !number.chars().any(|c| ('1'..='9').contains(&c)) {
                found.insert("number:negative_zero".to_string());
            }
        }
        for string in strings {
            for escape in escapes(string) {
                let label = match escape.as_str() {
                    "/" => "string:escaped_solidus",
                    "n" | "t" | "b" | "f" | "r" | "\"" | "\\" => "string:short_escape",
                    u if u.starts_with("ud8")
                        || u.starts_with("ud9")
                        || u.starts_with("uda")
                        || u.starts_with("udb") =>
                    {
                        "string:surrogate_pair"
                    }
                    _ => "string:unicode_escape",
                };
                found.insert(label.to_string());
            }
            if !string.is_ascii() {
                found.insert("string:raw_non_ascii".to_string());
            }
        }
        for (space, label) in [
            ('\n', "space:newline"),
            ('\t', "space:tab"),
            ('\r', "space:return"),
        ] {
            if text.contains(space) {
                found.insert(label.to_string());
            }
        }
        if document
            .as_object()
            .is_some_and(|members| members.keys().any(|name| name.starts_with('x')))
        {
            found.insert("member:extra".to_string());
        }
        let Some(packet) = packets
            .iter()
            .find(|packet| packet["name"] == message.packet)
        else {
            continue;
        };
        let keys = packet["identify"].as_object().map_or(0, |keys| keys.len());
        if keys == 2 {
            found.insert("identify:nested_match".to_string());
        }
        if keys == 1 && document.get("sub").is_some() {
            found.insert("identify:stray_sub".to_string());
        }
        let recorded: BTreeMap<&str, &Raw> = message
            .record
            .iter()
            .map(|(name, raw)| (name.as_str(), raw))
            .collect();
        for entry in packet["entries"].as_array().into_iter().flatten() {
            let name = entry["name"].as_str().unwrap_or("");
            let pointer = entry["pointer"].as_str().unwrap_or("");
            match recorded.get(name) {
                None if entry["optional"] == true => {
                    found.insert(
                        match document.pointer(pointer) {
                            None => "optional:missing",
                            Some(_) => "optional:null",
                        }
                        .to_string(),
                    );
                }
                None if entry.get("when").is_some() => {
                    found.insert("when:skipped".to_string());
                    if document.pointer(pointer).is_some() {
                        found.insert("when:skipped_with_value".to_string());
                    }
                }
                Some(Raw::Array(items)) if items.contains(&Raw::Null) => {
                    found.insert(
                        if entry["element"]["kind"] == "struct" {
                            "element:null_record"
                        } else {
                            "element:null_scalar"
                        }
                        .to_string(),
                    );
                }
                _ => {}
            }
        }
        found.insert(
            match message.event_time {
                EventTime::At(_) => "time:at",
                EventTime::Invalid => "time:invalid",
                EventTime::Ground => "time:ground",
                EventTime::SinceBoot { .. } => "time:since_boot",
            }
            .to_string(),
        );
        if !iso8601 {
            continue;
        }
        match document.get("t") {
            None => {
                found.insert("iso8601:missing".to_string());
            }
            Some(Value::String(stamp)) if BOTCHED.contains(&stamp.as_str()) => {
                found.insert("iso8601:botched".to_string());
            }
            Some(Value::String(stamp)) if message.event_time == EventTime::Invalid => {
                found.insert("iso8601:out_of_range".to_string());
                if stamp.starts_with("0000") {
                    found.insert("iso8601:year_0000_refused".to_string());
                }
            }
            Some(Value::String(stamp)) => {
                let bytes = stamp.as_bytes();
                if let Some(separator) = bytes.get(10) {
                    found.insert(
                        format!("iso8601:separator_{}", *separator as char).replace(' ', "space"),
                    );
                }
                if stamp.get(17..19) == Some("60") {
                    found.insert("iso8601:second_60".to_string());
                }
                if stamp.starts_with("0000") {
                    found.insert("iso8601:year_0000".to_string());
                }
                let zone = if stamp.ends_with('Z') {
                    "zone_z_upper"
                } else if stamp.ends_with('z') {
                    "zone_z_lower"
                } else if stamp.ends_with("+00:00") {
                    "zone_plus_zero"
                } else if stamp.len() > 6 && stamp.as_bytes()[stamp.len() - 6] == b'-' {
                    "zone_negative"
                } else {
                    "zone_positive"
                };
                found.insert(format!("iso8601:{zone}"));
                let digits = stamp.split_once('.').map_or(0, |(_, rest)| {
                    rest.chars().take_while(char::is_ascii_digit).count()
                });
                found.insert(
                    match digits {
                        0 => "iso8601:no_fraction",
                        1..=9 => "iso8601:fraction",
                        _ => "iso8601:fraction_past_nanoseconds",
                    }
                    .to_string(),
                );
            }
            Some(_) => {
                found.insert("iso8601:not_a_string".to_string());
            }
        }
    }
    found
}

/// Generated JSON layouts reach every feature the JSON oracle is for:
/// each entry kind, field type, pointer form, key, alias and time source,
/// absent entries both ways, `null` elements, and each spelling of
/// numbers, strings, whitespace and RFC 3339 instants.
#[test]
fn json_layouts_reach_every_feature() {
    let mut runner = TestRunner::deterministic();
    let strategy = json_telemetry();
    let mut found = BTreeSet::new();
    for _ in 0..500 {
        let case = strategy.new_tree(&mut runner).unwrap().current();
        found.extend(labels(&case));
    }
    for feature in [
        "encoding:json",
        "locator:fixed",
        "locator:path",
        "alias:number",
        "time:iso8601",
        "time:elapsed",
        "time:gps",
        "key:packet.identify",
        "key:packet.time_field",
        "key:entry.pointer",
        "key:entry.optional",
        "key:entry.when",
        "key:entry.invalid",
        "key:entry.sample_interval_ms",
        "entry:field",
        "entry:array",
        "entry:struct",
        "element:scalar",
        "element:struct",
        "field_type:uint",
        "field_type:int",
        "field_type:float",
        "field_type:bool",
        "field_type:string",
        "conversion:states",
        "conversion:linear",
        "conversion:table",
        "condition:compare",
        "condition:in",
        "condition:not",
        "identify:string",
        "identify:integer",
        "identify:nested",
        "identify:nested_match",
        "identify:catch_all",
        "identify:stray_sub",
        "identify:kind_field",
        "pointer:escaped_slash",
        "pointer:escaped_tilde",
        "pointer:looks_escaped",
        "pointer:nested",
        "pointer:non_ascii",
        "number:exponent",
        "number:fraction_zeros",
        "number:negative_zero",
        "number:past_safe_literal",
        "number:past_i64_literal",
        "string:short_escape",
        "string:escaped_solidus",
        "string:unicode_escape",
        "string:surrogate_pair",
        "string:raw_non_ascii",
        "space:newline",
        "space:tab",
        "space:return",
        "member:extra",
        "optional:missing",
        "optional:null",
        "when:skipped",
        "when:skipped_with_value",
        "element:null_scalar",
        "element:null_record",
        "time:at",
        "time:invalid",
        "time:ground",
        "iso8601:missing",
        "iso8601:botched",
        "iso8601:out_of_range",
        "iso8601:not_a_string",
        "iso8601:separator_T",
        "iso8601:separator_t",
        "iso8601:separator_space",
        "iso8601:second_60",
        "iso8601:year_0000",
        "iso8601:zone_z_upper",
        "iso8601:zone_z_lower",
        "iso8601:zone_plus_zero",
        "iso8601:zone_negative",
        "iso8601:zone_positive",
        "iso8601:no_fraction",
        "iso8601:fraction",
        "iso8601:fraction_past_nanoseconds",
    ] {
        assert!(found.contains(feature), "no JSON layout reaches {feature}");
    }
}

/// What the JSON damaged-message reach test counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Reached {
    Kind(FaultKind),
    Route(Route),
    Found(JsonKind),
    Code(&'static str),
    Named(Named),
    /// Two faults planted in one message.
    Pair,
    /// Of two faults, the error is the one picked second: the one decoding
    /// reads first.
    SecondFirst,
}

/// Where the entry an error names is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Named {
    AtTop,
    Element,
    InRecord,
    InStruct,
}

/// Damaged JSON messages reach every route their faults take, every kind
/// of value `wrong_type` can find, every place an error can name, and
/// pairs, at least five times each.
#[test]
fn json_damaged_messages_reach_every_route() {
    let mut runner = TestRunner::deterministic();
    let strategy = json_damaged();
    let mut found: BTreeMap<Reached, usize> = BTreeMap::new();
    let mut note = |reached: Reached| *found.entry(reached).or_default() += 1;
    let (mut messages, mut damaged) = (0, 0);
    for _ in 0..600 {
        let case = strategy.new_tree(&mut runner).unwrap().current();
        for message in &case.messages {
            messages += 1;
            let Some(damage) = &message.damage else {
                continue;
            };
            damaged += 1;
            for route in &damage.routes {
                note(Reached::Kind(route.kind()));
                note(Reached::Route(*route));
            }
            let failure = &damage.failure;
            note(Reached::Code(failure.problem.code()));
            if let Problem::WrongType { found, .. } = failure.problem {
                note(Reached::Found(found));
            }
            if let [first, second] = damage.routes.as_slice() {
                note(Reached::Pair);
                let code_of = |route: &Route| match route.kind() {
                    FaultKind::Missing => "missing_value",
                    FaultKind::WrongType => "wrong_type",
                    _ => "out_of_range",
                };
                if code_of(second) == failure.problem.code() && code_of(first) != code_of(second) {
                    note(Reached::SecondFirst);
                }
            }
            let path = failure.path.as_str();
            note(Reached::Named(if path.contains("].") {
                Named::InRecord
            } else if path.ends_with(']') {
                Named::Element
            } else if path.contains('.') {
                Named::InStruct
            } else {
                Named::AtTop
            }));
        }
    }
    println!("{damaged} of {messages} messages damaged; {found:#?}");
    assert!(
        damaged * 10 >= messages * 9,
        "{damaged} of {messages} damaged"
    );
    let mut required: Vec<Reached> = FaultKind::ALL
        .into_iter()
        .filter(|kind| kind.family() == Family::Json)
        .map(Reached::Kind)
        .collect();
    required.extend(Gone::ALL.map(|gone| Reached::Route(Route::Missing(gone))));
    required.extend(Wanted::ALL.map(|wanted| Reached::Route(Route::WrongType(wanted))));
    required.extend(Unfit::ALL.map(|unfit| Reached::Route(Route::OutOfRange(unfit))));
    required.extend(
        JsonKind::ALL
            .into_iter()
            .filter(|kind| *kind != JsonKind::Null)
            .map(Reached::Found),
    );
    required.extend(["missing_value", "wrong_type", "out_of_range"].map(Reached::Code));
    required.extend(
        [
            Named::AtTop,
            Named::Element,
            Named::InRecord,
            Named::InStruct,
        ]
        .map(Reached::Named),
    );
    required.extend([Reached::Pair, Reached::SecondFirst]);
    for reached in required {
        let count = found.get(&reached).copied().unwrap_or(0);
        assert!(count >= 5, "{reached:?} is reached {count} times");
    }
}
