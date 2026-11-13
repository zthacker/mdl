//! JSON families (`spec/decoding.md` §2.3, §3.1, §4, §5.3, §8): telemetry
//! whose payloads are JSON documents, each entry read at a JSON Pointer
//! relative to its container (the document, a struct's object, or a
//! record element). The alias is fixed or read at a pointer; packets are
//! told apart by pointer keys, the most specific match winning; the event
//! time is an RFC 3339 string, or a count in a time field.
//!
//! Values are drawn as the binary oracle draws them, with the meanings
//! they have there, and the record and samples are worked out the same
//! way. The document is written by [`doc`], in any of JSON's spellings for
//! each value, with members in any order and members no entry reads.

use std::collections::BTreeMap;

use proptest::collection::{btree_set, vec};
use proptest::prelude::*;
use proptest::sample::select;
use proptest::strategy::Union;
use serde_json::{Map, Value, json};

use super::condition::{Condition, Op, Operand, Selector, condition_json, leaning, when_spec};
use super::meaning::sample;
use super::time::{
    Count, EventTime, SECOND, Scale, TimeFields, TimeSource, count_value, epoch, event_time,
    instant, label, no_reading, received, rfc3339, rfc3339_form, time_fields, time_json,
    time_scale, unit, week_value,
};
use super::{
    Encoding, FieldSpec, Kind, Length, Meaning, Message, Placed, Raw, Sample, Sentinel, Telemetry,
    Text, Unusable, interval, letters, meaning, meaning_json, offset_ns, param_json, raw_value,
    sentinels, type_name,
};
use doc::{Doc, Spell};

mod doc;
pub(super) mod faults;
#[cfg(test)]
mod tests;

/// The most elements a JSON array has in a message.
const MOST_ELEMENTS: usize = 5;

/// A string field's kind: JSON text is UTF-8, of any length.
const TEXT: Kind = Kind::Text {
    length: Length::Remaining,
    text: Text::Utf8,
};

/// Any JSON telemetry layout validation accepts, of the kinds this module
/// builds, with one to three messages for each packet.
pub fn json_telemetry() -> BoxedStrategy<Telemetry> {
    case(message)
}

/// Layouts as [`json_telemetry`] makes them, with messages damaged as
/// `layout/json/faults.rs` plants faults in them: each fails with its
/// [`Damage::failure`](super::Damage::failure).
pub fn json_damaged() -> BoxedStrategy<Telemetry> {
    case(faults::damaged_message)
}

/// A JSON family and its set, with one to three messages from `message`
/// for each packet.
fn case(message: fn(JsonSet, usize) -> BoxedStrategy<Message>) -> BoxedStrategy<Telemetry> {
    set()
        .prop_flat_map(move |set| {
            let messages: Vec<BoxedStrategy<Vec<Message>>> = (0..set.packets.len())
                .map(|index| vec(message(set.clone(), index), 1..=3).boxed())
                .collect();
            (Just(set), messages)
        })
        .prop_map(|(set, messages)| assemble(&set, messages.into_iter().flatten().collect()))
        .boxed()
}

// ---- What a set is ----------------------------------------------------------

/// A JSON family's one set, laid out.
#[derive(Debug, Clone)]
struct JsonSet {
    alias: Alias,
    time: JsonTime,
    packets: Vec<JsonPacket>,
    /// Its struct types, for the set.
    types: Vec<JsonType>,
}

/// Where a JSON family's alias is (§3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Alias {
    /// A fixed alias, `A1`.
    Fixed,
    /// The string `A1` at `/sat`.
    Text,
    /// The integer 17 at `/sat`, in any spelling of it, or the text
    /// `"17"`, which is that integer: the native id `17` (§3.2).
    Number,
}

/// A JSON set's time source (§8).
#[derive(Debug, Clone)]
enum JsonTime {
    /// None, `elapsed` or `gps`: counts in each packet's time fields.
    Fields(TimeSource),
    /// `iso8601`: an RFC 3339 instant at `/t`.
    Iso8601,
}

/// A JSON packet, laid out.
#[derive(Debug, Clone)]
struct JsonPacket {
    name: String,
    /// Its identify keys: each a member of the document, and the value it
    /// holds there (§4). None for the catch-all.
    keys: Vec<(String, Key)>,
    /// Whether a field `kind` reads its first key too, as JSON packets
    /// often name their kind in a member a consumer reads.
    kind_field: bool,
    /// Its entries after its time fields and `kind`, with pointers
    /// relative to the document.
    entries: Vec<JsonEntry>,
    /// Its time fields, `t` and a GPS `week`, where its source counts.
    timing: TimeFields,
}

/// An identify key's value (§4): a string, or an integer in any of its
/// spellings.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Key {
    Text(String),
    Int(i64),
}

/// An entry of a packet or struct type, laid out.
#[derive(Debug, Clone)]
struct JsonEntry {
    name: String,
    /// Its pointer's reference tokens, relative to its container, as
    /// member names (RFC 6901 escapes undone).
    tokens: Vec<String>,
    /// Absent when nothing, or `null`, is at its pointer (§5.3).
    optional: bool,
    /// Present only when this holds (§5.4).
    when: Option<Condition>,
    node: JsonNode,
}

#[derive(Debug, Clone)]
enum JsonNode {
    Field(Placed),
    Array {
        element: JsonElement,
        /// For mapped scalar elements, sometimes: milliseconds between
        /// samples.
        interval: Option<f64>,
    },
    Struct(JsonType),
}

#[derive(Debug, Clone)]
enum JsonElement {
    Scalar(Placed),
    Record(JsonType),
}

/// A struct type: its entries, with pointers relative to its object.
#[derive(Debug, Clone)]
struct JsonType {
    name: String,
    entries: Vec<JsonEntry>,
}

// ---- Laying a set out -----------------------------------------------------

/// An entry, before it's named.
#[derive(Debug, Clone)]
struct EntrySpec {
    shape: ShapeSpec,
    optional: bool,
    when: Option<super::condition::WhenSpec>,
    path: PathSpec,
}

#[derive(Debug, Clone)]
enum ShapeSpec {
    Field(FieldSpec),
    Array {
        element: ElementSpec,
        interval: Option<f64>,
    },
    Struct(Vec<EntrySpec>),
}

#[derive(Debug, Clone)]
enum ElementSpec {
    Scalar(FieldSpec),
    Record(Vec<EntrySpec>),
}

/// How an entry's pointer goes.
#[derive(Debug, Clone, Copy)]
enum PathSpec {
    /// One token: its name.
    Plain,
    /// One token: its name and one of [`ODD`].
    Odd(usize),
    /// Two tokens: its name, inside an object of its own.
    Nested,
}

/// Endings a member name may have that a pointer escapes (`/` as `~1`,
/// `~` as `~0`), or that look like an escape (`~1` written as `~01`), or
/// that are text beyond ASCII.
const ODD: [&str; 6] = ["/x", "~x", "~1", "a b", "é", "\u{1F600}"];

/// A packet, before it's laid out: its entries, time fields, whether a
/// field reads its kind, and whether it shares the packet before's kind
/// and adds a `sub` key.
type PacketSpec = (Vec<EntrySpec>, TimeFields, bool, bool);

fn set() -> BoxedStrategy<JsonSet> {
    (
        alias(),
        json_time(),
        any::<bool>(),
        proptest::bool::weighted(0.25),
    )
        .prop_flat_map(|(alias, time, numbered, catch_all)| {
            let fields = match &time {
                JsonTime::Fields(source) => time_fields(source),
                JsonTime::Iso8601 => Just(TimeFields::default()).boxed(),
            };
            let packet = (
                vec(entry_spec(2), 1..=4),
                fields,
                any::<bool>(),
                proptest::bool::weighted(0.4),
            );
            (
                Just(alias),
                Just(time),
                Just(numbered),
                Just(catch_all),
                vec(packet, 1..=3),
            )
        })
        .prop_map(|(alias, time, numbered, catch_all, packets)| {
            lay_out(alias, time, numbered, catch_all, packets)
        })
        .boxed()
}

fn alias() -> BoxedStrategy<Alias> {
    select(&[Alias::Fixed, Alias::Text, Alias::Number][..]).boxed()
}

fn json_time() -> BoxedStrategy<JsonTime> {
    prop_oneof![
        1 => Just(JsonTime::Fields(TimeSource::None)),
        2 => Just(JsonTime::Iso8601),
        2 => (unit(), epoch(), time_scale()).prop_map(|(unit, epoch, scale)| {
            JsonTime::Fields(TimeSource::Elapsed { unit, epoch, scale })
        }),
        1 => (unit(), proptest::option::of(select(&[10_u32, 13, 16][..])))
            .prop_map(|(unit, week_bits)| JsonTime::Fields(TimeSource::Gps { unit, week_bits })),
    ]
    .boxed()
}

/// An entry of a packet or struct type: a field mostly, an array, or a
/// struct while `depth` allows another level; optional now and then, or
/// under a condition (§5.4), and at a pointer of one or two tokens.
fn entry_spec(depth: u32) -> BoxedStrategy<EntrySpec> {
    let field = json_field_spec().prop_map(ShapeSpec::Field).boxed();
    let array = (
        element_spec(depth),
        proptest::option::weighted(0.4, interval()),
    )
        .prop_map(|(element, interval)| ShapeSpec::Array { element, interval })
        .boxed();
    let mut shapes = vec![(6, field), (3, array)];
    if depth > 0 {
        let record = vec(entry_spec(depth - 1), 1..=3)
            .prop_map(ShapeSpec::Struct)
            .boxed();
        shapes.push((2, record));
    }
    let path = prop_oneof![
        5 => Just(PathSpec::Plain),
        2 => (0..ODD.len()).prop_map(PathSpec::Odd),
        2 => Just(PathSpec::Nested),
    ];
    (
        Union::new_weighted(shapes),
        proptest::bool::weighted(0.25),
        proptest::option::weighted(0.2, when_spec()),
        path,
    )
        .prop_map(|(shape, optional, when, path)| EntrySpec {
            shape,
            optional: optional && when.is_none(),
            when,
            path,
        })
        .boxed()
}

fn element_spec(depth: u32) -> BoxedStrategy<ElementSpec> {
    let scalar = json_field_spec().prop_map(ElementSpec::Scalar);
    if depth == 0 {
        return scalar.boxed();
    }
    prop_oneof![
        2 => scalar,
        1 => vec(entry_spec(depth - 1), 1..=3).prop_map(ElementSpec::Record),
    ]
    .boxed()
}

/// A JSON field: any type JSON gives (§5.3), with any meaning and
/// sentinels a field of it can have, mapped most of the time.
fn json_field_spec() -> BoxedStrategy<FieldSpec> {
    let kinds = prop_oneof![
        3 => Just(Kind::Uint(64)),
        3 => Just(Kind::Int(64)),
        2 => Just(Kind::Float(64)),
        1 => Just(Kind::Bool),
        2 => Just(TEXT),
    ];
    (kinds, proptest::bool::weighted(0.85))
        .prop_flat_map(|(kind, mapped)| {
            (
                json_meaning(&kind),
                json_sentinels(&kind),
                Just(kind),
                Just(mapped),
            )
        })
        .prop_map(|(meaning, invalid, kind, mapped)| FieldSpec {
            kind,
            meaning,
            invalid,
            flip: false,
            mapped,
        })
        .boxed()
}

/// A JSON field's meaning: as a binary field of its kind has, and for a
/// string, states of letters too.
fn json_meaning(kind: &Kind) -> BoxedStrategy<Meaning> {
    match kind {
        Kind::Text { .. } => prop_oneof![
            Just(Meaning::Str),
            btree_set(letters(kind), 1..=3)
                .prop_map(|names| Meaning::TextState(names.into_iter().collect())),
        ]
        .boxed(),
        _ => meaning(kind),
    }
}

/// A JSON field's sentinels: as a binary field of its kind has, and for a
/// string, letters.
fn json_sentinels(kind: &Kind) -> BoxedStrategy<Vec<Sentinel>> {
    match kind {
        Kind::Text { .. } => btree_set(letters(kind).prop_map(Sentinel::Text), 0..=2)
            .prop_map(|set| set.into_iter().collect())
            .boxed(),
        _ => sentinels(kind),
    }
}

/// How a set's entries, params and struct types are named, and its types
/// so far.
struct Names {
    packet: String,
    params: String,
    types: Vec<JsonType>,
}

/// The set: packet `i` named `J<i>`, its params `j<i>.<entry>`. Each
/// keyed packet's `kind` is `K<i>`, or the integer `10 + i` if
/// `numbered`; one that nests shares the packet before's kind (when that
/// one has only it) and adds `sub`, the integer `i`, so it's the more
/// specific of the two (§4). With `catch_all` the first packet has no
/// keys.
fn lay_out(
    alias: Alias,
    time: JsonTime,
    numbered: bool,
    catch_all: bool,
    specs: Vec<PacketSpec>,
) -> JsonSet {
    let mut packets: Vec<JsonPacket> = Vec::new();
    let mut types = Vec::new();
    for (index, (entries, timing, kind_field, nests)) in specs.into_iter().enumerate() {
        let name = format!("J{index}");
        let mut names = Names {
            packet: name.clone(),
            params: format!("j{index}"),
            types: Vec::new(),
        };
        let entries = lay_out_entries(entries, &format!("j{index}"), &mut names);
        types.append(&mut names.types);
        let previous = packets.last().filter(|packet| packet.keys.len() == 1);
        let keys = match previous {
            _ if catch_all && index == 0 => Vec::new(),
            Some(previous) if nests => {
                let sub = i64::try_from(index).unwrap_or(0);
                vec![previous.keys[0].clone(), ("sub".to_string(), Key::Int(sub))]
            }
            _ if numbered => vec![("kind".to_string(), Key::Int(10 + index as i64))],
            _ => vec![("kind".to_string(), Key::Text(format!("K{index}")))],
        };
        packets.push(JsonPacket {
            name,
            kind_field: kind_field && !keys.is_empty(),
            keys,
            entries,
            timing,
        });
    }
    JsonSet {
        alias,
        time,
        packets,
        types,
    }
}

/// A container's entries, named `f0`, `f1` and on, each conditional one
/// after its selector (`<entry>_sel`), or the container's last selector of
/// its kind where it shares that.
fn lay_out_entries(specs: Vec<EntrySpec>, params: &str, names: &mut Names) -> Vec<JsonEntry> {
    let mut entries: Vec<JsonEntry> = Vec::new();
    let mut selectors: Vec<(String, Selector)> = Vec::new();
    for (position, spec) in specs.into_iter().enumerate() {
        let name = format!("f{position}");
        let when = spec.when.map(|when| {
            let shared = selectors
                .last()
                .filter(|(_, kind)| when.share && *kind == when.selector)
                .map(|(field, _)| field.clone());
            let field = match shared {
                Some(field) => field,
                None => {
                    let field = format!("{name}_sel");
                    entries.push(JsonEntry {
                        name: field.clone(),
                        tokens: vec![field.clone()],
                        optional: false,
                        when: None,
                        node: JsonNode::Field(placed(&field, when.field, params)),
                    });
                    selectors.push((field.clone(), when.selector));
                    field
                }
            };
            Condition {
                field,
                test: when.test,
            }
        });
        let node = lay_out_node(spec.shape, &name, params, names);
        entries.push(JsonEntry {
            tokens: tokens(&name, spec.path),
            name,
            optional: spec.optional,
            when,
            node,
        });
    }
    entries
}

fn lay_out_node(shape: ShapeSpec, name: &str, params: &str, names: &mut Names) -> JsonNode {
    match shape {
        ShapeSpec::Field(spec) => JsonNode::Field(placed(name, spec, params)),
        ShapeSpec::Array { element, interval } => {
            let element = match element {
                ElementSpec::Scalar(spec) => JsonElement::Scalar(placed(name, spec, params)),
                ElementSpec::Record(specs) => JsonElement::Record(struct_type(specs, names)),
            };
            let mapped = matches!(&element, JsonElement::Scalar(field) if field.param.is_some());
            JsonNode::Array {
                element,
                interval: interval.filter(|_| mapped),
            }
        }
        ShapeSpec::Struct(specs) => JsonNode::Struct(struct_type(specs, names)),
    }
}

fn placed(name: &str, spec: FieldSpec, params: &str) -> Placed {
    Placed {
        name: name.to_string(),
        param: spec.mapped.then(|| format!("{params}.{name}")),
        kind: spec.kind,
        meaning: spec.meaning,
        invalid: spec.invalid,
        little: false,
        explicit: false,
    }
}

/// A struct type, named after its packet (`J0_t1`), its params after the
/// packet's (`j0.t1`), kept for the set.
fn struct_type(specs: Vec<EntrySpec>, names: &mut Names) -> JsonType {
    let index = names.types.len();
    let name = format!("{}_t{index}", names.packet);
    names.types.push(JsonType {
        name: name.clone(),
        entries: Vec::new(),
    });
    let params = format!("{}.t{index}", names.params);
    let entries = lay_out_entries(specs, &params, names);
    let ty = JsonType { name, entries };
    names.types[index] = ty.clone();
    ty
}

fn tokens(name: &str, path: PathSpec) -> Vec<String> {
    match path {
        PathSpec::Plain => vec![name.to_string()],
        PathSpec::Odd(k) => vec![format!("{name}{}", ODD[k % ODD.len()])],
        PathSpec::Nested => vec![format!("{name}_in"), name.to_string()],
    }
}

/// A pointer to `tokens` (RFC 6901): `~` written `~0` before `/` is
/// written `~1`.
fn pointer(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|token| format!("/{}", token.replace('~', "~0").replace('/', "~1")))
        .collect()
}

// ---- As IR JSON -------------------------------------------------------------

fn assemble(set: &JsonSet, messages: Vec<Message>) -> Telemetry {
    let mut params = Vec::new();
    for packet in &set.packets {
        params_of(&packet.entries, &mut params);
    }
    let params: Map<String, Value> = params.into_iter().collect();
    let packets: Vec<Value> = set
        .packets
        .iter()
        .map(|packet| packet_json(set, packet))
        .collect();
    let mut set_json = json!({ "name": "fam_v1", "family": "fam", "packets": packets });
    match &set.time {
        JsonTime::Fields(source) => {
            if let Some(time) = time_json(source, 0) {
                set_json["time"] = time;
            }
        }
        JsonTime::Iso8601 => set_json["time"] = json!({ "kind": "iso8601", "pointer": "/t" }),
    }
    if !set.types.is_empty() {
        let types: Map<String, Value> = set
            .types
            .iter()
            .map(|ty| {
                let entries: Vec<Value> = ty.entries.iter().map(entry_json).collect();
                (ty.name.clone(), json!({ "entries": entries }))
            })
            .collect();
        set_json["types"] = Value::Object(types);
    }
    let alias = match set.alias {
        Alias::Fixed => json!({ "kind": "fixed", "value": "A1" }),
        Alias::Text | Alias::Number => json!({ "kind": "path", "pointer": "/sat" }),
    };
    let bundle = json!({
        "mdl_version": "2.0",
        "catalog": { "params": params },
        "families": [{ "name": "fam", "encoding": "json", "alias": alias }],
        "sets": [set_json]
    });
    let native_id = match set.alias {
        Alias::Number => "17",
        Alias::Fixed | Alias::Text => "A1",
    };
    Telemetry {
        bundle,
        family: "fam".into(),
        set: "fam_v1".into(),
        native_id: native_id.into(),
        messages,
    }
}

fn packet_json(set: &JsonSet, packet: &JsonPacket) -> Value {
    let mut entries = Vec::new();
    if let (JsonTime::Fields(source), Some(count)) = (&set.time, packet.timing.count) {
        entries.extend(time_entries(source, count, &packet.timing));
    }
    if let (true, Some((_, key))) = (packet.kind_field, packet.keys.first()) {
        let ty = match key {
            Key::Text(_) => "string",
            Key::Int(_) => "int",
        };
        entries.push(json!({ "kind": "field", "name": "kind", "type": ty, "pointer": "/kind" }));
    }
    entries.extend(packet.entries.iter().map(entry_json));
    let mut json = json!({ "name": packet.name, "entries": entries });
    if !packet.keys.is_empty() {
        let identify: Map<String, Value> = packet
            .keys
            .iter()
            .map(|(name, key)| {
                let value = match key {
                    Key::Text(text) => json!(text),
                    Key::Int(n) => json!(n),
                };
                (format!("/{name}"), value)
            })
            .collect();
        json["identify"] = Value::Object(identify);
    }
    if packet.timing.count.is_some() {
        json["time_field"] = json!("t");
    }
    json
}

/// A packet's time fields as IR JSON: a GPS week, then the count, each
/// restating the unit its source counts it in where `units` says so.
fn time_entries(source: &TimeSource, count: Count, timing: &TimeFields) -> Vec<Value> {
    let mut entries = Vec::new();
    let unit = match source {
        TimeSource::Elapsed { unit, .. } | TimeSource::Gps { unit, .. } => unit.name(),
        _ => "s",
    };
    if matches!(source, TimeSource::Gps { .. }) {
        let mut week =
            json!({ "kind": "field", "name": "week", "type": "uint", "pointer": "/week" });
        if !timing.week_invalid.is_empty() {
            week["invalid"] = json!(timing.week_invalid);
        }
        if timing.units {
            week["unit"] = json!("wk");
        }
        entries.push(week);
    }
    let ty = match count {
        Count::Uint(_) => "uint",
        Count::Int(_) => "int",
        Count::Float(_) => "float",
    };
    let mut t = json!({ "kind": "field", "name": "t", "type": ty, "pointer": "/t" });
    if !timing.count_invalid.is_empty() {
        let invalid: Vec<Value> = timing
            .count_invalid
            .iter()
            .map(|n| super::integer_json(*n))
            .collect();
        t["invalid"] = json!(invalid);
    }
    if timing.units {
        t["unit"] = json!(unit);
    }
    entries.push(t);
    entries
}

fn entry_json(entry: &JsonEntry) -> Value {
    let scalar = |field: &Placed, kind: &str| {
        let mut json = Map::new();
        json.insert("kind".into(), json!(kind));
        json.insert("type".into(), json!(type_name(&field.kind)));
        meaning_json(field, &mut json);
        json
    };
    let mut json = match &entry.node {
        JsonNode::Field(field) => {
            let mut json = scalar(field, "field");
            json.insert("name".into(), json!(entry.name));
            if let Some(param) = &field.param {
                json.insert("canonical".into(), json!(param));
            }
            Value::Object(json)
        }
        JsonNode::Array { element, interval } => {
            let element_json = match element {
                JsonElement::Scalar(element) => Value::Object(scalar(element, "scalar")),
                JsonElement::Record(ty) => json!({ "kind": "struct", "name": ty.name }),
            };
            let mut json = json!({ "kind": "array", "name": entry.name, "element": element_json });
            if let JsonElement::Scalar(Placed {
                param: Some(param), ..
            }) = element
            {
                json["canonical"] = json!(param);
            }
            if let Some(interval) = interval {
                json["sample_interval_ms"] = json!(interval);
            }
            json
        }
        JsonNode::Struct(ty) => {
            json!({ "kind": "struct", "name": entry.name, "struct": ty.name })
        }
    };
    json["pointer"] = json!(pointer(&entry.tokens));
    if entry.optional {
        json["optional"] = json!(true);
    }
    if let Some(when) = &entry.when {
        json["when"] = condition_json(&when.field, &when.test);
    }
    json
}

fn params_of(entries: &[JsonEntry], params: &mut Vec<(String, Value)>) {
    let mut add = |field: &Placed| {
        if let Some(param) = &field.param {
            params.push((param.clone(), param_json(&field.meaning)));
        }
    };
    let mut inner = Vec::new();
    for entry in entries {
        match &entry.node {
            JsonNode::Field(field) => add(field),
            JsonNode::Array {
                element: JsonElement::Scalar(element),
                ..
            } => add(element),
            JsonNode::Array {
                element: JsonElement::Record(ty),
                ..
            }
            | JsonNode::Struct(ty) => inner.push(&ty.entries),
        }
    }
    for entries in inner {
        params_of(entries, params);
    }
}

// ---- Messages -----------------------------------------------------------

/// What a message holds for an entry.
#[derive(Debug, Clone)]
enum Fill {
    Scalar(Raw),
    /// An array's elements, `None` for a `null` one.
    Array(Vec<Option<Element>>),
    Struct(Vec<Fill>),
    /// An optional entry with nothing at its pointer, or `null` there.
    Absent {
        null: bool,
    },
    /// A conditional entry whose condition fails, with what its pointer
    /// finds, if anything: decoding doesn't read it.
    Skipped(Option<Doc>),
}

#[derive(Debug, Clone)]
enum Element {
    Scalar(Raw),
    Record(Vec<Fill>),
}

/// What `/t` holds for an `iso8601` source.
#[derive(Debug, Clone)]
enum Stamp {
    Text(String),
    Number(i64),
    Missing,
}

/// What a JSON message holds before it's written.
#[derive(Debug, Clone)]
struct JsonDraft {
    /// The packet's time fields: its count and GPS week, where it has
    /// them.
    count: Option<Raw>,
    week: u16,
    /// For an `iso8601` source: what `/t` holds, and the event time it
    /// gives.
    stamp: Option<(Stamp, EventTime)>,
    /// Whether a message of a packet with one key carries a `sub` no
    /// packet has, or one that can't be read (§4).
    stray: bool,
    fills: Vec<Fill>,
    received: i128,
    /// The seed of its spellings ([`Spell`]).
    seed: u64,
}

/// The `sub` a message carries that no packet has.
const STRAY: i128 = 1000;

fn message(set: JsonSet, index: usize) -> BoxedStrategy<Message> {
    draft(&set, index)
        .prop_map(move |draft| pack(&set, index, &draft))
        .boxed()
}

fn draft(set: &JsonSet, index: usize) -> BoxedStrategy<JsonDraft> {
    let packet = set.packets[index].clone();
    let time = set.time.clone();
    received()
        .prop_flat_map(move |ground| {
            let count = match (&time, packet.timing.count) {
                (JsonTime::Fields(source), Some(count)) => {
                    count_draw(source, count, &packet.timing.count_invalid)
                        .prop_map(Some)
                        .boxed()
                }
                _ => Just(None).boxed(),
            };
            let week = match &time {
                JsonTime::Fields(source @ TimeSource::Gps { .. }) => {
                    week_draw(source, ground, &packet.timing.week_invalid)
                }
                _ => Just(0).boxed(),
            };
            let stamp = match &time {
                JsonTime::Iso8601 => stamp(ground).prop_map(Some).boxed(),
                JsonTime::Fields(_) => Just(None).boxed(),
            };
            (
                Just(ground),
                count,
                week,
                stamp,
                fills(&packet.entries),
                any::<bool>(),
                any::<u64>(),
            )
        })
        .prop_map(
            |(received, count, week, stamp, fills, stray, seed)| JsonDraft {
                count,
                week,
                stamp,
                stray,
                fills,
                received,
                seed,
            },
        )
        .boxed()
}

/// A count field's value: as the binary oracle draws one, a sentinel now
/// and then, and finite, as JSON has no NaN or infinity (±1e300 is as far
/// past year 9999).
fn count_draw(source: &TimeSource, count: Count, invalid: &[i128]) -> BoxedStrategy<Raw> {
    let value = count_value(source, count).prop_map(|raw| match raw {
        Raw::Float(x) if !x.is_finite() => Raw::Float(if x < 0.0 { -1e300 } else { 1e300 }),
        raw => raw,
    });
    if invalid.is_empty() {
        return value.boxed();
    }
    let sentinels: Vec<Raw> = invalid
        .iter()
        .map(|n| match count {
            Count::Uint(_) => Raw::Uint(u64::try_from(*n).unwrap_or(0)),
            Count::Int(_) => Raw::Int(i64::try_from(*n).unwrap_or(0)),
            Count::Float(_) => Raw::Float(*n as f64),
        })
        .collect();
    prop_oneof![4 => value, 1 => select(sentinels)].boxed()
}

fn week_draw(source: &TimeSource, ground: i128, invalid: &[u64]) -> BoxedStrategy<u16> {
    if invalid.is_empty() {
        return week_value(source, ground);
    }
    let sentinels: Vec<u16> = invalid
        .iter()
        .map(|n| u16::try_from(*n).unwrap_or(0))
        .collect();
    prop_oneof![4 => week_value(source, ground), 1 => select(sentinels)].boxed()
}

/// What `/t` holds for an `iso8601` source, and the event time it gives
/// (§8): an RFC 3339 instant near the ground time or near the ends of
/// years 0001 to 9999, written any way §8 allows; now and then text that
/// isn't one, a number, or nothing.
fn stamp(ground: i128) -> BoxedStrategy<(Stamp, EventTime)> {
    let day = 86_400 * SECOND;
    let first = label((1, 1, 1), (0, 0, 0), 0);
    let last = label((9999, 12, 31), (23, 59, 59), SECOND - 1);
    let instants = prop_oneof![
        6 => ground - 400 * day..ground + 400 * day,
        1 => first - day..first + day,
        1 => last - day..last + day,
    ];
    let written = (instants, rfc3339_form())
        .prop_filter_map("a local year four digits write", |(unix, form)| {
            rfc3339(unix, &form)
        })
        .prop_map(|(text, read)| (Stamp::Text(text), instant(Some(read), Scale::Utc)));
    let botched =
        select(&BOTCHED[..]).prop_map(|text| (Stamp::Text(text.to_string()), EventTime::Invalid));
    let other = prop_oneof![
        Just(Stamp::Missing),
        any::<i32>().prop_map(|n| Stamp::Number(i64::from(n))),
    ]
    .prop_map(|stamp| (stamp, EventTime::Invalid));
    prop_oneof![8 => written, 1 => botched, 1 => other].boxed()
}

/// Text near an RFC 3339 instant that isn't one (§8).
const BOTCHED: [&str; 16] = [
    // No offset.
    "2026-06-01T12:34:56",
    // A month, a day, an hour, a minute or a second out of range.
    "2026-13-01T12:34:56Z",
    "2026-06-31T12:34:56Z",
    "2026-02-29T12:34:56Z",
    "2026-06-01T24:00:00Z",
    "2026-06-01T12:60:00Z",
    "2026-06-01T12:34:61Z",
    // An offset of 24 hours, of 60 minutes, or without its colon.
    "2026-06-01T12:34:56+24:00",
    "2026-06-01T12:34:56+05:60",
    "2026-06-01T12:34:56+0500",
    // A short year, or no seconds.
    "26-06-01T12:34:56Z",
    "2026-06-01T12:34Z",
    // A comma before the fraction, or no digits after the point.
    "2026-06-01T12:34:56,5Z",
    "2026-06-01T12:34:56.Z",
    // A separator §8 doesn't name, and a stray space.
    "2026-06-01x12:34:56Z",
    "2026-06-01T12:34:56Z ",
];

/// What's drawn for an entry before conditions are worked out: its value,
/// whether an optional one is absent (`null` there, or not), and what a
/// skipped one's pointer finds.
#[derive(Debug, Clone)]
struct Drawn {
    fill: Fill,
    absent: Option<bool>,
    junk: Option<Doc>,
}

/// What a message holds for a container's entries: each one's value, an
/// optional one absent now and then (nothing at its pointer, or `null`),
/// and a conditional one skipped where its selector's value fails the
/// condition, with what its pointer finds half the time.
fn fills(entries: &[JsonEntry]) -> BoxedStrategy<Vec<Fill>> {
    let selected = selector_values(entries);
    let drawn: Vec<BoxedStrategy<Drawn>> = entries
        .iter()
        .map(|entry| {
            let absent = if entry.optional {
                prop_oneof![6 => Just(None), 1 => Just(Some(false)), 1 => Just(Some(true))].boxed()
            } else {
                Just(None).boxed()
            };
            let junk = if entry.when.is_some() {
                proptest::option::weighted(0.5, junk()).boxed()
            } else {
                Just(None).boxed()
            };
            (fill(entry, &selected), absent, junk)
                .prop_map(|(fill, absent, junk)| Drawn { fill, absent, junk })
                .boxed()
        })
        .collect();
    let entries = entries.to_vec();
    drawn
        .prop_map(move |drawn| {
            let mut record: Vec<(String, Raw)> = Vec::new();
            let mut fills = Vec::new();
            for (entry, Drawn { fill, absent, junk }) in entries.iter().zip(drawn) {
                if entry.when.as_ref().is_some_and(|when| !when.holds(&record)) {
                    fills.push(Fill::Skipped(junk));
                    continue;
                }
                let fill = match absent {
                    Some(null) => Fill::Absent { null },
                    None => fill,
                };
                if let Fill::Scalar(raw) = &fill {
                    record.push((entry.name.clone(), raw.clone()));
                }
                fills.push(fill);
            }
            fills
        })
        .boxed()
}

fn fill(entry: &JsonEntry, selected: &BTreeMap<String, Vec<Raw>>) -> BoxedStrategy<Fill> {
    match &entry.node {
        JsonNode::Field(field) => {
            let values = match selected.get(&field.name) {
                Some(leaned) => {
                    prop_oneof![3 => select(leaned.clone()), 1 => json_raw(field)].boxed()
                }
                None => json_raw(field),
            };
            values.prop_map(Fill::Scalar).boxed()
        }
        JsonNode::Array { element, .. } => {
            let element = match element {
                JsonElement::Scalar(element) => json_raw(element).prop_map(Element::Scalar).boxed(),
                JsonElement::Record(ty) => fills(&ty.entries).prop_map(Element::Record).boxed(),
            };
            vec(proptest::option::weighted(0.85, element), 0..=MOST_ELEMENTS)
                .prop_map(Fill::Array)
                .boxed()
        }
        JsonNode::Struct(ty) => fills(&ty.entries).prop_map(Fill::Struct).boxed(),
    }
}

/// A raw value for a JSON field: as a binary field of its kind holds, but
/// finite, as JSON has no NaN or infinity.
fn json_raw(field: &Placed) -> BoxedStrategy<Raw> {
    raw_value(field)
        .prop_map(|raw| match raw {
            Raw::Float(x) if x.is_nan() => Raw::Float(0.0),
            Raw::Float(x) if x.is_infinite() => {
                Raw::Float(if x > 0.0 { f64::MAX } else { f64::MIN })
            }
            raw => raw,
        })
        .boxed()
}

/// Any small JSON value: what a skipped entry's pointer may find.
fn junk() -> BoxedStrategy<Doc> {
    prop_oneof![
        Just(Doc::Null),
        any::<bool>().prop_map(Doc::Bool),
        any::<i64>().prop_map(|n| Doc::Int(i128::from(n))),
        vec(select(&['q', 'r', 's'][..]), 0..=3)
            .prop_map(|chars| Doc::Str(chars.into_iter().collect())),
        Just(Doc::Array(Vec::new())),
        Just(Doc::Object(Vec::new())),
    ]
    .boxed()
}

/// The values each selector field of a container leans on (§5.4).
fn selector_values(entries: &[JsonEntry]) -> BTreeMap<String, Vec<Raw>> {
    let mut operands: BTreeMap<&str, Vec<(Op, Operand)>> = BTreeMap::new();
    for entry in entries {
        if let Some(when) = &entry.when {
            when.test
                .operands(operands.entry(when.field.as_str()).or_default());
        }
    }
    entries
        .iter()
        .filter_map(|entry| {
            let JsonNode::Field(field) = &entry.node else {
                return None;
            };
            let compared = operands.get(field.name.as_str())?;
            let raws = leaning(&field.kind, compared);
            (!raws.is_empty()).then(|| (field.name.clone(), raws))
        })
        .collect()
}

// ---- Writing ----------------------------------------------------------------

/// A drafted message, valid: its document written, and the record,
/// samples and event time decoding it gives.
fn pack(set: &JsonSet, index: usize, draft: &JsonDraft) -> Message {
    let packet = &set.packets[index];
    let mut spell = Spell::new(draft.seed);
    let document = document(set, packet, draft, &mut spell);
    let payload = doc::write(&document, &mut spell).into_bytes();
    Message {
        packet: packet.name.clone(),
        payload,
        record: record(set, packet, draft),
        samples: samples(packet, draft),
        received: draft.received,
        event_time: event_time_of(set, packet, draft),
        encoding: Encoding::Json,
        damage: None,
    }
}

/// A message's document: the alias, the packet's keys, its time members,
/// and its entries' values at their pointers, with members no entry reads,
/// in any order.
fn document(set: &JsonSet, packet: &JsonPacket, draft: &JsonDraft, spell: &mut Spell) -> Doc {
    let mut members: Vec<(String, Doc)> = Vec::new();
    match set.alias {
        Alias::Text => members.push(("sat".into(), Doc::Str("A1".into()))),
        Alias::Number if spell.pick(4) == 0 => {
            members.push(("sat".into(), Doc::Str("17".into())));
        }
        Alias::Number => members.push(("sat".into(), Doc::Int(17))),
        Alias::Fixed => {}
    }
    for (name, key) in &packet.keys {
        members.push((name.clone(), key_doc(key)));
    }
    if packet.keys.is_empty() {
        members.extend(catch_all_kind(set, spell).map(|kind| ("kind".to_string(), kind)));
    }
    if packet.keys.len() == 1 && draft.stray {
        // A `sub` no packet has, or one that can't be read: the packet
        // that nests on this one doesn't match, so this one does (§4).
        let sub = match spell.pick(2) {
            0 => Doc::Int(STRAY),
            _ => {
                let unusable = UNREADABLE[spell.pick(UNREADABLE.len())];
                unusable.value().unwrap_or(Doc::Int(STRAY))
            }
        };
        members.push(("sub".into(), sub));
    }
    match &draft.stamp {
        Some((Stamp::Text(text), _)) => members.push(("t".into(), Doc::Str(text.clone()))),
        Some((Stamp::Number(n), _)) => members.push(("t".into(), Doc::Int(i128::from(*n)))),
        Some((Stamp::Missing, _)) | None => {}
    }
    if let Some(count) = &draft.count {
        if matches!(set.time, JsonTime::Fields(TimeSource::Gps { .. })) {
            members.push(("week".into(), Doc::Int(i128::from(draft.week))));
        }
        members.push(("t".into(), scalar_doc(count)));
    }
    members.extend(members_of(&packet.entries, &draft.fills, spell));
    extras(&mut members, spell);
    edges(&mut members, spell);
    spell.shuffle(&mut members);
    Doc::Object(members)
}

fn key_doc(key: &Key) -> Doc {
    match key {
        Key::Text(text) => Doc::Str(text.clone()),
        Key::Int(n) => Doc::Int(i128::from(*n)),
    }
}

/// What a value of a key or locator that can't be read is (§3.1, §4),
/// each kind but nothing there.
const UNREADABLE: [Unusable; 7] = [
    Unusable::Array,
    Unusable::Bool,
    Unusable::Fraction,
    Unusable::Large,
    Unusable::Null,
    Unusable::Object,
    Unusable::Wide,
];

impl Unusable {
    /// What a member holds for it; `None` for nothing there.
    fn value(self) -> Option<Doc> {
        Some(match self {
            Unusable::Array => Doc::Array(vec![Doc::Int(7)]),
            Unusable::Bool => Doc::Bool(true),
            Unusable::Fraction => Doc::Literal("1.5".into()),
            Unusable::Gone => return None,
            Unusable::Large => Doc::Literal("1e20".into()),
            Unusable::Null => Doc::Null,
            Unusable::Object => Doc::Object(vec![("v".into(), Doc::Int(7))]),
            Unusable::Wide => Doc::Literal("18446744073709551616".into()),
        })
    }
}

/// What a catch-all's message holds at `/kind`, if anything: a kind no
/// packet has, a value that can't be read, or a numbered packet's kind
/// written as text, which a pointer compares exactly (§3.2). None of
/// them matches a keyed packet, so the catch-all is the message's (§4).
fn catch_all_kind(set: &JsonSet, spell: &mut Spell) -> Option<Doc> {
    let numbered = set
        .packets
        .iter()
        .find_map(|packet| match packet.keys.first() {
            Some((_, Key::Int(n))) => Some(*n),
            _ => None,
        });
    match spell.pick(4) {
        0 => UNREADABLE[spell.pick(UNREADABLE.len())].value(),
        1 => None,
        2 => numbered.map_or(Some(Doc::Str("Z".into())), |n| {
            Some(Doc::Str(n.to_string()))
        }),
        _ => Some(Doc::Str("Z".into())),
    }
}

/// A raw value as JSON.
fn scalar_doc(raw: &Raw) -> Doc {
    match raw {
        Raw::Uint(n) => Doc::Int(i128::from(*n)),
        Raw::Int(n) => Doc::Int(i128::from(*n)),
        Raw::Float(x) => Doc::Float(*x),
        Raw::Bool(b) => Doc::Bool(*b),
        Raw::Str(text) => Doc::Str(text.clone()),
        Raw::Bytes(_) | Raw::Array(_) | Raw::Record(_) | Raw::Null => Doc::Null,
    }
}

/// The members a container's entries put in its object: each value at its
/// pointer, `null` for an optional entry absent that way, and what a
/// skipped entry's pointer finds.
fn members_of(entries: &[JsonEntry], fills: &[Fill], spell: &mut Spell) -> Vec<(String, Doc)> {
    let mut members = Vec::new();
    for (entry, fill) in entries.iter().zip(fills) {
        let doc = match (&entry.node, fill) {
            (_, Fill::Scalar(raw)) => Some(scalar_doc(raw)),
            (JsonNode::Array { element, .. }, Fill::Array(items)) => {
                let items = items
                    .iter()
                    .map(|item| match (element, item) {
                        (_, Some(Element::Scalar(raw))) => scalar_doc(raw),
                        (JsonElement::Record(ty), Some(Element::Record(fills))) => {
                            object(&ty.entries, fills, spell)
                        }
                        _ => Doc::Null,
                    })
                    .collect();
                Some(Doc::Array(items))
            }
            (JsonNode::Struct(ty), Fill::Struct(fills)) => Some(object(&ty.entries, fills, spell)),
            (_, Fill::Absent { null: true }) => Some(Doc::Null),
            (_, Fill::Skipped(junk)) => junk.clone(),
            _ => None,
        };
        if let Some(doc) = doc {
            match entry.tokens.as_slice() {
                [name] => members.push((name.clone(), doc)),
                [outer, inner] => {
                    members.push((outer.clone(), Doc::Object(vec![(inner.clone(), doc)])));
                }
                _ => {}
            }
        }
    }
    members
}

/// A struct's or record's object.
fn object(entries: &[JsonEntry], fills: &[Fill], spell: &mut Spell) -> Doc {
    let mut members = members_of(entries, fills, spell);
    extras(&mut members, spell);
    spell.shuffle(&mut members);
    Doc::Object(members)
}

/// Now and then a member or two no entry reads (`x0`, `x1`), which
/// decoding passes over.
fn extras(members: &mut Vec<(String, Doc)>, spell: &mut Spell) {
    for k in 0..spell.pick(4).saturating_sub(1) {
        let value = match spell.pick(5) {
            0 => Doc::Null,
            1 => Doc::Bool(spell.pick(2) == 0),
            2 => Doc::Int(spell.pick(100) as i128),
            3 => Doc::Str("extra".into()),
            _ => Doc::Array(vec![Doc::Object(Vec::new())]),
        };
        members.push((format!("x{k}"), value));
    }
}

/// Now and then a document's member no entry reads that §2.3 accepts, at
/// its edges: nesting 127 deep, the document being the first level; names
/// alike but not equal, `é` composed and not; noncharacters; numbers
/// whose nearest double is 0; the empty name; NUL and U+2028 in a string.
fn edges(members: &mut Vec<(String, Doc)>, spell: &mut Spell) {
    if spell.pick(8) == 0 {
        members.push(("deep".into(), nested(DEEPEST - 1, spell.pick(2) == 0)));
    }
    if spell.pick(8) == 0 {
        let alike = ["a", "A", "a ", "\u{e9}", "e\u{301}"];
        let members_alike = alike
            .iter()
            .enumerate()
            .map(|(k, name)| (name.to_string(), Doc::Int(k as i128)))
            .collect();
        members.push(("alike".into(), Doc::Object(members_alike)));
    }
    if spell.pick(8) == 0 {
        members.push((
            "\u{fdd0}x\u{ffff}".into(),
            Doc::Str("\u{1fffe}\u{10ffff}".into()),
        ));
    }
    if spell.pick(8) == 0 {
        members.push(("tiny".into(), Doc::Literal("1e-400".into())));
    }
    // The empty name, which `/` points at; NUL (escaped) and U+2028 in a
    // string; a zero with a huge exponent, which is 0.
    if spell.pick(8) == 0 {
        members.push((String::new(), Doc::Str("\u{0}\u{2028}".into())));
    }
    if spell.pick(8) == 0 {
        members.push(("zero".into(), Doc::Literal("0e999999999".into())));
    }
}

/// The deepest arrays and objects nest that MDL reads (§2.3).
const DEEPEST: usize = 127;

/// Arrays, or objects, nested `levels` deep.
fn nested(levels: usize, objects: bool) -> Doc {
    let mut doc = Doc::Int(1);
    for _ in 0..levels {
        doc = if objects {
            Doc::Object(vec![("n".into(), doc)])
        } else {
            Doc::Array(vec![doc])
        };
    }
    doc
}

/// The record a message decodes to: its time fields, `kind` where a field
/// reads it, then its entries' values, absent and skipped ones left out,
/// a `null` element kept at its place (§5.3).
fn record(set: &JsonSet, packet: &JsonPacket, draft: &JsonDraft) -> Vec<(String, Raw)> {
    let mut record = Vec::new();
    if let Some(count) = &draft.count {
        if matches!(set.time, JsonTime::Fields(TimeSource::Gps { .. })) {
            record.push(("week".to_string(), Raw::Uint(u64::from(draft.week))));
        }
        record.push(("t".to_string(), count.clone()));
    }
    if let (true, Some((_, key))) = (packet.kind_field, packet.keys.first()) {
        let raw = match key {
            Key::Text(text) => Raw::Str(text.clone()),
            Key::Int(n) => Raw::Int(*n),
        };
        record.push(("kind".to_string(), raw));
    }
    record.extend(values(&packet.entries, &draft.fills));
    record
}

fn values(entries: &[JsonEntry], fills: &[Fill]) -> Vec<(String, Raw)> {
    entries
        .iter()
        .zip(fills)
        .filter_map(|(entry, fill)| {
            let raw = match (&entry.node, fill) {
                (_, Fill::Scalar(raw)) => raw.clone(),
                (JsonNode::Array { element, .. }, Fill::Array(items)) => Raw::Array(
                    items
                        .iter()
                        .map(|item| match (element, item) {
                            (_, Some(Element::Scalar(raw))) => raw.clone(),
                            (JsonElement::Record(ty), Some(Element::Record(fills))) => {
                                Raw::Record(values(&ty.entries, fills))
                            }
                            _ => Raw::Null,
                        })
                        .collect(),
                ),
                (JsonNode::Struct(ty), Fill::Struct(fills)) => {
                    Raw::Record(values(&ty.entries, fills))
                }
                _ => return None,
            };
            Some((entry.name.clone(), raw))
        })
        .collect()
}

/// The samples a message gives (§7.3): each mapped field and scalar
/// element in record order; a `null` element gives none, though later
/// elements keep their index (§5.3).
fn samples(packet: &JsonPacket, draft: &JsonDraft) -> Vec<Sample> {
    let mut samples = Vec::new();
    samples_of(&packet.entries, &draft.fills, "", None, &mut samples);
    samples
}

fn samples_of(
    entries: &[JsonEntry],
    fills: &[Fill],
    prefix: &str,
    index: Option<u64>,
    samples: &mut Vec<Sample>,
) {
    for (entry, fill) in entries.iter().zip(fills) {
        let path = if prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{prefix}.{}", entry.name)
        };
        match (&entry.node, fill) {
            (JsonNode::Field(field), Fill::Scalar(raw)) => {
                samples.extend(sample(field, raw, path, index, None));
            }
            (JsonNode::Array { element, interval }, Fill::Array(items)) => {
                for (position, item) in (0_u64..).zip(items) {
                    let at = format!("{path}[{position}]");
                    match (element, item) {
                        (JsonElement::Scalar(element), Some(Element::Scalar(raw))) => {
                            let offset = interval.map(|ms| offset_ns(position, ms));
                            samples.extend(sample(element, raw, at, Some(position), offset));
                        }
                        (JsonElement::Record(ty), Some(Element::Record(fills))) => {
                            samples_of(&ty.entries, fills, &at, Some(position), samples);
                        }
                        _ => {}
                    }
                }
            }
            (JsonNode::Struct(ty), Fill::Struct(fills)) => {
                samples_of(&ty.entries, fills, &path, index, samples);
            }
            _ => {}
        }
    }
}

/// A message's event time (§8): what `/t` gives an `iso8601` source;
/// else the count's, ground time where a time field holds a sentinel.
fn event_time_of(set: &JsonSet, packet: &JsonPacket, draft: &JsonDraft) -> EventTime {
    match &set.time {
        JsonTime::Iso8601 => draft
            .stamp
            .as_ref()
            .map_or(EventTime::Invalid, |(_, time)| time.clone()),
        JsonTime::Fields(source) => {
            let timing = &packet.timing;
            if no_reading(
                source,
                draft.count.as_ref(),
                draft.week,
                &timing.count_invalid,
                &timing.week_invalid,
            ) {
                return EventTime::Ground;
            }
            event_time(
                source,
                &[],
                draft.count.as_ref(),
                u64::from(draft.week),
                draft.received,
            )
        }
    }
}
