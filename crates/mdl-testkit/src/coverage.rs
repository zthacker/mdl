//! The features of the IR a bundle uses, by name, and the list of every
//! feature there is ([`REQUIRED`]). The full fixture must use them all
//! (`crates/mdl/tests/coverage.rs`), and so must the generators.
//!
//! When the IR gains a kind or an option, add it to `REQUIRED`, to the
//! fixture, and to the generators.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

/// Every feature, as `features` names them: `holder:kind` for each kind of
/// tagged object, `key:holder.name` for each key worth naming, and each
/// enumerated value by what it enumerates.
pub const REQUIRED: &[&str] = &[
    // Entries and their elements.
    "entry:field",
    "entry:array",
    "entry:struct",
    "entry:padding",
    "entry:checksum",
    "element:scalar",
    "element:struct",
    "field_type:uint",
    "field_type:int",
    "field_type:float",
    "field_type:bool",
    "field_type:string",
    "field_type:bytes",
    // UTF-8 is the default text encoding, which the IR refuses to spell.
    "text:ascii",
    "text:latin1",
    "key:entry.byte_order",
    "key:entry.offset_bits",
    "key:entry.pointer",
    "key:entry.optional",
    "key:entry.invalid",
    "key:entry.when",
    "key:entry.sample_interval_ms",
    "key:entry.canonical",
    "key:entry.unit",
    "key:entry.conversion",
    "key:entry.text",
    "key:entry.value",
    "key:entry.argument",
    "key:entry.inverse",
    "key:entry.target",
    "key:entry.from_bits",
    "key:element.byte_order",
    "key:element.length",
    "key:element.text",
    "key:element.conversion",
    "key:element.unit",
    "key:element.invalid",
    "key:element.inverse",
    "scalar:string_integer",
    // Lengths and counts.
    "length:exact",
    "length:at_least",
    "length:fixed",
    "length:fixed/terminator",
    "length:field",
    "length:terminator",
    "length:remaining",
    "count:fixed",
    "count:field",
    "count:remaining",
    "key:length.multiply",
    "key:length.add",
    "key:count.add",
    // Conditions.
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
    // Conversions, and the inverses commands encode through.
    "conversion:linear",
    "conversion:poly",
    "conversion:states",
    "conversion:table",
    "conversion:table/step",
    "conversion:table/extrapolate",
    "conversion:spline",
    "conversion:spline/extrapolate",
    "inverse:linear",
    "inverse:poly",
    // Checksums.
    "checksum:crc",
    "checksum:crc/reflected",
    "checksum:sum",
    "checksum:xor",
    "checksum:iso8473",
    // Families.
    "encoding:binary",
    "encoding:json",
    "byte_order:big",
    "byte_order:little",
    "locator:bits",
    "locator:metadata",
    "locator:path",
    "locator:fixed",
    "key:family.metadata",
    "key:family.version",
    // Sets and packets.
    "key:set.version",
    "key:set.types",
    "key:set.time",
    "key:set.commands",
    "key:packet.identify",
    "key:packet.composites",
    "key:packet.time_field",
    // Time.
    "time:cuc",
    "time:iso8601",
    "time:boot",
    "time:boot/correlate",
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
    // The catalog.
    "param_type:f64",
    "param_type:i64",
    "param_type:u64",
    "param_type:bool",
    "param_type:str",
    "param_type:bytes",
    "param_type:enum",
    "param_type:vec3",
    "param_type:quat",
    "frame:ECEF",
    "frame:ECI_J2000",
    "frame:BODY",
    "frame:NED",
    "frame:ENU",
    "frame:GEODETIC",
    "key:param.unit",
    "key:param.frame",
    "key:param.description",
    "key:param.deprecated",
    "key:limits.red_low",
    "key:limits.yellow_low",
    "key:limits.yellow_high",
    "key:limits.red_high",
    "key:command.description",
    "key:command.deprecated",
    "key:argument.unit",
    "key:argument.states",
    "key:argument.range",
    "key:argument.list",
    "key:argument.description",
    "default:bool",
    "default:int",
    "default:float",
    "default:str",
    "default:list",
    // Command packets.
    "key:command_packet.metadata",
];

/// The features `value`, a bundle, uses.
pub fn features(value: &Value) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    walk(value, "", "", &mut found);
    found
}

/// What the objects under `key` are, given what holds them.
fn holder(key: &str, owner: &str) -> String {
    match key {
        "" => "bundle",
        "families" => "family",
        "sets" => "set",
        "packets" => "packet",
        "commands" if owner == "set" => "command_packet",
        "arguments" => "argument",
        "composites" => "composite",
        "entries" => "entry",
        "when" | "condition" | "conditions" => "condition",
        "algorithm" => "checksum",
        "alias" | "version" => "locator",
        "points" => "point",
        other => other,
    }
    .to_string()
}

/// The holder of a map's values, for maps keyed by names people choose,
/// whose keys say nothing about the features in use.
fn name_map(key: &str, owner: &str) -> Option<&'static str> {
    match (key, owner) {
        ("params", "catalog") => Some("param"),
        ("commands", "catalog") => Some("command"),
        ("types", "set") => Some("struct_type"),
        ("identify", "packet") => Some("identify"),
        ("metadata", "command_packet") => Some("metadata"),
        _ => None,
    }
}

fn walk(value: &Value, key: &str, owner: &str, found: &mut BTreeSet<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                walk(item, key, owner, found);
            }
        }
        Value::Object(fields) => match name_map(key, owner) {
            Some(member) => {
                for inner in fields.values() {
                    scalar(inner, found);
                    if let Value::Object(fields) = inner {
                        walk_fields(fields, member, found);
                    }
                }
            }
            None => {
                let what = holder(key, owner);
                if let Some(kind) = fields.get("kind").and_then(Value::as_str) {
                    tagged(fields, &what, kind, found);
                }
                walk_fields(fields, &what, found);
            }
        },
        _ => {}
    }
}

fn walk_fields(fields: &Map<String, Value>, what: &str, found: &mut BTreeSet<String>) {
    for (name, inner) in fields {
        note(name, inner, what, found);
        walk(inner, name, what, found);
    }
}

/// A tagged object's kind, and the options worth naming with it.
fn tagged(fields: &Map<String, Value>, what: &str, kind: &str, found: &mut BTreeSet<String>) {
    found.insert(format!("{what}:{kind}"));
    if fields.get("interpolation").and_then(Value::as_str) == Some("step") {
        found.insert(format!("{what}:{kind}/step"));
    }
    if fields.get("extrapolate") == Some(&Value::Bool(true)) {
        found.insert(format!("{what}:{kind}/extrapolate"));
    }
    if fields.get("reflect_in") == Some(&Value::Bool(true))
        || fields.get("reflect_out") == Some(&Value::Bool(true))
    {
        found.insert(format!("{what}:{kind}/reflected"));
    }
    for option in ["terminator", "week_bits", "correlate"] {
        if fields.contains_key(option) {
            found.insert(format!("{what}:{kind}/{option}"));
        }
    }
}

/// Whether a value says anything: not null, false or empty.
fn given(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(false) => false,
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
        _ => true,
    }
}

/// An integer past 2^53 - 1 where the IR holds a scalar is written as a
/// string, which canonical JSON keeps exact.
fn scalar(value: &Value, found: &mut BTreeSet<String>) {
    const SAFE: i128 = (1 << 53) - 1;
    let past_safe = |text: &str| text.parse::<i128>().is_ok_and(|n| n.abs() > SAFE);
    let strings: Vec<&str> = match value {
        Value::String(text) => vec![text.as_str()],
        Value::Array(items) => items.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if strings.into_iter().any(past_safe) {
        found.insert("scalar:string_integer".to_string());
    }
}

/// What one key of an object held by `owner` says about the features in
/// use.
fn note(name: &str, value: &Value, owner: &str, found: &mut BTreeSet<String>) {
    if given(value) && name != "kind" && name != "name" {
        found.insert(format!("key:{owner}.{name}"));
    }
    if matches!(name, "invalid" | "value" | "values" | "raw") {
        scalar(value, found);
    }
    let Some(text) = value.as_str() else {
        if name == "default" {
            let literal = match value {
                Value::Bool(_) => "bool",
                Value::Number(n) if n.is_i64() || n.is_u64() => "int",
                Value::Number(_) => "float",
                Value::Array(_) => "list",
                _ => "other",
            };
            found.insert(format!("default:{literal}"));
        }
        return;
    };
    let enumerated = match (name, owner) {
        ("type", "entry" | "element") => "field_type",
        ("type", "param" | "argument") => "param_type",
        ("unit", "time") | ("clock_unit", _) => "time_unit",
        ("default", _) => "default",
        ("op" | "scale" | "frame" | "text" | "encoding" | "byte_order", _) => name,
        _ => return,
    };
    let text = if enumerated == "default" { "str" } else { text };
    found.insert(format!("{enumerated}:{text}"));
}

#[cfg(test)]
mod tests;
