//! The command oracle against the spec's own worked examples, and the
//! generator reaching every feature it claims to.

use std::collections::{BTreeMap, BTreeSet};

use proptest::strategy::{Strategy, ValueTree};
use proptest::test_runner::TestRunner;
use serde_json::Value;

use super::encoders::f64_encoder;
use super::requests::{buffer, decoded_value, native, raw_of};
use super::values::{unrounded, value};
use super::{Arg, ArgType, Commands, Given, Len, Shape, Target, Through, commands, encoder_of};
use crate::coverage::features;
use crate::layout::{Numeric, Raw};

fn linear(scale: f64, offset: f64, shape: Shape) -> super::Encoder {
    let mut encoder = encoder_of(ArgType::F64, shape);
    encoder.through = Through::Exact(Numeric::Linear { scale, offset });
    encoder
}

fn raw(encoder: &super::Encoder, value: f64) -> Raw {
    raw_of(encoder, &encoder.shape, &Arg::F64(value))
}

/// `spec/encoding.md` §3.1's examples: each operation rounded as written,
/// then ties away from zero for an integer field, to even for binary32.
#[test]
fn values_round_as_the_spec_works_them() {
    let tenth = linear(0.1, 0.0, Shape::Uint(16));
    assert_eq!(unrounded(&tenth, 0.15), Some(1.4999999999999998));
    assert_eq!(raw(&tenth, 0.15), Raw::Uint(1));
    assert_eq!(raw(&tenth, 0.25), Raw::Uint(3));

    let mut inverse = encoder_of(ArgType::F64, Shape::Uint(16));
    inverse.through = Through::Inverse {
        conversion: Numeric::Poly(vec![0.0, 0.1]),
        inverse: Numeric::Poly(vec![-0.1, 10.0]),
    };
    assert_eq!(unrounded(&inverse, 0.36), Some(3.4999999999999996));
    assert_eq!(raw(&inverse, 0.36), Raw::Uint(3));

    let unit = linear(1.0, 0.0, Shape::Int(8));
    assert_eq!(raw(&unit, 2.5), Raw::Int(3));
    assert_eq!(raw(&unit, -2.5), Raw::Int(-3));

    // 1 + 2^-24 is halfway between 1 and 1 + 2^-23; 1 + 3·2^-24 halfway
    // between 1 + 2^-23 and 1 + 2^-22.
    let single = encoder_of(ArgType::F64, Shape::Float(32));
    let step = f64::powi(2.0, -24);
    assert_eq!(raw(&single, 1.0 + step), Raw::Float(1.0));
    assert_eq!(raw(&single, 1.0 + 3.0 * step), Raw::Float(1.0 + 4.0 * step));
}

/// §3.1: 25 Cel into a K field is 298.15 before its inverse.
#[test]
fn units_convert_before_the_inverse() {
    let mut heat = encoder_of(ArgType::F64, Shape::Float(64));
    heat.units = Some(("Cel", "K"));
    assert_eq!(unrounded(&heat, 25.0), Some(298.15));
}

/// §7.4: 3600.06 s at 0.1 s per count encodes to raw 36001, which converts
/// to 3600.1 s and decodes as 3600.06 s when the range ends there.
#[test]
fn a_value_past_its_range_only_by_rounding_decodes_to_the_ranges_end() {
    let mut delay = linear(0.1, 0.0, Shape::Uint(16));
    delay.max = Some(3600.06);
    assert_eq!(raw(&delay, 3600.06), Raw::Uint(36001));
    assert_eq!(decoded_value(&delay, &Raw::Uint(36001)), Arg::F64(3600.06));
}

/// §3.4: a NUL terminator pads as C's `strncpy` does, a space with spaces,
/// none when the value fills the buffer.
#[test]
fn a_terminated_buffer_is_padded_with_its_terminator() {
    assert_eq!(
        buffer(b"PING".to_vec(), &Len::Padded(8, 0)),
        b"PING\0\0\0\0"
    );
    assert_eq!(buffer(b"ON".to_vec(), &Len::Padded(4, b' ')), b"ON  ");
    assert_eq!(buffer(b"FULL".to_vec(), &Len::Padded(4, 0)), b"FULL");
    assert_eq!(buffer(b"ON".to_vec(), &Len::Terminator(b';')), b"ON;");
}

/// `spec/bindings.md` §3: text is an integer only in its one decimal
/// spelling, between −2^63 and 2^64 − 1.
#[test]
fn native_ids_are_integers_only_in_their_one_spelling() {
    assert_eq!(native("42"), Target::Int(42));
    assert_eq!(native("-7"), Target::Int(-7));
    assert_eq!(native("0"), Target::Int(0));
    assert_eq!(native("18446744073709551615"), Target::Int(u64::MAX.into()));
    assert_eq!(native("-9223372036854775808"), Target::Int(i64::MIN.into()));
    for text in [
        "042",
        "-0",
        "+5",
        "18446744073709551616",
        "-9223372036854775809",
        "",
        "PX-9",
    ] {
        assert_eq!(native(text), Target::Text(text.into()), "{text:?}");
    }
}

/// Requests lean toward raw values exactly halfway between two a field
/// holds, where rounding decides.
#[test]
fn requests_reach_rounding_ties() {
    let mut runner = TestRunner::deterministic();
    let strategy = f64_encoder().prop_flat_map(|encoder| {
        let values = proptest::collection::vec(value(&encoder), 20);
        (proptest::strategy::Just(encoder), values)
    });
    let (mut integer_ties, mut binary32_ties) = (0, 0);
    for _ in 0..500 {
        let (encoder, values) = strategy.new_tree(&mut runner).unwrap().current();
        for value in values {
            let Arg::F64(v) = value else { continue };
            let Some(x) = unrounded(&encoder, v) else {
                continue;
            };
            match encoder.shape {
                Shape::Uint(_) | Shape::Int(_) if x.fract().abs() == 0.5 => integer_ties += 1,
                Shape::Float(32) => {
                    let near = x as f32;
                    let other = if f64::from(near) > x {
                        near.next_down()
                    } else {
                        near.next_up()
                    };
                    if x == (f64::from(near) + f64::from(other)) / 2.0 {
                        binary32_ties += 1;
                    }
                }
                _ => {}
            }
        }
    }
    assert!(integer_ties >= 10, "{integer_ties} integer ties");
    assert!(binary32_ties >= 10, "{binary32_ties} binary32 ties");
}

/// What a case uses that the coverage walker doesn't name: which argument
/// types go into which fields and how, spellings, targets, keys, and
/// whether decoded arguments encode to the same payload.
fn labels(case: &Commands) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut note = |label: String| {
        found.insert(label);
    };
    let catalog = &case.bundle["catalog"]["commands"];
    let mut types: BTreeMap<(String, String), String> = BTreeMap::new();
    let packets = case.bundle["sets"][0]["commands"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for packet in &packets {
        let command = packet["canonical"].as_str().unwrap_or("");
        let arguments = catalog[command]["arguments"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let entries = packet["entries"].as_array().cloned().unwrap_or_default();
        let unit_of = |name: &str| {
            arguments
                .iter()
                .find(|argument| argument["name"] == name)
                .and_then(|argument| argument["unit"].as_str().map(str::to_string))
        };
        for argument in &arguments {
            let name = argument["name"].as_str().unwrap_or("").to_string();
            let ty = argument["type"].as_str().unwrap_or("").to_string();
            if !entries
                .iter()
                .any(|entry| entry["argument"] == argument["name"])
            {
                note("argument:left_out".into());
                note(format!("argument:left_out/{ty}"));
            }
            let range = &argument["range"];
            if range["min"] == 0.0 || range["max"] == 0.0 {
                note("range:zero_end".into());
            }
            if range["min"].is_number() && range["min"] == range["max"] {
                note("range:one_value".into());
            }
            types.insert((command.to_string(), name), ty);
        }
        let key = &entries[0];
        note(format!("key:{}", key["type"].as_str().unwrap_or("")));
        if key["length"]["terminator"].is_number() {
            note("key:string/terminated".into());
        }
        let sums: Vec<&Value> = entries
            .iter()
            .filter(|entry| entry["kind"] == "checksum")
            .collect();
        if sums.len() >= 2 {
            note("checksums:two".into());
            // From byte 0, the later covers the earlier's final value.
            if sums[1].get("from_bits").is_none() {
                note("checksums:later_covers_earlier".into());
            }
        }
        // A length field with other entries between it and its field.
        for (k, entry) in entries.iter().enumerate() {
            let Some(measured) = entry["name"].as_str().and_then(|n| n.strip_suffix("_len")) else {
                continue;
            };
            let next = entries[k + 1..]
                .iter()
                .find(|later| later["kind"] == "field");
            if next.is_some_and(|later| later["name"] != measured) {
                note("length:early".into());
            }
        }
        for entry in &entries {
            let field = entry["type"].as_str().unwrap_or("");
            if let Some(order) = entry["byte_order"].as_str() {
                note(format!("entry_order:{order}"));
            }
            if field == "float" {
                note(format!("float:{}", entry["size_bits"]));
            }
            if entry["target"] == true {
                note(format!("target>{field}"));
            }
            let Some(name) = entry["argument"].as_str() else {
                continue;
            };
            let ty = types
                .get(&(command.to_string(), name.to_string()))
                .cloned()
                .unwrap_or_default();
            let conversion = &entry["conversion"];
            match conversion["kind"].as_str() {
                Some("states") => note(format!("argument:{ty}>{field}/states")),
                _ => note(format!("argument:{ty}>{field}")),
            }
            if ty == "f64" {
                let kind = conversion["kind"].as_str().unwrap_or("plain");
                if let Some(inverse) = entry["inverse"]["kind"].as_str() {
                    note(format!("inverse:{inverse}"));
                    let step = conversion["interpolation"] == "step";
                    note(format!("forward:{kind}{}", if step { "/step" } else { "" }));
                } else {
                    note(format!("f64:{kind}"));
                    let points = conversion["points"].as_array().cloned().unwrap_or_default();
                    if points.len() >= 2
                        && points[0]["value"].as_f64() > points[1]["value"].as_f64()
                    {
                        note("f64:table/falling".into());
                    }
                }
                let unit = entry["unit"].as_str().map(str::to_string);
                if unit.is_some() && unit != unit_of(name) {
                    note("units:converted".into());
                }
            }
        }
    }
    for request in &case.requests {
        for (name, given) in &request.arguments {
            let ty = types
                .get(&(request.command.clone(), name.clone()))
                .cloned()
                .unwrap_or_default();
            let spelled = match given {
                Given::Int(_) => "Int",
                Given::Uint(_) => "Uint",
                Given::Float(x) if x.abs() >= 2_f64.powi(53) && ty != "f64" => "wide Float",
                Given::Float(_) => "Float",
                _ => continue,
            };
            note(format!("given:{spelled}-for-{ty}"));
        }
        match &request.target {
            Some(Target::Int(_)) => note("target:int".into()),
            Some(Target::Text(text)) => match native(text) {
                Target::Int(_) => note("target:integer_text".into()),
                Target::Text(_) => note("target:text".into()),
            },
            None => {}
        }
        if !request.defaulted.is_empty() {
            note("request:defaulted".into());
        }
        if !request.metadata.is_empty() {
            note("request:metadata".into());
        }
        let again = if request.again == request.payload {
            "same"
        } else {
            "differs"
        };
        note(format!("again:{again}"));
    }
    found
}

#[test]
fn commands_reach_every_feature() {
    let mut runner = TestRunner::deterministic();
    let strategy = commands();
    let mut found = BTreeSet::new();
    for _ in 0..300 {
        let case = strategy.new_tree(&mut runner).unwrap().current();
        found.extend(features(&case.bundle));
        found.extend(labels(&case));
    }
    let required = [
        "param_type:bool",
        "param_type:u64",
        "param_type:i64",
        "param_type:f64",
        "param_type:enum",
        "param_type:str",
        "param_type:bytes",
        "conversion:linear",
        "conversion:poly",
        "conversion:states",
        "conversion:table",
        "conversion:table/step",
        "conversion:table/extrapolate",
        "conversion:spline",
        "conversion:spline/extrapolate",
        "key:entry.value",
        "key:entry.argument",
        "key:entry.inverse",
        "key:entry.target",
        "key:entry.from_bits",
        "key:entry.unit",
        "text:ascii",
        "text:latin1",
        "length:fixed",
        "length:fixed/terminator",
        "length:field",
        "length:terminator",
        "length:remaining",
        "key:length.multiply",
        "key:length.add",
        "checksum:crc",
        "checksum:crc/reflected",
        "checksum:sum",
        "checksum:xor",
        "checksum:iso8473",
        "key:family.metadata",
        "key:command_packet.metadata",
        "key:argument.unit",
        "key:argument.states",
        "key:argument.range",
        "default:bool",
        "default:int",
        "default:float",
        "default:str",
        "scalar:string_integer",
        "argument:bool>bool",
        "argument:bool>uint",
        "argument:bool>int",
        "argument:u64>uint",
        "argument:i64>int",
        "argument:i64>uint",
        "argument:f64>float",
        "argument:f64>uint",
        "argument:f64>int",
        "argument:enum>uint/states",
        "argument:enum>int/states",
        "argument:enum>string",
        "argument:str>string",
        "argument:bytes>bytes",
        "argument:left_out",
        "f64:plain",
        "f64:linear",
        "f64:table",
        "f64:table/falling",
        "inverse:linear",
        "inverse:poly",
        "inverse:table",
        "forward:poly",
        "forward:spline",
        "forward:table",
        "forward:table/step",
        "units:converted",
        "float:32",
        "float:64",
        "entry_order:little",
        "entry_order:big",
        "key:uint",
        "key:int",
        "key:string",
        "key:string/terminated",
        "checksums:two",
        "checksums:later_covers_earlier",
        "length:early",
        "range:zero_end",
        "range:one_value",
        "argument:left_out/u64",
        "given:Uint-for-f64",
        "given:wide Float-for-u64",
        "given:wide Float-for-i64",
        "given:Int-for-u64",
        "given:Float-for-u64",
        "given:Uint-for-i64",
        "given:Float-for-i64",
        "given:Int-for-f64",
        "target:int",
        "target:integer_text",
        "target:text",
        "target>string",
        "target>uint",
        "target>int",
        "request:defaulted",
        "request:metadata",
        "again:same",
        "again:differs",
    ];
    let missing: Vec<&str> = required
        .into_iter()
        .filter(|feature| !found.contains(*feature))
        .collect();
    assert!(missing.is_empty(), "never generated: {missing:?}");
}
