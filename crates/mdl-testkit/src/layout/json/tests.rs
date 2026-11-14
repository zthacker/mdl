//! The JSON oracle against itself: its documents read back as it meant
//! them, under serde_json's reading of RFC 8259 and §2.3's integer rule,
//! and its layouts and faults reach everything they're for.

use std::collections::{BTreeMap, BTreeSet};

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::TestRunner;
use serde_json::{Map, Number, Value};

use super::doc::{self, Doc, Spell};
use super::{
    Alias, BOTCHED, JsonTime, TimeFields, json_damaged, json_telemetry, lay_out, packet_spec,
};
use crate::coverage::features;
use crate::layout::{
    EventTime, Expected, Family, FaultKind, Gone, JsonKind, Malformed, Meta, Problem, Raw, Route,
    Telemetry, Unfit, Unmet, Unusable, Wanted,
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
/// or a number whose nearest double has no fractional part within
/// ±(2^53 − 1).
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
fn labels(case: &Telemetry) -> BTreeSet<String> {
    let mut found = features(&case.bundle);
    let set = &case.bundle["sets"][0];
    let packets = set["packets"].as_array().cloned().unwrap_or_default();
    let iso8601 = set["time"]["kind"] == "iso8601";
    let boot = set["time"]["kind"] == "boot";
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
                for value in keys.values().filter_map(Value::as_i64) {
                    if value < 0 {
                        found.insert("identify:negative_integer".to_string());
                    } else if value > 1 << 32 {
                        found.insert("identify:wide_integer".to_string());
                    }
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
    // Pointers through a container of an entry's own (RFC 6901).
    for pointer in &pointers {
        let tokens: Vec<&str> = pointer.split('/').skip(1).collect();
        if let Some(at) = tokens.iter().position(|token| token.ends_with("_at")) {
            let label = match tokens.get(at + 1).copied().unwrap_or("") {
                "-" => "pointer:element_dash",
                "" => "pointer:element_empty",
                token if token.starts_with('+') => "pointer:element_plus",
                token if token.contains('.') => "pointer:element_decimal",
                token if token.len() > 1 && token.starts_with('0') => {
                    "pointer:element_leading_zero"
                }
                token if token.len() > 1 => "pointer:element_two_digits",
                _ => "pointer:element_index",
            };
            found.insert(label.to_string());
            if tokens.len() > at + 2 {
                found.insert("pointer:through_element".to_string());
            }
        }
    }
    // String states and sentinels beyond letters, compared exactly.
    let letters = |text: &str| text.chars().all(|c| c.is_ascii_alphabetic());
    for param in case.bundle["catalog"]["params"]
        .as_object()
        .into_iter()
        .flat_map(|params| params.values())
    {
        let states: Vec<&str> = param["states"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if states.iter().any(|state| !letters(state)) {
            found.insert("state:beyond_letters".to_string());
        }
    }
    let mut entries: Vec<&Value> = Vec::new();
    for packet in &packets {
        entries.extend(packet["entries"].as_array().into_iter().flatten());
    }
    for ty in set["types"]
        .as_object()
        .into_iter()
        .flat_map(|types| types.values())
    {
        entries.extend(ty["entries"].as_array().into_iter().flatten());
    }
    for entry in entries {
        for sentinel in entry["invalid"].as_array().into_iter().flatten() {
            match sentinel.as_str() {
                Some("") => {
                    found.insert("sentinel:empty_text".to_string());
                }
                Some(text) if !letters(text) => {
                    found.insert("sentinel:beyond_letters".to_string());
                }
                _ => {}
            }
        }
    }
    let by_metadata = case.bundle["families"][0]["metadata"]
        .as_array()
        .is_some_and(|names| names.iter().any(|name| name == "kind"));
    if by_metadata {
        found.insert("identify:by_metadata".to_string());
        if packets.iter().any(|packet| {
            packet["identify"].get("kind").is_some() && packet["identify"].get("/sub").is_some()
        }) {
            found.insert("identify:metadata_and_pointer".to_string());
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
                if digits == "1e-400" {
                    found.insert("number:below_smallest".to_string());
                }
            } else if let Some((_, fraction)) = number.split_once('.') {
                if fraction.chars().all(|c| c == '0') {
                    found.insert("number:fraction_zeros".to_string());
                } else if fraction.len() >= 3
                    && fraction.ends_with('1')
                    && fraction[..fraction.len() - 1].chars().all(|c| c == '0')
                {
                    found.insert("number:past_double_digits".to_string());
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
        if case.native_id == "17" && document.get("sat").is_some_and(Value::is_string) {
            found.insert("alias:number_as_text".to_string());
        }
        if depth(&document) == 127 {
            found.insert("edge:depth_127".to_string());
        }
        for (member, label) in [
            ("alike", "edge:alike_names"),
            ("tiny", "edge:tiny_number"),
            ("", "edge:empty_name"),
            ("zero", "edge:zero_huge_exponent"),
        ] {
            if document.get(member).is_some() {
                found.insert(label.to_string());
            }
        }
        if document
            .as_object()
            .is_some_and(|members| members.keys().any(|name| name.contains('\u{fdd0}')))
        {
            found.insert("edge:noncharacters".to_string());
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
        match document.get("sub") {
            Some(sub) if keys == 1 && (sub.is_u64() || sub.is_i64()) => {
                found.insert("identify:stray_sub".to_string());
            }
            Some(_) if keys == 1 => {
                found.insert("identify:stray_sub_unreadable".to_string());
            }
            _ => {}
        }
        if by_metadata {
            let kinds: Vec<&Meta> = message
                .metadata
                .iter()
                .filter(|(name, _)| name == "kind")
                .map(|(_, value)| value)
                .collect();
            let label = match kinds.as_slice() {
                [] => "metadata:none",
                [_, _, ..] => "metadata:twice",
                // A numbered kind's text with a leading zero, after any sign.
                [Meta::Text(text)]
                    if keys == 0 && text.trim_start_matches('-').starts_with('0') =>
                {
                    "metadata:leading_zero"
                }
                [Meta::Text(text)] if text.parse::<i64>().is_ok() => "metadata:integer_as_text",
                [Meta::Text(_)] => "metadata:text",
                [_] => "metadata:integer",
            };
            found.insert(label.to_string());
            if document.get("kind").is_some() {
                found.insert("metadata:decoy_member".to_string());
            }
        }
        for sample in &message.samples {
            match &sample.value {
                Expected::Enum(state) if !letters(state) => {
                    found.insert("state:read_beyond_letters".to_string());
                }
                // A state's name with `é` the other way round: no state.
                Expected::Str(text)
                    if sample.quality == "unknown_state"
                        && (text.contains('\u{e9}') || text.contains('\u{301}')) =>
                {
                    found.insert("state:twin_unknown".to_string());
                }
                _ => {}
            }
        }
        // A catch-all's `/kind`, where a member is its key.
        if keys == 0 && !by_metadata {
            match document.get("kind") {
                None => {
                    found.insert("identify:catch_all_no_kind".to_string());
                }
                Some(Value::String(text)) if text.parse::<i64>().is_ok() => {
                    found.insert("identify:catch_all_kind_as_text".to_string());
                }
                Some(Value::String(_)) => {}
                Some(_) => {
                    found.insert("identify:catch_all_kind_unreadable".to_string());
                }
            }
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
                    // Absent through an array: one ending before the index,
                    // or one a token that isn't an index finds nothing in.
                    let tokens: Vec<&str> = pointer.split('/').skip(1).collect();
                    let index = |token: &str| {
                        let digits = !token.is_empty() && token.bytes().all(|b| b.is_ascii_digit());
                        let leading_zero = token.len() > 1 && token.starts_with('0');
                        (digits && !leading_zero)
                            .then(|| token.parse::<usize>().ok())
                            .flatten()
                    };
                    if let [outer, token, ..] = tokens.as_slice()
                        && outer.ends_with("_at")
                        && let Some(Value::Array(items)) = document.get(*outer)
                        && document.pointer(pointer).is_none()
                    {
                        match index(token) {
                            Some(at) if items.len() <= at => {
                                found.insert("absent:array_ends_before".to_string());
                            }
                            Some(_) => {}
                            None => {
                                found.insert("absent:array_and_no_index".to_string());
                            }
                        }
                    }
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
            // Read through a container of its own: an array, or an object.
            let tokens: Vec<&str> = pointer.split('/').skip(1).collect();
            if recorded.contains_key(name)
                && let [outer, ..] = tokens.as_slice()
                && outer.ends_with("_at")
            {
                match document.get(*outer) {
                    Some(Value::Array(_)) => {
                        found.insert("element:in_an_array".to_string());
                    }
                    Some(Value::Object(_)) => {
                        found.insert("element:as_a_member".to_string());
                    }
                    _ => {}
                }
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
        if boot {
            boot_labels(packet, message, &mut found);
        }
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
    composite_labels(case, &mut found);
    found
}

/// What a message of a boot set shows (§8, §8.2): a count since boot past
/// 32 bits, or a sentinel there, which gives ground time; a correlation
/// packet's clocks past 32 bits, or holding a sentinel.
fn boot_labels(packet: &Value, message: &crate::layout::Message, found: &mut BTreeSet<String>) {
    let reading = |name: &str| {
        message.record.iter().find_map(|(field, raw)| match raw {
            Raw::Uint(n) if field == name => Some(*n),
            _ => None,
        })
    };
    let wide = |n: u64| n > u64::from(u32::MAX);
    if let Some(t) = reading("t") {
        if wide(t) {
            found.insert("boot:wide_count".to_string());
        }
        if message.event_time == EventTime::Ground {
            found.insert("boot:count_sentinel".to_string());
        }
    }
    if packet["time_field"].is_null() && message.packet == "CORR" {
        found.insert("boot:correlation".to_string());
        for name in ["clock", "boot"] {
            let invalid: Vec<u64> = packet["entries"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|entry| entry["name"] == name)
                .flat_map(|entry| entry["invalid"].as_array().cloned().unwrap_or_default())
                .filter_map(|sentinel| sentinel.as_u64())
                .collect();
            if let Some(n) = reading(name) {
                if wide(n) {
                    found.insert(format!("boot:wide_{name}"));
                }
                if invalid.contains(&n) {
                    found.insert(format!("boot:{name}_sentinel"));
                }
            }
        }
    }
}

/// What a case's composites show (§7.4): what holds their members, and for
/// each message, a sample, one that isn't good, or why there's none;
/// `composite:unexplained` where a sample disagrees with its members'
/// presence, or [`absence`] can't follow a member.
fn composite_labels(case: &Telemetry, found: &mut BTreeSet<String>) {
    let set = &case.bundle["sets"][0];
    let empty = Map::new();
    let types = set["types"].as_object().unwrap_or(&empty);
    for packet in set["packets"].as_array().into_iter().flatten() {
        let entries = packet["entries"].as_array().map_or(&[][..], Vec::as_slice);
        let composites = packet["composites"]
            .as_array()
            .map_or(&[][..], Vec::as_slice);
        let members_of = |composite: &Value| -> Vec<String> {
            composite["members"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|member| member.as_str().map(str::to_string))
                .collect()
        };
        for composite in composites {
            let holder = match members_of(composite).first() {
                Some(path) if path.contains("].") => "records",
                Some(path) if path.contains('[') => "array",
                Some(path) if path.contains('.') => "struct",
                _ => "fields",
            };
            found.insert(format!("composite:{holder}"));
        }
        let messages = case
            .messages
            .iter()
            .filter(|message| packet["name"] == message.packet.as_str());
        for message in messages {
            for composite in composites {
                let canonical = composite["canonical"].as_str().unwrap_or("");
                let sample = message
                    .samples
                    .iter()
                    .find(|sample| sample.param == canonical);
                let absent: Result<Vec<&str>, String> = members_of(composite)
                    .iter()
                    .filter_map(|member| {
                        absence(&message.record, member, entries, types).transpose()
                    })
                    .collect();
                match (sample, absent) {
                    (_, Err(lost)) => {
                        found.insert(format!("composite:unexplained: {lost}"));
                    }
                    (Some(sample), Ok(absent)) if absent.is_empty() => {
                        let label = if sample.quality == "good" {
                            "composite:sample"
                        } else {
                            "composite:not_good"
                        };
                        found.insert(label.to_string());
                    }
                    (Some(_), Ok(_)) => {
                        found.insert(format!(
                            "composite:unexplained: {canonical} has a sample, a member absent"
                        ));
                    }
                    (None, Ok(absent)) if absent.is_empty() => {
                        found.insert(format!(
                            "composite:unexplained: {canonical} has no sample, every member there"
                        ));
                    }
                    (None, Ok(absent)) => {
                        found.extend(
                            absent
                                .into_iter()
                                .map(|why| format!("composite:absent_{why}")),
                        );
                    }
                }
            }
        }
    }
}

/// Whether the member at record path `path` is in `record` (§7.4):
/// `Ok(None)` if it is; if it isn't, why: its own entry or one holding it
/// absent (optional, or skipped by its condition), its index past its
/// array's end, or it or its record a `null` element (§5.3). `Err` names
/// what the walk couldn't follow. `entries` are the packet's, `types` its
/// set's struct types.
fn absence(
    record: &[(String, Raw)],
    path: &str,
    entries: &[Value],
    types: &Map<String, Value>,
) -> Result<Option<&'static str>, String> {
    let lost =
        |what: &str| -> Result<Option<&'static str>, String> { Err(format!("{path}: {what}")) };
    let scalar = |raw: &Raw| !matches!(raw, Raw::Array(_) | Raw::Record(_) | Raw::Null);
    let parts: Vec<&str> = path.split('.').collect();
    let mut container = record;
    let mut entries = entries;
    for (k, part) in parts.iter().enumerate() {
        let last = k + 1 == parts.len();
        let (name, index) = match part.split_once('[') {
            Some((name, rest)) => {
                match rest.strip_suffix(']').and_then(|i| i.parse::<usize>().ok()) {
                    Some(index) => (name, Some(index)),
                    None => return lost("an index that isn't one"),
                }
            }
            None => (*part, None),
        };
        let Some(entry) = entries.iter().find(|entry| entry["name"] == name) else {
            return lost("an entry the IR doesn't have");
        };
        let Some((_, raw)) = container.iter().find(|(field, _)| field == name) else {
            let holder = !last || index.is_some();
            return match (
                entry["optional"] == true,
                entry.get("when").is_some(),
                holder,
            ) {
                (true, _, false) => Ok(Some("member_optional")),
                (true, _, true) => Ok(Some("holder_optional")),
                (false, true, false) => Ok(Some("member_skipped")),
                (false, true, true) => Ok(Some("holder_skipped")),
                (false, false, _) => lost("a required entry absent"),
            };
        };
        let inner = match (index, raw) {
            (Some(i), Raw::Array(items)) => match items.get(i) {
                None => return Ok(Some("past_end")),
                Some(Raw::Null) if last => return Ok(Some("null_element")),
                Some(Raw::Null) => return Ok(Some("null_record")),
                Some(item) if last && scalar(item) => return Ok(None),
                Some(Raw::Record(fields)) if !last => {
                    container = fields;
                    entry["element"]["name"].as_str()
                }
                Some(_) => return lost("an element of the wrong shape"),
            },
            (None, raw) if last && scalar(raw) => return Ok(None),
            (None, Raw::Record(fields)) if !last => {
                container = fields;
                entry["struct"].as_str()
            }
            _ => return lost("a value of the wrong shape"),
        };
        let Some(inner) = inner
            .and_then(|name| types.get(name))
            .and_then(|ty| ty["entries"].as_array())
        else {
            return lost("a struct type the IR doesn't have");
        };
        entries = inner;
    }
    lost("a path that ends at a struct")
}

/// Every composite a packet plans finds its members once its set is laid
/// out, so none drops out of the bundle unnoticed, as `body/tests.rs`
/// checks for binary bodies.
#[test]
fn every_composite_finds_its_members() {
    let mut runner = TestRunner::deterministic();
    let strategy = vec(packet_spec(Just(TimeFields::default()).boxed()), 1..=3);
    let mut composites = 0;
    for _ in 0..400 {
        let specs = strategy.new_tree(&mut runner).unwrap().current();
        let wanted: Vec<usize> = specs.iter().map(|spec| spec.composites.len()).collect();
        let set = lay_out(
            "",
            Alias::Fixed,
            JsonTime::Iso8601,
            None,
            false,
            specs,
            None,
        );
        let resolved: Vec<usize> = set
            .packets
            .iter()
            .map(|packet| packet.composites.len())
            .collect();
        assert_eq!(resolved, wanted, "a composite didn't resolve");
        composites += wanted.iter().sum::<usize>();
    }
    assert!(composites > 100, "only {composites} composites generated");
}

/// How deep a value's arrays and objects nest, a scalar being 0.
fn depth(value: &Value) -> usize {
    match value {
        Value::Array(items) => 1 + items.iter().map(depth).max().unwrap_or(0),
        Value::Object(members) => 1 + members.values().map(depth).max().unwrap_or(0),
        _ => 0,
    }
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
    for _ in 0..1000 {
        let case = strategy.new_tree(&mut runner).unwrap().current();
        found.extend(labels(&case));
    }
    for feature in [
        "encoding:json",
        "locator:fixed",
        "locator:path",
        "alias:number",
        "alias:number_as_text",
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
        "identify:stray_sub_unreadable",
        "identify:catch_all_no_kind",
        "identify:catch_all_kind_unreadable",
        "identify:catch_all_kind_as_text",
        "identify:by_metadata",
        "identify:metadata_and_pointer",
        "metadata:none",
        "metadata:twice",
        "metadata:leading_zero",
        "metadata:integer_as_text",
        "metadata:text",
        "metadata:integer",
        "metadata:decoy_member",
        "pointer:element_index",
        "pointer:element_two_digits",
        "pointer:element_leading_zero",
        "pointer:element_dash",
        "pointer:element_plus",
        "pointer:element_decimal",
        "pointer:element_empty",
        "identify:negative_integer",
        "identify:wide_integer",
        "pointer:through_element",
        "element:in_an_array",
        "element:as_a_member",
        "absent:array_ends_before",
        "absent:array_and_no_index",
        "state:beyond_letters",
        "state:twin_unknown",
        "state:read_beyond_letters",
        "sentinel:empty_text",
        "sentinel:beyond_letters",
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
        "number:past_double_digits",
        "number:below_smallest",
        "edge:depth_127",
        "edge:alike_names",
        "edge:noncharacters",
        "edge:tiny_number",
        "edge:empty_name",
        "edge:zero_huge_exponent",
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
        // Composites (§7.4): each holder, each frame pairing's odd one out,
        // a sample good or not, and each way a member is absent.
        "key:packet.composites",
        "param_type:vec3",
        "param_type:quat",
        "frame:GEODETIC",
        "composite:fields",
        "composite:struct",
        "composite:array",
        "composite:records",
        "composite:sample",
        "composite:not_good",
        "composite:absent_member_optional",
        "composite:absent_member_skipped",
        "composite:absent_holder_optional",
        "composite:absent_holder_skipped",
        "composite:absent_past_end",
        "composite:absent_null_element",
        "composite:absent_null_record",
        // Boot time and correlation (§8, §8.2).
        "time:boot",
        "time:boot/correlate",
        "time:since_boot",
        "boot:wide_count",
        "boot:count_sentinel",
        "boot:correlation",
        "boot:wide_clock",
        "boot:wide_boot",
        "boot:clock_sentinel",
        "boot:boot_sentinel",
    ] {
        assert!(found.contains(feature), "no JSON layout reaches {feature}");
    }
    let unexplained: Vec<&String> = found
        .iter()
        .filter(|label| label.starts_with("composite:unexplained"))
        .collect();
    assert!(unexplained.is_empty(), "{unexplained:?}");
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
    /// Of two faults of these kinds, decoding met the first's first.
    Over(FaultKind, FaultKind),
    /// A document that isn't an object gave this error.
    Document(&'static str),
    /// With a fixed alias, of two faults the error is `invalid_json`, the
    /// other met later.
    ParsedFirst,
    /// An error at a correlation packet's clock or boot field.
    Clock,
}

/// Where the entry an error names is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Named {
    AtTop,
    Element,
    InRecord,
    InStruct,
    /// No entry: before any, or the document as a whole.
    Whole,
}

/// Damaged JSON messages reach every route their faults take, every kind
/// of value `wrong_type` can find, every place an error can name, each
/// error a document that isn't an object can give, pairs, and §3.3's
/// order between kinds, at least five times each.
#[test]
fn json_damaged_messages_reach_every_route() {
    let mut runner = TestRunner::deterministic();
    let strategy = json_damaged();
    let mut found: BTreeMap<Reached, usize> = BTreeMap::new();
    let mut note = |reached: Reached| *found.entry(reached).or_default() += 1;
    let (mut messages, mut damaged) = (0, 0);
    for _ in 0..800 {
        let case = strategy.new_tree(&mut runner).unwrap().current();
        let fixed = case.bundle["families"][0]["alias"]["kind"] == "fixed";
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
            let code = failure.problem.code();
            note(Reached::Code(code));
            if let Problem::WrongType { found, .. } = failure.problem {
                note(Reached::Found(found));
            }
            if let [Route::Document(_)] = damage.routes.as_slice() {
                note(Reached::Document(code));
            }
            if let [first, second] = damage.routes.as_slice() {
                note(Reached::Pair);
                let code_of = |route: &Route| match route.kind() {
                    FaultKind::AsBinary => "wrong_encoding",
                    FaultKind::Malformed => "invalid_json",
                    FaultKind::Locator => "unreadable_locator",
                    FaultKind::Unbound => "unknown_asset",
                    FaultKind::Unmatched => "no_packet",
                    FaultKind::Missing => "missing_value",
                    FaultKind::WrongType => "wrong_type",
                    _ => "out_of_range",
                };
                if code_of(second) == code && code_of(first) != code_of(second) {
                    note(Reached::SecondFirst);
                }
                let (met, other) = if code_of(first) == code {
                    (first, second)
                } else {
                    (second, first)
                };
                note(Reached::Over(met.kind(), other.kind()));
                // §3.3: with a fixed alias nothing reads the document before
                // decoding does, which parses it before identification.
                if fixed && code == "invalid_json" && other.kind() != FaultKind::AsBinary {
                    note(Reached::ParsedFirst);
                }
            }
            let path = failure.path.as_str();
            if matches!(path, "clock" | "boot") {
                note(Reached::Clock);
            }
            note(Reached::Named(if path.is_empty() {
                Named::Whole
            } else if path.contains("].") {
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
    required.extend(Malformed::ALL.map(|malformed| Reached::Route(Route::Malformed(malformed))));
    required.extend(Unusable::ALL.map(|unusable| Reached::Route(Route::Locator(unusable))));
    required.extend(Unusable::ALL.map(|unusable| Reached::Route(Route::Unmatched(unusable))));
    required.extend(Unmet::ALL.map(|unmet| Reached::Route(Route::Unmet(unmet))));
    required.extend([Route::AsBinary, Route::Unbound, Route::KindAsText].map(Reached::Route));
    required.extend(
        JsonKind::ALL
            .into_iter()
            .filter(|kind| *kind != JsonKind::Object)
            .map(|kind| Reached::Route(Route::Document(kind))),
    );
    // The document's own `wrong_type` finds `null` too.
    required.extend(JsonKind::ALL.map(Reached::Found));
    required.extend(
        [
            "missing_value",
            "wrong_type",
            "out_of_range",
            "wrong_encoding",
            "invalid_json",
            "unreadable_locator",
            "unknown_asset",
            "no_packet",
        ]
        .map(Reached::Code),
    );
    required.extend(["unreadable_locator", "wrong_type", "no_packet"].map(Reached::Document));
    required.extend(
        [
            Named::AtTop,
            Named::Element,
            Named::InRecord,
            Named::InStruct,
            Named::Whole,
        ]
        .map(Reached::Named),
    );
    required.extend([
        Reached::Pair,
        Reached::SecondFirst,
        Reached::ParsedFirst,
        Reached::Clock,
    ]);
    // §3.3's order, then identification before entries.
    required.extend(
        [
            (FaultKind::AsBinary, FaultKind::Malformed),
            (FaultKind::Malformed, FaultKind::Locator),
            (FaultKind::Malformed, FaultKind::Unmatched),
            (FaultKind::Locator, FaultKind::Unmatched),
            (FaultKind::Unbound, FaultKind::Unmatched),
            (FaultKind::Unmatched, FaultKind::Missing),
            (FaultKind::Malformed, FaultKind::WrongType),
        ]
        .map(|(met, other)| Reached::Over(met, other)),
    );
    for reached in required {
        let count = found.get(&reached).copied().unwrap_or(0);
        assert!(count >= 5, "{reached:?} is reached {count} times");
    }
}
