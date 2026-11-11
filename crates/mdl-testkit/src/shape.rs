//! Any bundle the IR's shape allows: every kind and key, with values that
//! read but needn't validate. Names, units, pointers and epochs are any
//! text, quotes, control and astral characters included, and numbers are
//! any of their type: any finite double, and integers JSON reads exactly.
//!
//! Each bundle is spelled as the IR writes it. An empty collection (a list
//! or map the IR writes absent when it's empty, not an empty list default,
//! which is a value) is left out; everything else is written as it reads.
//! So reading a generated bundle and writing it back gives the same
//! canonical JSON, and any difference is the reader's or the writer's.
//! [`loosely_spelled`] gives the same bundles spelled as the IR reads but
//! doesn't write them.

use std::ops::Range;

use proptest::collection::{btree_map, vec};
use proptest::option;
use proptest::prelude::*;
use serde_json::{Map, Number, Value, json};

type Gen = BoxedStrategy<Value>;

/// The largest integer every JSON reader reads exactly, 2^53 - 1.
const MAX_SAFE: i64 = (1 << 53) - 1;

/// How often a bundle's optional keys are given: each bundle picks one,
/// so some come out nearly bare and some nearly full, and keys that are
/// rarely given together are.
const DENSITIES: [f64; 3] = [0.1, 0.5, 0.9];

/// Any bundle the IR's shape allows.
pub fn bundle() -> Gen {
    prop::sample::select(&DENSITIES[..])
        .prop_flat_map(bundle_with)
        .boxed()
}

/// A bundle whose optional keys are each given with probability `given`.
fn bundle_with(given: f64) -> Gen {
    (
        catalog(given),
        maybe(given, list(family(given), 1..3)),
        maybe(given, list(set(given), 1..3)),
    )
        .prop_map(|(catalog, families, sets)| {
            object([
                ("mdl_version", Some(json!("2.0"))),
                ("catalog", Some(catalog)),
                ("families", families),
                ("sets", sets),
            ])
        })
        .boxed()
}

/// A bundle, and the same bundle spelled loosely: some of the collections
/// it leaves out given as empty, or as null, which the IR reads as absent.
pub fn loosely_spelled() -> BoxedStrategy<(Value, Value)> {
    (bundle(), vec(any::<u8>(), 64))
        .prop_map(|(bundle, choices)| {
            let mut loose = bundle.clone();
            loosen(&mut loose, "bundle", &mut choices.into_iter().cycle());
            (bundle, loose)
        })
        .boxed()
}

/// The collections an object may leave out, which the IR reads as empty
/// when they're empty or null, each with whether it's a map.
fn collections(holder: &str) -> &'static [(&'static str, bool)] {
    match holder {
        "bundle" => &[("families", false), ("sets", false)],
        "catalog" => &[("params", true), ("commands", true)],
        "command" => &[("arguments", false)],
        "param" | "argument" => &[("states", false)],
        "family" => &[("metadata", false)],
        "set" => &[("types", true), ("packets", false), ("commands", false)],
        "packet" => &[
            ("identify", true),
            ("entries", false),
            ("composites", false),
        ],
        "command_packet" => &[("metadata", true), ("entries", false)],
        "field" | "element" => &[("invalid", false)],
        _ => &[],
    }
}

/// Spells the collections `value`, held as `holder`, leaves out as empty
/// or null, as `choices` say, then does the same for what it holds.
fn loosen(value: &mut Value, holder: &str, choices: &mut impl Iterator<Item = u8>) {
    let Value::Object(members) = value else {
        return;
    };
    for &(key, is_map) in collections(holder) {
        if members.contains_key(key) {
            continue;
        }
        let spelled = match choices.next().unwrap_or(0) % 3 {
            0 => continue,
            1 if is_map => json!({}),
            1 => json!([]),
            _ => Value::Null,
        };
        members.insert(key.to_string(), spelled);
    }
    for (key, inner) in members.iter_mut() {
        let held = match (holder, key.as_str()) {
            ("bundle", "catalog") => "catalog",
            ("bundle", "families") => "family",
            ("bundle", "sets") => "set",
            ("catalog", "params") => "param",
            ("catalog", "commands") => "command",
            ("command", "arguments") => "argument",
            ("set", "types") => "struct_type",
            ("set", "packets") => "packet",
            ("set", "commands") => "command_packet",
            ("packet" | "command_packet" | "struct_type", "entries") => "entry",
            ("array", "element") if inner["kind"] == "scalar" => "element",
            _ => continue,
        };
        let maps = matches!(held, "param" | "command" | "struct_type");
        match inner {
            Value::Array(items) => items
                .iter_mut()
                .for_each(|item| loosen_one(item, held, choices)),
            Value::Object(members) if maps => members
                .values_mut()
                .for_each(|item| loosen_one(item, held, choices)),
            _ => loosen_one(inner, held, choices),
        }
    }
}

/// An entry is held as what its kind is.
fn loosen_one(value: &mut Value, held: &str, choices: &mut impl Iterator<Item = u8>) {
    let held = match (held, value["kind"].as_str()) {
        ("entry", Some("field")) => "field",
        ("entry", Some("array")) => "array",
        ("entry", _) => return,
        (held, _) => held,
    };
    loosen(value, held, choices);
}

// ---- Values ------------------------------------------------------------

/// Any text, each char any Unicode scalar value.
pub fn text() -> Gen {
    any_string().prop_map(Value::String).boxed()
}

fn any_string() -> BoxedStrategy<String> {
    vec(any::<char>(), 0..8).prop_map(String::from_iter).boxed()
}

/// Any finite double, with everyday values and whole numbers of every
/// size (past 2^53 and past i64 among them) more often than random bits
/// would give them.
pub fn finite() -> Gen {
    prop_oneof![
        2 => any::<f64>(),
        1 => -1e6..1e6_f64,
        1 => any::<i64>().prop_map(|n| n as f64),
        1 => any::<u64>().prop_map(|n| n as f64),
    ]
    .prop_filter_map("a finite double", |x| {
        Number::from_f64(x).map(Value::Number)
    })
    .boxed()
}

/// An integer within ±(2^53 - 1).
fn safe_int() -> Gen {
    (-MAX_SAFE..=MAX_SAFE).prop_map(|n| json!(n)).boxed()
}

fn u32_value() -> Gen {
    any::<u32>().prop_map(|n| json!(n)).boxed()
}

fn u8_value() -> Gen {
    any::<u8>().prop_map(|n| json!(n)).boxed()
}

fn boolean() -> Gen {
    any::<bool>().prop_map(Value::Bool).boxed()
}

/// One of `words`, as a string.
fn word(words: &'static [&'static str]) -> Gen {
    prop::sample::select(words)
        .prop_map(|word| json!(word))
        .boxed()
}

/// An integer past ±(2^53 - 1), written as the string the IR spells it.
fn wide_int_text() -> Gen {
    prop_oneof![
        ((MAX_SAFE as u64 + 1)..=u64::MAX).prop_map(|n| json!(n.to_string())),
        (i64::MIN..-MAX_SAFE).prop_map(|n| json!(n.to_string())),
    ]
    .boxed()
}

/// A constant: an integer JSON reads exactly, a wider one as a string, or
/// text.
pub fn scalar() -> Gen {
    prop_oneof![3 => safe_int(), 1 => wide_int_text(), 3 => text()].boxed()
}

/// An argument's default: a bool, a number, text (a wide integer among
/// it), or a list of them.
pub fn literal() -> Gen {
    let single = prop_oneof![boolean(), safe_int(), finite(), text(), wide_int_text()].boxed();
    prop_oneof![
        3 => single.clone(),
        1 => vec(single, 0..4).prop_map(Value::Array),
    ]
    .boxed()
}

// ---- Building blocks ------------------------------------------------------

/// A JSON object of the members that are given.
fn object<const N: usize>(members: [(&str, Option<Value>); N]) -> Value {
    Value::Object(
        members
            .into_iter()
            .filter_map(|(key, value)| Some((key.to_string(), value?)))
            .collect(),
    )
}

/// The value, given with probability `given`.
fn maybe(given: f64, strategy: Gen) -> BoxedStrategy<Option<Value>> {
    option::weighted(given, strategy).boxed()
}

fn list(strategy: Gen, size: Range<usize>) -> Gen {
    vec(strategy, size).prop_map(Value::Array).boxed()
}

/// A map from any text to the values.
fn map(values: Gen, size: Range<usize>) -> Gen {
    btree_map(any_string(), values, size)
        .prop_map(|members| Value::Object(members.into_iter().collect::<Map<_, _>>()))
        .boxed()
}

/// `kind` followed by nothing, for the kinds that have no keys.
fn bare(kind: &'static str) -> Gen {
    Just(json!({ "kind": kind })).boxed()
}

// ---- Catalog ----------------------------------------------------------------

fn catalog(given: f64) -> Gen {
    (
        maybe(given, map(param(given), 1..3)),
        maybe(given, map(command(given), 1..3)),
    )
        .prop_map(|(params, commands)| object([("params", params), ("commands", commands)]))
        .boxed()
}

fn param_type() -> Gen {
    word(&[
        "f64", "i64", "u64", "bool", "str", "bytes", "enum", "vec3", "quat",
    ])
}

fn frame() -> Gen {
    word(&["ECI_J2000", "ECEF", "BODY", "NED", "ENU", "GEODETIC"])
}

fn param(given: f64) -> Gen {
    let limits = (
        maybe(given, finite()),
        maybe(given, finite()),
        maybe(given, finite()),
        maybe(given, finite()),
    )
        .prop_map(|(red_low, yellow_low, yellow_high, red_high)| {
            object([
                ("red_low", red_low),
                ("yellow_low", yellow_low),
                ("yellow_high", yellow_high),
                ("red_high", red_high),
            ])
        })
        .boxed();
    (
        param_type(),
        maybe(given, text()),
        maybe(given, list(text(), 1..3)),
        maybe(given, frame()),
        maybe(given, limits),
        maybe(given, text()),
        maybe(given, text()),
    )
        .prop_map(
            |(ty, unit, states, frame, limits, description, deprecated)| {
                object([
                    ("type", Some(ty)),
                    ("unit", unit),
                    ("states", states),
                    ("frame", frame),
                    ("limits", limits),
                    ("description", description),
                    ("deprecated", deprecated),
                ])
            },
        )
        .boxed()
}

fn command(given: f64) -> Gen {
    (
        maybe(given, list(argument(given), 1..3)),
        maybe(given, text()),
        maybe(given, text()),
    )
        .prop_map(|(arguments, description, deprecated)| {
            object([
                ("arguments", arguments),
                ("description", description),
                ("deprecated", deprecated),
            ])
        })
        .boxed()
}

fn argument(given: f64) -> Gen {
    // An end is a number, or an integer past ±(2^53 − 1) as a string.
    let end = || prop_oneof![2 => finite(), 1 => safe_int(), 1 => wide_int_text()].boxed();
    let range = (maybe(given, end()), maybe(given, end()))
        .prop_map(|(min, max)| object([("min", min), ("max", max)]))
        .boxed();
    let bounds = (maybe(given, u32_value()), maybe(given, u32_value()))
        .prop_map(|(min, max)| object([("min", min), ("max", max)]))
        .boxed();
    (
        text(),
        param_type(),
        maybe(given, text()),
        maybe(given, list(text(), 1..3)),
        maybe(given, range),
        maybe(given, bounds),
        maybe(given, literal()),
        maybe(given, text()),
    )
        .prop_map(
            |(name, ty, unit, states, range, list, default, description)| {
                object([
                    ("name", Some(name)),
                    ("type", Some(ty)),
                    ("unit", unit),
                    ("states", states),
                    ("range", range),
                    ("list", list),
                    ("default", default),
                    ("description", description),
                ])
            },
        )
        .boxed()
}

// ---- Families ---------------------------------------------------------------

fn byte_order() -> Gen {
    word(&["big", "little"])
}

fn locator() -> Gen {
    prop_oneof![
        (u32_value(), u32_value()).prop_map(|(offset_bits, size_bits)| {
            json!({ "kind": "bits", "offset_bits": offset_bits, "size_bits": size_bits })
        }),
        text().prop_map(|name| json!({ "kind": "metadata", "name": name })),
        text().prop_map(|pointer| json!({ "kind": "path", "pointer": pointer })),
        text().prop_map(|value| json!({ "kind": "fixed", "value": value })),
    ]
    .boxed()
}

fn family(given: f64) -> Gen {
    (
        text(),
        word(&["binary", "json"]),
        maybe(given, byte_order()),
        locator(),
        maybe(given, locator()),
        maybe(given, list(text(), 1..3)),
    )
        .prop_map(|(name, encoding, byte_order, alias, version, metadata)| {
            object([
                ("name", Some(name)),
                ("encoding", Some(encoding)),
                ("byte_order", byte_order),
                ("alias", Some(alias)),
                ("version", version),
                ("metadata", metadata),
            ])
        })
        .boxed()
}

// ---- Sets ---------------------------------------------------------------

fn time_scale() -> Gen {
    word(&["utc", "tai", "gps", "tt"])
}

fn time_unit() -> Gen {
    word(&["s", "ms", "us", "ns"])
}

fn time(given: f64) -> Gen {
    let correlate = (text(), text(), time_unit(), text(), time_scale(), text()).prop_map(
        |(packet, clock_field, clock_unit, epoch, scale, boot_field)| {
            json!({
                "packet": packet, "clock_field": clock_field, "clock_unit": clock_unit,
                "epoch": epoch, "scale": scale, "boot_field": boot_field
            })
        },
    );
    prop_oneof![
        (u32_value(), u8_value(), u8_value(), text(), time_scale()).prop_map(
            |(offset_bits, coarse_bytes, fine_bytes, epoch, scale)| json!({
                "kind": "cuc", "offset_bits": offset_bits, "coarse_bytes": coarse_bytes,
                "fine_bytes": fine_bytes, "epoch": epoch, "scale": scale
            })
        ),
        text().prop_map(|pointer| json!({ "kind": "iso8601", "pointer": pointer })),
        (time_unit(), correlate).prop_map(|(unit, correlate)| {
            json!({ "kind": "boot", "unit": unit, "correlate": correlate })
        }),
        (time_unit(), text(), time_scale()).prop_map(|(unit, epoch, scale)| {
            json!({ "kind": "elapsed", "unit": unit, "epoch": epoch, "scale": scale })
        }),
        (time_unit(), text(), maybe(given, u8_value())).prop_map(
            |(unit, week_field, week_bits)| {
                object([
                    ("kind", Some(json!("gps"))),
                    ("unit", Some(unit)),
                    ("week_field", Some(week_field)),
                    ("week_bits", week_bits),
                ])
            }
        ),
    ]
    .boxed()
}

fn set(given: f64) -> Gen {
    let types = map(
        list(entry(given), 0..3)
            .prop_map(|entries| json!({ "entries": entries }))
            .boxed(),
        1..3,
    );
    (
        text(),
        text(),
        maybe(given, scalar()),
        maybe(given, time(given)),
        maybe(given, types),
        maybe(given, list(packet(given), 1..3)),
        maybe(given, list(command_packet(given), 1..3)),
    )
        .prop_map(|(name, family, version, time, types, packets, commands)| {
            object([
                ("name", Some(name)),
                ("family", Some(family)),
                ("version", version),
                ("time", time),
                ("types", types),
                ("packets", packets),
                ("commands", commands),
            ])
        })
        .boxed()
}

fn packet(given: f64) -> Gen {
    let length = prop_oneof![
        u32_value().prop_map(|bytes| json!({ "kind": "exact", "bytes": bytes })),
        u32_value().prop_map(|bytes| json!({ "kind": "at_least", "bytes": bytes })),
    ]
    .boxed();
    let composite = (text(), frame(), list(text(), 0..3))
        .prop_map(|(canonical, frame, members)| {
            json!({ "canonical": canonical, "frame": frame, "members": members })
        })
        .boxed();
    (
        text(),
        maybe(given, map(scalar(), 1..3)),
        maybe(given, length),
        maybe(given, text()),
        maybe(given, list(entry(given), 1..4)),
        maybe(given, list(composite, 1..3)),
    )
        .prop_map(
            |(name, identify, length, time_field, entries, composites)| {
                object([
                    ("name", Some(name)),
                    ("identify", identify),
                    ("length", length),
                    ("time_field", time_field),
                    ("entries", entries),
                    ("composites", composites),
                ])
            },
        )
        .boxed()
}

fn command_packet(given: f64) -> Gen {
    (
        text(),
        text(),
        maybe(given, map(scalar(), 1..3)),
        maybe(given, list(entry(given), 1..4)),
    )
        .prop_map(|(name, canonical, metadata, entries)| {
            object([
                ("name", Some(name)),
                ("canonical", Some(canonical)),
                ("metadata", metadata),
                ("entries", entries),
            ])
        })
        .boxed()
}

// ---- Entries ------------------------------------------------------------

fn entry(given: f64) -> Gen {
    prop_oneof![
        4 => field(given),
        2 => array(given),
        1 => struct_entry(given),
        1 => padding(given),
        1 => checksum(given),
    ]
    .boxed()
}

fn scalar_type() -> Gen {
    word(&["uint", "int", "float", "bool", "string", "bytes"])
}

fn text_encoding() -> Gen {
    word(&["utf8", "ascii", "latin1"])
}

/// Sentinels: at least one, since an empty list is written absent.
fn sentinels() -> Gen {
    list(scalar(), 1..3)
}

fn field(given: f64) -> Gen {
    let place = (
        text(),
        scalar_type(),
        maybe(given, u32_value()),
        maybe(given, text()),
        maybe(given, u32_value()),
        maybe(given, byte_order()),
    );
    let reading = (
        maybe(given, byte_length(given)),
        maybe(given, text_encoding()),
        maybe(given, conversion(given)),
        maybe(given, text()),
        maybe(given, text()),
        maybe(given, sentinels()),
    );
    let writing = (
        maybe(given, condition()),
        maybe(given, boolean()),
        maybe(given, scalar()),
        maybe(given, text()),
        maybe(given, conversion(given)),
        maybe(given, boolean()),
    );
    (place, reading, writing)
        .prop_map(
            |(
                (name, ty, offset_bits, pointer, size_bits, byte_order),
                (length, text, conversion, unit, canonical, invalid),
                (when, optional, value, argument, inverse, target),
            )| {
                object([
                    ("kind", Some(json!("field"))),
                    ("name", Some(name)),
                    ("type", Some(ty)),
                    ("offset_bits", offset_bits),
                    ("pointer", pointer),
                    ("size_bits", size_bits),
                    ("byte_order", byte_order),
                    ("length", length),
                    ("text", text),
                    ("conversion", conversion),
                    ("unit", unit),
                    ("canonical", canonical),
                    ("invalid", invalid),
                    ("when", when),
                    ("optional", optional),
                    ("value", value),
                    ("argument", argument),
                    ("inverse", inverse),
                    ("target", target),
                ])
            },
        )
        .boxed()
}

fn element(given: f64) -> Gen {
    let scalar_element = (
        (
            scalar_type(),
            maybe(given, u32_value()),
            maybe(given, byte_order()),
            maybe(given, byte_length(given)),
            maybe(given, text_encoding()),
        ),
        (
            maybe(given, conversion(given)),
            maybe(given, text()),
            maybe(given, sentinels()),
            maybe(given, conversion(given)),
        ),
    )
        .prop_map(
            |((ty, size_bits, byte_order, length, text), (conversion, unit, invalid, inverse))| {
                object([
                    ("kind", Some(json!("scalar"))),
                    ("type", Some(ty)),
                    ("size_bits", size_bits),
                    ("byte_order", byte_order),
                    ("length", length),
                    ("text", text),
                    ("conversion", conversion),
                    ("unit", unit),
                    ("invalid", invalid),
                    ("inverse", inverse),
                ])
            },
        );
    prop_oneof![
        scalar_element,
        text().prop_map(|name| json!({ "kind": "struct", "name": name })),
    ]
    .boxed()
}

fn array(given: f64) -> Gen {
    (
        (
            text(),
            maybe(given, u32_value()),
            maybe(given, text()),
            maybe(given, count(given)),
        ),
        element(given),
        (
            maybe(given, text()),
            maybe(given, text()),
            maybe(given, finite()),
            maybe(given, condition()),
            maybe(given, boolean()),
        ),
    )
        .prop_map(
            |(
                (name, offset_bits, pointer, count),
                element,
                (canonical, argument, sample_interval_ms, when, optional),
            )| {
                object([
                    ("kind", Some(json!("array"))),
                    ("name", Some(name)),
                    ("offset_bits", offset_bits),
                    ("pointer", pointer),
                    ("count", count),
                    ("element", Some(element)),
                    ("canonical", canonical),
                    ("argument", argument),
                    ("sample_interval_ms", sample_interval_ms),
                    ("when", when),
                    ("optional", optional),
                ])
            },
        )
        .boxed()
}

fn struct_entry(given: f64) -> Gen {
    (
        text(),
        text(),
        maybe(given, u32_value()),
        maybe(given, text()),
        maybe(given, condition()),
        maybe(given, boolean()),
    )
        .prop_map(
            |(name, struct_type, offset_bits, pointer, when, optional)| {
                object([
                    ("kind", Some(json!("struct"))),
                    ("name", Some(name)),
                    ("struct", Some(struct_type)),
                    ("offset_bits", offset_bits),
                    ("pointer", pointer),
                    ("when", when),
                    ("optional", optional),
                ])
            },
        )
        .boxed()
}

fn padding(given: f64) -> Gen {
    (
        u32_value(),
        maybe(given, u32_value()),
        maybe(given, condition()),
    )
        .prop_map(|(size_bits, offset_bits, when)| {
            object([
                ("kind", Some(json!("padding"))),
                ("size_bits", Some(size_bits)),
                ("offset_bits", offset_bits),
                ("when", when),
            ])
        })
        .boxed()
}

fn checksum(given: f64) -> Gen {
    let algorithm = prop_oneof![
        (
            u8_value(),
            scalar(),
            scalar(),
            boolean(),
            boolean(),
            scalar()
        )
            .prop_map(
                |(width, poly, init, reflect_in, reflect_out, xor_out)| json!({
                    "kind": "crc", "width": width, "poly": poly, "init": init,
                    "reflect_in": reflect_in, "reflect_out": reflect_out, "xor_out": xor_out
                })
            ),
        u8_value().prop_map(|width| json!({ "kind": "sum", "width": width })),
        bare("xor"),
        bare("iso8473"),
    ]
    .boxed();
    (
        text(),
        maybe(given, u32_value()),
        maybe(given, u32_value()),
        algorithm,
    )
        .prop_map(|(name, offset_bits, from_bits, algorithm)| {
            object([
                ("kind", Some(json!("checksum"))),
                ("name", Some(name)),
                ("offset_bits", offset_bits),
                ("from_bits", from_bits),
                ("algorithm", Some(algorithm)),
            ])
        })
        .boxed()
}

/// A length or count taken from another field.
fn from_field(given: f64) -> Gen {
    (text(), maybe(given, u32_value()), maybe(given, safe_int()))
        .prop_map(|(field, multiply, add)| {
            object([
                ("kind", Some(json!("field"))),
                ("field", Some(field)),
                ("multiply", multiply),
                ("add", add),
            ])
        })
        .boxed()
}

fn byte_length(given: f64) -> Gen {
    prop_oneof![
        (u32_value(), maybe(given, u8_value())).prop_map(|(bytes, terminator)| {
            object([
                ("kind", Some(json!("fixed"))),
                ("bytes", Some(bytes)),
                ("terminator", terminator),
            ])
        }),
        from_field(given),
        u8_value().prop_map(|byte| json!({ "kind": "terminator", "byte": byte })),
        bare("remaining"),
    ]
    .boxed()
}

fn count(given: f64) -> Gen {
    prop_oneof![
        u32_value().prop_map(|count| json!({ "kind": "fixed", "count": count })),
        from_field(given),
        bare("remaining"),
    ]
    .boxed()
}

// ---- Conversions and conditions ----------------------------------------

fn conversion(given: f64) -> Gen {
    let point = (finite(), finite())
        .prop_map(|(raw, value)| json!({ "raw": raw, "value": value }))
        .boxed();
    let state = (scalar(), text())
        .prop_map(|(raw, name)| json!({ "raw": raw, "name": name }))
        .boxed();
    prop_oneof![
        (finite(), finite()).prop_map(|(scale, offset)| {
            json!({ "kind": "linear", "scale": scale, "offset": offset })
        }),
        list(finite(), 0..4)
            .prop_map(|coefficients| { json!({ "kind": "poly", "coefficients": coefficients }) }),
        list(state, 0..3).prop_map(|states| json!({ "kind": "states", "states": states })),
        (
            list(point.clone(), 0..4),
            maybe(given, word(&["linear", "step"])),
            maybe(given, boolean())
        )
            .prop_map(|(points, interpolation, extrapolate)| {
                object([
                    ("kind", Some(json!("table"))),
                    ("points", Some(points)),
                    ("interpolation", interpolation),
                    ("extrapolate", extrapolate),
                ])
            }),
        (list(point, 0..4), maybe(given, boolean())).prop_map(|(points, extrapolate)| {
            object([
                ("kind", Some(json!("spline"))),
                ("points", Some(points)),
                ("extrapolate", extrapolate),
            ])
        }),
    ]
    .boxed()
}

fn condition() -> Gen {
    let op = word(&["eq", "ne", "lt", "le", "gt", "ge", "mask_any", "mask_all"]);
    let leaf = prop_oneof![
        (text(), op, scalar()).prop_map(|(field, op, value)| {
            json!({ "kind": "compare", "field": field, "op": op, "value": value })
        }),
        (text(), list(scalar(), 0..3)).prop_map(|(field, values)| {
            json!({ "kind": "in", "field": field, "values": values })
        }),
    ];
    leaf.prop_recursive(3, 12, 3, |inner| {
        prop_oneof![
            list(inner.clone().boxed(), 0..3)
                .prop_map(|conditions| json!({ "kind": "all", "conditions": conditions })),
            list(inner.clone().boxed(), 0..3)
                .prop_map(|conditions| json!({ "kind": "any", "conditions": conditions })),
            inner.prop_map(|condition| json!({ "kind": "not", "condition": condition })),
        ]
    })
    .boxed()
}

#[cfg(test)]
mod tests;
