//! A case's bundle as IR JSON: the catalog's commands, the family and the
//! set.

use serde_json::{Map, Value, json};

use super::{
    Arg, ArgType, Commands, Const, Encoder, Layout, Len, Placed, Request, Shape, Source, Through,
};
use crate::layout::{Text, conversion_json, integer_json};

/// The case: the bundle the packets make, and their requests.
pub(super) fn assemble(
    little: bool,
    metadata: bool,
    layouts: &[Layout],
    requests: Vec<Vec<Request>>,
) -> Commands {
    let mut commands = Map::new();
    for layout in layouts {
        let arguments: Vec<Value> = layout
            .arguments
            .iter()
            .enumerate()
            .map(|(index, (encoder, _))| argument_json(index, encoder))
            .collect();
        commands.insert(layout.command.clone(), json!({ "arguments": arguments }));
    }
    let packets: Vec<Value> = layouts.iter().map(packet_json).collect();
    let mut family = json!({
        "name": "fam",
        "encoding": "binary",
        "byte_order": if little { "little" } else { "big" },
        "alias": { "kind": "fixed", "value": "A1" }
    });
    if metadata {
        family["metadata"] = json!(["apid"]);
    }
    let bundle = json!({
        "mdl_version": "2.0",
        "catalog": { "commands": commands },
        "families": [family],
        "sets": [{ "name": "fam_v1", "family": "fam", "commands": packets }]
    });
    Commands {
        bundle,
        family: "fam".into(),
        set: "fam_v1".into(),
        requests: requests.into_iter().flatten().collect(),
    }
}

fn argument_json(index: usize, encoder: &Encoder) -> Value {
    let mut json = Map::new();
    json.insert("name".into(), json!(format!("a{index}")));
    json.insert("type".into(), json!(encoder.ty.name()));
    if let Some((unit, _)) = encoder.units {
        json.insert("unit".into(), json!(unit));
    }
    if let ArgType::Enum(states) = &encoder.ty {
        json.insert("states".into(), json!(states));
    }
    if encoder.min.is_some() || encoder.max.is_some() {
        let mut range = Map::new();
        if let Some(min) = encoder.min {
            range.insert("min".into(), json!(min));
        }
        if let Some(max) = encoder.max {
            range.insert("max".into(), json!(max));
        }
        json.insert("range".into(), Value::Object(range));
    }
    if let Some(default) = &encoder.default {
        json.insert("default".into(), arg_json(default));
    }
    Value::Object(json)
}

/// A value as the catalog writes it: bytes as lowercase hex, an integer
/// past ±(2^53 − 1) as a decimal string.
fn arg_json(value: &Arg) -> Value {
    match value {
        Arg::Bool(b) => json!(b),
        Arg::U64(n) => integer_json(i128::from(*n)),
        Arg::I64(n) => integer_json(i128::from(*n)),
        Arg::F64(x) => json!(x),
        Arg::Text(text) => json!(text),
        Arg::Bytes(bytes) => json!(
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ),
    }
}

fn packet_json(layout: &Layout) -> Value {
    let entries: Vec<Value> = layout
        .entries
        .iter()
        .map(|entry| entry_json(layout, entry))
        .collect();
    let mut json = json!({ "name": layout.name, "canonical": layout.command, "entries": entries });
    if let Some(apid) = layout.apid {
        json["metadata"] = json!({ "apid": apid });
    }
    json
}

fn entry_json(layout: &Layout, entry: &Placed) -> Value {
    let (name, shape, little, explicit, source) = match entry {
        Placed::Padding(bits) => return json!({ "kind": "padding", "size_bits": bits }),
        Placed::Checksum {
            name,
            algorithm,
            from_bits,
        } => {
            let mut json =
                json!({ "kind": "checksum", "name": name, "algorithm": algorithm.json() });
            if *from_bits > 0 {
                json["from_bits"] = json!(from_bits);
            }
            return json;
        }
        Placed::Field {
            name,
            shape,
            little,
            explicit,
            source,
        } => (name, shape, little, explicit, source),
    };
    let mut json = Map::new();
    json.insert("kind".into(), json!("field"));
    json.insert("name".into(), json!(name));
    json.insert("type".into(), json!(shape.type_name()));
    if let Some(bits) = shape.bits()
        && !matches!(shape, Shape::Text(..) | Shape::Bytes(_))
    {
        json.insert("size_bits".into(), json!(bits));
    }
    if let Some(len) = shape.len() {
        json.insert("length".into(), length_json(len, name));
    }
    match shape {
        Shape::Text(Text::Ascii, _) => {
            json.insert("text".into(), json!("ascii"));
        }
        Shape::Text(Text::Latin1, _) => {
            json.insert("text".into(), json!("latin1"));
        }
        _ => {}
    }
    if *explicit {
        json.insert(
            "byte_order".into(),
            json!(if *little { "little" } else { "big" }),
        );
    }
    match source {
        Source::Constant(value) => {
            let value = match value {
                Const::Int(n) => integer_json(*n),
                Const::Bool(b) => json!(u8::from(*b)),
                Const::Text(text) => json!(text),
            };
            json.insert("value".into(), value);
        }
        Source::Argument(index) => {
            let encoder = &layout.arguments[*index].0;
            json.insert("argument".into(), json!(format!("a{index}")));
            match &encoder.through {
                Through::Plain => {}
                Through::States(states) => {
                    let states: Vec<Value> = states
                        .iter()
                        .map(|(raw, name)| json!({ "raw": integer_json(*raw), "name": name }))
                        .collect();
                    json.insert(
                        "conversion".into(),
                        json!({ "kind": "states", "states": states }),
                    );
                }
                Through::Exact(conversion) => {
                    if let Some(conversion) = conversion_json(conversion) {
                        json.insert("conversion".into(), conversion);
                    }
                }
                Through::Inverse {
                    conversion,
                    inverse,
                } => {
                    if let Some(conversion) = conversion_json(conversion) {
                        json.insert("conversion".into(), conversion);
                    }
                    if let Some(inverse) = conversion_json(inverse) {
                        json.insert("inverse".into(), inverse);
                    }
                }
            }
            if let Some((_, unit)) = encoder.units {
                json.insert("unit".into(), json!(unit));
            }
        }
        Source::Target => {
            json.insert("target".into(), json!(true));
        }
        Source::Length(_) => {}
    }
    Value::Object(json)
}

/// A length as IR JSON; a length field is named after the entry it serves.
fn length_json(len: &Len, name: &str) -> Value {
    match len {
        Len::Fixed(bytes) => json!({ "kind": "fixed", "bytes": bytes }),
        Len::Padded(bytes, byte) => json!({ "kind": "fixed", "bytes": bytes, "terminator": byte }),
        Len::Terminator(byte) => json!({ "kind": "terminator", "byte": byte }),
        Len::Field { multiply, add, .. } => {
            let mut json = json!({ "kind": "field", "field": format!("{name}_len") });
            if *multiply != 1 {
                json["multiply"] = json!(multiply);
            }
            if *add != 0 {
                json["add"] = json!(add);
            }
            json
        }
        Len::Remaining => json!({ "kind": "remaining" }),
    }
}
