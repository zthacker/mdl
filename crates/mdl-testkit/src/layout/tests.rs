//! The packer against hand-worked bytes, so the oracle doesn't rest only
//! on agreeing with `mdl`. The whole-byte vectors are what Python's
//! `struct.pack` gives (`<H`, `>H`, `<Q`, `>f`, `<f`).

use super::meaning::{convert_unit, spline, table};
use super::time::{Scale, leaps, to_utc};
use super::{fit, put_bits, reference, sign_extend};

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

/// What a generated case holds that the IR's features don't name: how
/// arrays, structs and composites are arranged, where they start within a
/// byte, and what messages and samples come of them.
fn labels(case: &super::Telemetry) -> std::collections::BTreeSet<String> {
    use serde_json::Value;

    let mut found = std::collections::BTreeSet::new();
    let set = &case.bundle["sets"][0];
    let types = &set["types"];
    let little = case.bundle["families"][0]["byte_order"] == "little";
    let entries_of = |name: &str| {
        types[name]["entries"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    };
    // Each container's entries, with whether it's a struct type.
    let mut containers: Vec<(bool, Vec<Value>)> = Vec::new();
    for packet in set["packets"].as_array().into_iter().flatten() {
        containers.push((
            false,
            packet["entries"].as_array().cloned().unwrap_or_default(),
        ));
        let composites = packet["composites"].as_array().cloned().unwrap_or_default();
        let names: Vec<&str> = composites
            .iter()
            .filter_map(|c| c["canonical"].as_str())
            .collect();
        if names.windows(2).any(|pair| pair[0] > pair[1]) {
            found.insert("composite:listed_out_of_order".to_string());
        }
        for composite in &composites {
            for member in composite["members"].as_array().into_iter().flatten() {
                let member = member.as_str().unwrap_or("");
                let holder = match (member.contains('['), member.contains('.')) {
                    (false, false) => "fields",
                    (false, true) => "struct",
                    (true, false) => "array",
                    (true, true) => "records",
                };
                found.insert(format!("composite:{holder}"));
            }
            let canonical = composite["canonical"].as_str().unwrap_or("");
            let param = &case.bundle["catalog"]["params"][canonical];
            if param["frame"] != composite["frame"] {
                found.insert("composite:converted_frame".to_string());
            }
        }
    }
    for name in types.as_object().into_iter().flat_map(|types| types.keys()) {
        containers.push((true, entries_of(name)));
    }
    for (in_type, entries) in &containers {
        // Where each entry starts within a byte: what varies is whole
        // bytes, so this is exact.
        let mut at = 0_u64;
        let mut counted: Vec<(&str, &Value, &Value)> = Vec::new();
        let mut offsets = std::collections::BTreeSet::new();
        for (position, entry) in entries.iter().enumerate() {
            let kind = entry["kind"].as_str().unwrap_or("");
            let element = &entry["element"];
            if let Some(offset) = entry["offset_bits"].as_u64() {
                at = offset;
                found.insert(
                    if *in_type {
                        "offset:in_record"
                    } else {
                        "offset:top"
                    }
                    .to_string(),
                );
                if !offsets.insert(offset) {
                    found.insert("offset:alternatives".to_string());
                }
                let next = entries.get(position + 1);
                if entry.get("when").is_some()
                    && next.is_some_and(|next| next.get("offset_bits").is_none())
                {
                    found.insert("offset:conditional_then_sequential".to_string());
                }
            }
            if let Some(when) = entry.get("when") {
                found.insert(format!("when:{kind}"));
                if *in_type {
                    found.insert("when:in_struct".to_string());
                }
                let mut tested = when;
                while tested.get("field").is_none() {
                    tested = match (tested.get("condition"), tested["conditions"].get(0)) {
                        (Some(inner), _) | (None, Some(inner)) => inner,
                        (None, None) => break,
                    };
                }
                let selector = entries.iter().find(|e| e["name"] == tested["field"]);
                if let Some(ty) = selector.and_then(|s| s["type"].as_str()) {
                    found.insert(format!("selector:{ty}"));
                }
                // A condition compares the raw value, whatever it means.
                if selector.is_some_and(|s| s.get("conversion").is_some()) {
                    found.insert("selector:converted".to_string());
                }
                if selector.is_some_and(|s| s.get("invalid").is_some()) {
                    found.insert("selector:sentinel".to_string());
                }
            }
            if kind == "padding" {
                // Only spare bits are a byte or more: alignment is less.
                if entry["size_bits"].as_u64().is_some_and(|bits| bits >= 8) {
                    found.insert("padding:wide".to_string());
                }
                if position + 1 == entries.len() {
                    found.insert(
                        if *in_type {
                            "padding:struct_end"
                        } else {
                            "padding:packet_end"
                        }
                        .to_string(),
                    );
                }
            }
            if *in_type && entry["length"]["kind"] == "field" {
                found.insert("length:field/in_struct".to_string());
            }
            if element["length"]["kind"] == "terminator" {
                found.insert("element:terminated".to_string());
            }
            let size = |entry: &Value| -> u64 {
                entry["size_bits"]
                    .as_u64()
                    .or_else(|| entry["length"]["bytes"].as_u64().map(|b| b * 8))
                    .unwrap_or(0)
            };
            let struct_bits = |name: &str| -> u64 {
                // Fixed parts only: what varies is whole bytes.
                fn bits(entries: &[Value], types: &Value) -> u64 {
                    entries
                        .iter()
                        .map(|e| match e["kind"].as_str() {
                            Some("struct") => {
                                let name = e["struct"].as_str().unwrap_or("");
                                let inner = types[name]["entries"]
                                    .as_array()
                                    .cloned()
                                    .unwrap_or_default();
                                bits(&inner, types)
                            }
                            Some("array") => {
                                let count = e["count"]["count"].as_u64().unwrap_or(0);
                                let each = match e["element"]["name"].as_str() {
                                    Some(name) => {
                                        let inner = types[name]["entries"]
                                            .as_array()
                                            .cloned()
                                            .unwrap_or_default();
                                        bits(&inner, types)
                                    }
                                    None => e["element"]["size_bits"]
                                        .as_u64()
                                        .or_else(|| {
                                            e["element"]["length"]["bytes"].as_u64().map(|b| b * 8)
                                        })
                                        .unwrap_or(0),
                                };
                                count * each
                            }
                            _ => e["size_bits"]
                                .as_u64()
                                .or_else(|| e["length"]["bytes"].as_u64().map(|b| b * 8))
                                .unwrap_or(0),
                        })
                        .sum()
                }
                bits(&entries_of(name), types)
            };
            match kind {
                "struct" => {
                    let name = entry["struct"].as_str().unwrap_or("");
                    if *in_type {
                        found.insert("struct:nested".to_string());
                    }
                    if !at.is_multiple_of(8) {
                        found.insert("struct:mid_byte".to_string());
                    }
                    at += struct_bits(name);
                }
                "array" => {
                    if *in_type {
                        found.insert("array:in_struct".to_string());
                    }
                    let count = &entry["count"];
                    if let Some(field) = count["field"].as_str() {
                        if let Some((_, multiply, add)) = counted.iter().find(|(f, ..)| *f == field)
                        {
                            found.insert("count:shared".to_string());
                            if **multiply != count["multiply"] || **add != count["add"] {
                                found.insert("count:shared/scaled".to_string());
                            }
                        }
                        counted.push((field, &count["multiply"], &count["add"]));
                        let counter = entries.iter().find(|e| e["name"] == field);
                        if counter.is_some_and(|c| c.get("canonical").is_some()) {
                            found.insert("count:mapped".to_string());
                        }
                        // A count is the raw value, whatever it converts to.
                        if counter.is_some_and(|c| c.get("conversion").is_some()) {
                            found.insert("count:converted".to_string());
                        }
                    }
                    if count["count"] == 0 {
                        found.insert("count:zero".to_string());
                    }
                    let fixed = count["count"].as_u64();
                    match element["name"].as_str() {
                        Some(name) => {
                            let inner = entries_of(name);
                            if inner.iter().any(|e| e["kind"] == "array") {
                                found.insert("records:holding_arrays".to_string());
                            }
                            if inner.iter().any(|e| e["kind"] == "struct") {
                                found.insert("records:holding_structs".to_string());
                            }
                            let each = struct_bits(name);
                            if fixed.is_some() && !each.is_multiple_of(8) {
                                found.insert("records:bit_packed".to_string());
                            }
                            if count["kind"] == "remaining" {
                                found.insert("records:remaining".to_string());
                            }
                            if !at.is_multiple_of(8) {
                                found.insert("records:mid_byte".to_string());
                            }
                            at += fixed.unwrap_or(0) * each;
                        }
                        None => {
                            let ty = element["type"].as_str().unwrap_or("");
                            found.insert(format!("element:{ty}"));
                            let bits = size(element);
                            if !bits.is_multiple_of(8) && fixed.is_some_and(|n| n > 1) {
                                found.insert("element:bit_packed".to_string());
                                if little
                                    && element.get("byte_order").is_none()
                                    && matches!(ty, "uint" | "int")
                                {
                                    found.insert("element:narrow_little_endian".to_string());
                                }
                            }
                            at += fixed.unwrap_or(0) * bits;
                        }
                    }
                }
                _ => at += size(entry),
            }
        }
    }
    for message in &case.messages {
        let packet = set["packets"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|packet| packet["name"] == message.packet);
        let entries = packet
            .and_then(|packet| packet["entries"].as_array())
            .cloned()
            .unwrap_or_default();
        for entry in entries.iter().filter(|entry| entry.get("when").is_some()) {
            let present = message
                .record
                .iter()
                .any(|(name, _)| *name == entry["name"]);
            found.insert(
                if present {
                    "when:present"
                } else {
                    "when:absent"
                }
                .to_string(),
            );
        }
        let composites = packet
            .and_then(|packet| packet["composites"].as_array())
            .cloned()
            .unwrap_or_default();
        for composite in &composites {
            let sampled = message
                .samples
                .iter()
                .any(|sample| sample.param == composite["canonical"]);
            if !sampled {
                found.insert("composite:absent".to_string());
            }
        }
        for (name, raw) in &message.record {
            if matches!(raw, super::Raw::Array(items) if items.is_empty()) {
                found.insert(
                    if name == "rest" {
                        "array:remaining_empty"
                    } else {
                        "array:empty"
                    }
                    .to_string(),
                );
            }
        }
        for sample in &message.samples {
            if let super::Expected::Vector { .. } = sample.value {
                found.insert(format!("composite:{}", sample.quality));
                continue;
            }
            if sample.path.contains("].") {
                found.insert("sample:in_records".to_string());
            }
            if sample.path.matches('.').count() >= 2 {
                found.insert("sample:nested".to_string());
            }
            if sample.index.is_some() && sample.quality == "invalid" {
                found.insert("sample:element_invalid".to_string());
            }
            if let (Some(_), Some(index)) = (sample.time_offset_ns, sample.index) {
                found.insert("sample:time_offset".to_string());
                let interval = interval_of(set, &sample.param);
                let exact = index as f64 * interval * 1_000_000.0;
                if exact.fract().abs() == 0.5 {
                    found.insert("sample:time_offset/tie".to_string());
                }
            }
        }
    }
    found
}

/// The `sample_interval_ms` of the array that maps to `param`.
fn interval_of(set: &serde_json::Value, param: &str) -> f64 {
    fn find(entries: &[serde_json::Value], param: &str) -> Option<f64> {
        entries
            .iter()
            .find(|e| e["canonical"] == param && e["kind"] == "array")
            .and_then(|e| e["sample_interval_ms"].as_f64())
    }
    let packets = set["packets"].as_array().cloned().unwrap_or_default();
    let types = set["types"].as_object().cloned().unwrap_or_default();
    packets
        .iter()
        .filter_map(|p| p["entries"].as_array())
        .chain(types.values().filter_map(|t| t["entries"].as_array()))
        .find_map(|entries| find(entries, param))
        .unwrap_or(0.0)
}

/// Generated layouts reach every meaning the oracle computes: each
/// conversion kind and option, units, sentinels, every quality a binary
/// field can have, and arrays, structs and composites in every
/// arrangement the generator makes.
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
    let leap_starts: Vec<i128> = leaps().map(|(start, _)| start).collect();
    let mut inside_leap = false;
    let mut unreadable = false;
    // A leap second's event time is the rarest: about 1 case in 60.
    for _ in 0..600 {
        let case = strategy.new_tree(&mut runner).unwrap().current();
        found.extend(features(&case.bundle));
        found.extend(labels(&case));
        for message in &case.messages {
            qualities.extend(message.samples.iter().map(|sample| sample.quality));
            match message.event_time {
                super::EventTime::At(at) => inside_leap |= leap_starts.contains(&(at + 1)),
                super::EventTime::Invalid => unreadable = true,
                super::EventTime::Ground | super::EventTime::SinceBoot { .. } => {}
            }
        }
    }
    assert!(inside_leap, "no event time falls inside a leap second");
    assert!(unreadable, "no event time is unreadable");
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
        "length:terminator",
        "length:field",
        "length:remaining",
        "key:length.multiply",
        "key:length.add",
        "length:field/in_struct",
        "element:terminated",
        "entry:padding",
        "padding:wide",
        "padding:struct_end",
        "padding:packet_end",
        // Offsets and conditions.
        "key:entry.offset_bits",
        "key:entry.when",
        "condition:compare",
        "condition:in",
        "condition:all",
        "condition:any",
        "condition:not",
        "op:eq",
        "op:ne",
        "op:lt",
        "op:le",
        "op:gt",
        "op:ge",
        "op:mask_any",
        "op:mask_all",
        "offset:top",
        "offset:in_record",
        "offset:alternatives",
        "offset:conditional_then_sequential",
        "when:field",
        "when:array",
        "when:struct",
        "when:padding",
        "when:in_struct",
        "selector:uint",
        "selector:int",
        "selector:bool",
        "selector:string",
        "selector:converted",
        "selector:sentinel",
        "count:converted",
        "when:present",
        "when:absent",
        "composite:absent",
        "byte_order:little",
        "byte_order:big",
        "time:cuc",
        "time:elapsed",
        "time:gps",
        "time:gps/week_bits",
        "scale:utc",
        "scale:tai",
        "scale:gps",
        "scale:tt",
        "time_unit:s",
        "time_unit:ms",
        "time_unit:us",
        "time_unit:ns",
        "key:packet.time_field",
        // Arrays and structs.
        "entry:array",
        "entry:struct",
        "element:scalar",
        "element:struct",
        "count:fixed",
        "count:field",
        "count:remaining",
        "key:count.multiply",
        "key:count.add",
        "key:entry.sample_interval_ms",
        "key:set.types",
        "key:element.byte_order",
        "key:element.length",
        "key:element.text",
        "key:element.conversion",
        "key:element.unit",
        "key:element.invalid",
        "element:uint",
        "element:int",
        "element:float",
        "element:bool",
        "element:string",
        "element:bytes",
        "element:bit_packed",
        "element:narrow_little_endian",
        "struct:nested",
        "struct:mid_byte",
        "array:in_struct",
        "records:holding_arrays",
        "records:holding_structs",
        "records:bit_packed",
        "records:remaining",
        "records:mid_byte",
        "count:shared",
        "count:shared/scaled",
        "count:mapped",
        "count:zero",
        "array:empty",
        "array:remaining_empty",
        "sample:in_records",
        "sample:nested",
        "sample:element_invalid",
        "sample:time_offset",
        "sample:time_offset/tie",
        // Composites.
        "key:packet.composites",
        "param_type:vec3",
        "param_type:quat",
        "key:param.frame",
        "frame:ECEF",
        "frame:ECI_J2000",
        "frame:BODY",
        "frame:NED",
        "frame:ENU",
        "frame:GEODETIC",
        "composite:fields",
        "composite:struct",
        "composite:array",
        "composite:records",
        "composite:converted_frame",
        "composite:listed_out_of_order",
        "composite:good",
        "composite:invalid",
        "composite:non_finite",
        "composite:out_of_calibration",
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

/// The oracle's splines, tables, unit conversions and time scales agree
/// with SciPy's, pint's and astropy's (`reference/generate.py`), so its
/// arithmetic doesn't rest only on agreeing with `mdl`.
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
    // astropy rounds to the nanosecond through two doubles, so allow ten.
    for &(scale, label, want) in reference::TIMES {
        let on = match scale {
            "tai" => Scale::Tai,
            "gps" => Scale::Gps,
            "tt" => Scale::Tt,
            _ => Scale::Utc,
        };
        let got = to_utc(label, on);
        assert!(
            (got - want).abs() <= 10,
            "label {label} on {scale}: {got}, astropy {want}"
        );
    }
    for &(from, to, value, want) in reference::UNITS {
        let got = convert_unit(value, from, to);
        assert!(
            close(got, want, want),
            "{value} {from} in {to}: {got}, pint {want}"
        );
    }
}

/// What the damaged-message reach test counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Reached {
    Kind(super::FaultKind),
    Route(super::Route),
    /// Two faults planted in one message.
    Pair,
    /// A fault in a message whose event time can't be read.
    UnreadableTime,
    /// An error of this code.
    Code(&'static str),
    /// An error naming an entry here.
    Named(Named),
}

/// Where the entry an error names is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Named {
    /// Padding, named by its container: none at the top level, an element
    /// or a struct.
    PaddingAtTop,
    PaddingInElement,
    PaddingInStruct,
    /// A field of an array's record.
    InRecord,
    /// A scalar element.
    Element,
    /// A field of a struct entry.
    InStruct,
    /// An entry at the top level.
    AtTop,
}

impl Named {
    const ALL: [Named; 7] = [
        Named::PaddingAtTop,
        Named::PaddingInElement,
        Named::PaddingInStruct,
        Named::InRecord,
        Named::Element,
        Named::InStruct,
        Named::AtTop,
    ];
}

/// Whether one more element must reach `place` in the reach test. A new
/// place won't compile until it's decided here (and listed in
/// `Place::ALL`).
fn more_required(place: super::Place) -> bool {
    use super::Place;

    match place {
        Place::Array | Place::Element | Place::Field | Place::Terminator => true,
        // Inside an element, but rare: a bit-each check needs an element
        // holding an array of elements without an exact size, and padding
        // a cut can land in needs spare bits across a byte.
        Place::ArrayBitEach | Place::PaddingInElement | Place::PaddingInStruct => false,
        // Never inside an element.
        Place::Header | Place::Padding | Place::TimeCode => false,
    }
}

/// Damaged messages reach every route the faults take, and every error
/// and place an error can name, at least five times each.
#[test]
fn damaged_messages_reach_every_route() {
    use std::collections::BTreeMap;

    use proptest::strategy::{Strategy, ValueTree};
    use proptest::test_runner::TestRunner;

    use super::{EventTime, FaultKind, Garbling, Held, Place, Problem, Route, damaged};

    let mut runner = TestRunner::deterministic();
    let strategy = damaged();
    let mut found: BTreeMap<Reached, usize> = BTreeMap::new();
    let mut note = |reached: Reached| *found.entry(reached).or_default() += 1;
    let (mut messages, mut damaged) = (0, 0);
    for _ in 0..1000 {
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
            if damage.routes.len() > 1 {
                note(Reached::Pair);
            }
            if message.event_time == EventTime::Invalid {
                note(Reached::UnreadableTime);
            }
            let failure = &damage.failure;
            note(Reached::Code(failure.problem.code()));
            let named = match (failure.problem, failure.path.as_str()) {
                (Problem::PastEnd { padding: true, .. }, "") => Named::PaddingAtTop,
                (Problem::PastEnd { padding: true, .. }, path) if path.ends_with(']') => {
                    Named::PaddingInElement
                }
                (Problem::PastEnd { padding: true, .. }, _) => Named::PaddingInStruct,
                (_, path) if path.contains("].") => Named::InRecord,
                (_, path) if path.ends_with(']') => Named::Element,
                (_, path) if path.contains('.') => Named::InStruct,
                _ => Named::AtTop,
            };
            note(Reached::Named(named));
        }
    }
    println!("{damaged} of {messages} messages damaged; {found:#?}");
    // A message is left as it is only when nothing can be planted in it.
    assert!(
        damaged * 10 >= messages * 9,
        "{damaged} of {messages} damaged"
    );
    let mut required: Vec<Reached> = FaultKind::ALL.map(Reached::Kind).to_vec();
    required.extend(Place::ALL.map(|place| Reached::Route(Route::Cut(place))));
    required.extend(
        Place::ALL
            .into_iter()
            .filter(|place| more_required(*place))
            .map(|place| Reached::Route(Route::More(place))),
    );
    required.extend(Held::ALL.map(|held| Reached::Route(Route::Count(held))));
    required.extend(Held::ALL.map(|held| Reached::Route(Route::Length(held))));
    required.push(Reached::Route(Route::SharedCount));
    required.extend(Garbling::ALL.map(|form| Reached::Route(Route::Text(form))));
    required.extend([Reached::Pair, Reached::UnreadableTime]);
    required.extend(["past_end", "no_terminator", "invalid_text", "bad_length"].map(Reached::Code));
    required.extend(Named::ALL.map(Reached::Named));
    for reached in required {
        let count = found.get(&reached).copied().unwrap_or(0);
        assert!(count >= 5, "{reached:?} is reached {count} times");
    }
}
