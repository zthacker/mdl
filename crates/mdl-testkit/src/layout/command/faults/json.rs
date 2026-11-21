//! Faults planted in valid JSON command payloads, and the code decoding
//! each gives (`spec/encoding.md` §7, `decoding.md` §2.3, §5.3), one a
//! payload. Before any entry: the set, the encoding, text that isn't JSON
//! MDL reads, no packet (a key changed, gone or of another JSON type,
//! metadata gone), and a document that isn't an object, which no packet
//! matches unless one is keyed by metadata alone. Then, reading the
//! entries in order: a value gone or `null`, of another JSON type, or a
//! number an integer field can't take. Then the constants, then the
//! arguments: raw values decoding refuses, a `null` element, lists past
//! their bounds. Every top-level integer or string constant is a key in
//! JSON (§7.2), so a fault in one is `no_packet`.

use proptest::prelude::*;
use proptest::sample::Index;

use super::argument_name;
use super::garbled::{beyond_bounds, beyond_payloads, metadata, pick};
use crate::layout::command::canonical::{Canon, Hop, write};
use crate::layout::command::json::{document, read_back};
use crate::layout::command::requests::{parts, raws, values};
use crate::layout::command::values::unrounded;
use crate::layout::command::{
    ArgType, Const, Elements, Encoder, Encoding, Garbled, GarbledKind, Layout, Placed, Shape,
    Source, Through,
};

/// A valid JSON payload for the layout's command with one fault planted in
/// it, and the code decoding it gives. `keys` are every packet's values of
/// the key.
pub(in super::super) fn json_garbling(layout: Layout, keys: Vec<Const>) -> BoxedStrategy<Garbled> {
    (
        Just(layout.clone()),
        parts(&layout),
        any::<Index>(),
        any::<Index>(),
        beyond_bounds(&layout),
        any::<Index>(),
    )
        .prop_map(move |(layout, parts, which, detail, beyond, kind)| {
            let values = values(&layout, &parts.given);
            let target = parts.target.as_ref();
            let raws = read_back(raws(&layout, &values, target));
            let document = document(&layout, &raws);
            let mut faults = damaged(&layout, &keys, &document, &detail);
            faults.extend(beyond_payloads(&layout, &values, target, &beyond));
            pick(
                faults,
                &kind,
                &which,
                &layout,
                write(&document).into_bytes(),
            )
        })
        .boxed()
}

/// Where a fault is: its hops into the document, and the path an entry's
/// error names it by (`decoding.md` §9).
#[derive(Debug, Clone)]
struct At {
    hops: Vec<Hop>,
    path: String,
}

impl At {
    fn member(&self, tokens: &[String], name: &str) -> At {
        let mut hops = self.hops.clone();
        hops.extend(members(tokens));
        let path = if self.path.is_empty() {
            name.to_string()
        } else {
            format!("{}.{name}", self.path)
        };
        At { hops, path }
    }

    fn element(&self, index: usize) -> At {
        let mut hops = self.hops.clone();
        hops.push(Hop::Index(index));
        At {
            hops,
            path: format!("{}[{index}]", self.path),
        }
    }
}

/// The valid payload's document, and payloads made from it with one
/// change each.
struct Edits<'d> {
    document: &'d Canon,
}

impl Edits<'_> {
    /// The payload with the value at `hops` replaced; none where there's no
    /// value there.
    fn changed(&self, hops: &[Hop], value: Canon) -> Option<Vec<u8>> {
        let mut damaged = self.document.clone();
        *damaged.at_mut(hops)? = value;
        Some(write(&damaged).into_bytes())
    }

    /// The payload with the member at `hops` taken out; none where there's
    /// no member there.
    fn removed(&self, hops: &[Hop]) -> Option<Vec<u8>> {
        let mut damaged = self.document.clone();
        damaged.remove(hops).then(|| write(&damaged).into_bytes())
    }
}

/// The faults planted so far, each framed with the packet's metadata.
struct Faults {
    metadata: Vec<(String, i64)>,
    planted: Vec<Garbled>,
}

impl Faults {
    /// A fault, where its change could be made: its kind, payload, code,
    /// what its error names, and what it is, for a failing case's report.
    fn note(
        &mut self,
        kind: GarbledKind,
        payload: Option<Vec<u8>>,
        code: &'static str,
        at: Option<String>,
        planted: String,
    ) {
        if let Some(payload) = payload {
            self.planted.push(Garbled {
                set: "fam_v1".into(),
                payload,
                encoding: Encoding::Json,
                metadata: self.metadata.clone(),
                code,
                at,
                kind,
                planted,
            });
        }
    }
}

/// Every fault the payload can be given other than lists past their bounds,
/// each with its kind, the code it gives and what that error names; one
/// document that isn't an object, and one key's faults, by `detail`, so
/// the other ways no packet matches come up as often.
fn damaged(layout: &Layout, keys: &[Const], document: &Canon, detail: &Index) -> Vec<Garbled> {
    let edits = Edits { document };
    let mut faults = Faults {
        metadata: metadata(layout),
        planted: Vec::new(),
    };
    let text = write(document).into_bytes();
    for (payload, planted) in not_json(document) {
        faults.note(
            GarbledKind::NotJson,
            Some(payload),
            "invalid_json",
            None,
            planted,
        );
    }
    let key_count = layout
        .entries
        .iter()
        .filter(|placed| is_key(placed))
        .count();
    // A document that isn't an object: no key at a pointer can be read in
    // it, so no packet matches. A packet keyed by metadata alone matches
    // all the same, and then its document is `wrong_type` (§7.3).
    let roots = [
        Canon::Array(Vec::new()),
        Canon::Int(7),
        Canon::Str("{}".into()),
        Canon::Null,
    ];
    let other_root = &roots[detail.index(roots.len())];
    let (code, at) = if key_count == 0 {
        ("wrong_type", Some(String::new()))
    } else {
        ("no_packet", None)
    };
    faults.note(
        GarbledKind::Document,
        Some(write(other_root).into_bytes()),
        code,
        at,
        format!("the document {}", write(other_root)),
    );
    let faulted_key = detail.index(key_count.max(1));
    let mut keys_seen = 0;
    let root = At {
        hops: Vec::new(),
        path: String::new(),
    };
    for placed in &layout.entries {
        match placed {
            Placed::Field {
                name,
                shape,
                source,
                pointer,
                ..
            } => {
                let at = root.member(pointer, name);
                if is_key(placed) {
                    if keys_seen == faulted_key {
                        key_faults(&at, shape, source, keys, &edits, &mut faults);
                    }
                    keys_seen += 1;
                    continue;
                }
                value_faults(&at, shape, &edits, &mut faults);
                match source {
                    // A constant that isn't a key: a bool.
                    Source::Constant(value) => {
                        constant_fault(
                            GarbledKind::Constant,
                            &at,
                            value,
                            shape,
                            &edits,
                            &mut faults,
                        );
                    }
                    Source::Argument(argument) => {
                        let encoder = &layout.arguments[*argument].0;
                        let named = argument_name(*argument);
                        raw_faults(
                            GarbledKind::Raw,
                            &at,
                            named,
                            encoder,
                            shape,
                            &edits,
                            &mut faults,
                        );
                    }
                    _ => {}
                }
            }
            Placed::Array {
                name,
                elements,
                pointer,
                ..
            } => {
                let at = root.member(pointer, name);
                gone_faults(&at, &edits, &mut faults);
                for other in [
                    Canon::Object(Vec::new()),
                    Canon::Int(7),
                    Canon::Str("[]".into()),
                ] {
                    let planted = format!("{} holding {}", at.path, write(&other));
                    faults.note(
                        GarbledKind::WrongType,
                        edits.changed(&at.hops, other),
                        "wrong_type",
                        Some(at.path.clone()),
                        planted,
                    );
                }
                let count = match document.at(&at.hops) {
                    Some(Canon::Array(items)) => items.len(),
                    _ => 0,
                };
                for index in 0..count {
                    element_faults(
                        layout,
                        elements,
                        &at.element(index),
                        index,
                        &edits,
                        &mut faults,
                    );
                }
            }
            Placed::Padding(_) | Placed::Checksum { .. } => {}
        }
    }
    // Objects pointers go through, removed: the first entry under one, in
    // order, has nothing at its pointer, or no packet matches when a key is
    // under it.
    for (prefix, first, keyed) in containers(&layout.entries) {
        let shown = format!("the object at /{}", prefix.join("/"));
        let payload = edits.removed(&members(&prefix));
        if keyed {
            faults.note(
                GarbledKind::NoPacket,
                payload,
                "no_packet",
                None,
                format!("{shown}, which a key is in, removed"),
            );
        } else {
            faults.note(
                GarbledKind::Missing,
                payload,
                "missing_value",
                Some(first),
                format!("{shown} removed"),
            );
        }
    }
    let Faults {
        metadata,
        planted: mut faults,
    } = faults;
    if layout.apid.is_some() {
        faults.push(Garbled {
            set: "fam_v1".into(),
            payload: text.clone(),
            encoding: Encoding::Json,
            metadata: Vec::new(),
            code: "no_packet",
            at: None,
            kind: GarbledKind::NoPacket,
            planted: "no metadata".into(),
        });
    }
    faults.push(Garbled {
        set: "fam_v9".into(),
        payload: text.clone(),
        encoding: Encoding::Json,
        metadata: metadata.clone(),
        code: "unknown_set",
        at: None,
        kind: GarbledKind::Set,
        planted: "an unknown set".into(),
    });
    faults.push(Garbled {
        set: "fam_v1".into(),
        payload: text,
        encoding: Encoding::Binary,
        metadata,
        code: "wrong_encoding",
        at: None,
        kind: GarbledKind::Encoding,
        planted: "delivered as a binary payload".into(),
    });
    faults
}

/// Whether an entry is one of its packet's keys: a top-level integer or
/// string constant (§7.2).
fn is_key(placed: &Placed) -> bool {
    matches!(
        placed,
        Placed::Field {
            shape: Shape::Uint(_) | Shape::Int(..) | Shape::Text(..),
            source: Source::Constant(_),
            ..
        }
    )
}

/// A key changed so no packet matches: gone, `null`, of the other JSON
/// type, or a value no packet has (another value at all, for a key other
/// than the one every packet has).
fn key_faults(
    at: &At,
    shape: &Shape,
    source: &Source,
    keys: &[Const],
    edits: &Edits<'_>,
    faults: &mut Faults,
) {
    let Source::Constant(value) = source else {
        return;
    };
    let path = &at.path;
    let mut changes = vec![
        (edits.removed(&at.hops), format!("the key {path} removed")),
        (
            edits.changed(&at.hops, Canon::Null),
            format!("the key {path} null"),
        ),
    ];
    let taken = |candidate: &Const| keys.contains(candidate) || candidate == value;
    match value {
        Const::Int(n) => {
            changes.push((
                edits.changed(&at.hops, Canon::Str(n.to_string())),
                format!("the key {path} as text"),
            ));
            let held = shape.integers().unwrap_or(0..=0);
            if let Some(other) = held.take(64).find(|m| !taken(&Const::Int(*m))) {
                changes.push((
                    edits.changed(&at.hops, Canon::Int(other)),
                    format!("the key {path} as {other}"),
                ));
            }
        }
        Const::Text(_) => {
            changes.push((
                edits.changed(&at.hops, Canon::Int(7)),
                format!("the key {path} as a number"),
            ));
            if let Some(other) = (0..)
                .map(|k| format!("zz{k}"))
                .find(|text| !taken(&Const::Text(text.clone())))
            {
                changes.push((
                    edits.changed(&at.hops, Canon::Str(other.clone())),
                    format!("the key {path} as {other:?}"),
                ));
            }
        }
        Const::Bool(_) => {}
    }
    for (payload, planted) in changes {
        faults.note(GarbledKind::NoPacket, payload, "no_packet", None, planted);
    }
}

/// An entry's member gone, or `null` (`missing_value`, §5.3).
fn gone_faults(at: &At, edits: &Edits<'_>, faults: &mut Faults) {
    let path = &at.path;
    faults.note(
        GarbledKind::Missing,
        edits.removed(&at.hops),
        "missing_value",
        Some(path.clone()),
        format!("{path} removed"),
    );
    faults.note(
        GarbledKind::Missing,
        edits.changed(&at.hops, Canon::Null),
        "missing_value",
        Some(path.clone()),
        format!("{path} null"),
    );
}

/// What decoding refuses at a field or record field while reading it: its
/// member gone or `null`, a value of another JSON type, and for an integer
/// field, numbers it can't take.
fn value_faults(at: &At, shape: &Shape, edits: &Edits<'_>, faults: &mut Faults) {
    gone_faults(at, edits, faults);
    wrong_values(at, shape, edits, faults);
}

/// A field's or element's value of another JSON type (`wrong_type`), and
/// for an integer one, numbers it can't take (`out_of_range`, §5.3).
fn wrong_values(at: &At, shape: &Shape, edits: &Edits<'_>, faults: &mut Faults) {
    let path = &at.path;
    for other in other_types(shape) {
        let planted = format!("{path} holding {}", write(&other));
        faults.note(
            GarbledKind::WrongType,
            edits.changed(&at.hops, other),
            "wrong_type",
            Some(path.clone()),
            planted,
        );
    }
    for literal in unfit(shape) {
        faults.note(
            GarbledKind::Unfit,
            edits.changed(&at.hops, Canon::Literal(literal.to_string())),
            "out_of_range",
            Some(path.clone()),
            format!("{path} holding {literal}"),
        );
    }
}

/// A constant holding another value of its JSON type, which its field
/// holds (`wrong_constant`, §7.3).
fn constant_fault(
    kind: GarbledKind,
    at: &At,
    value: &Const,
    shape: &Shape,
    edits: &Edits<'_>,
    faults: &mut Faults,
) {
    let other = match value {
        Const::Int(n) => {
            let held = shape.integers().unwrap_or(0..=0);
            [*n + 1, *n - 1]
                .into_iter()
                .find(|m| held.contains(m))
                .map(Canon::Int)
        }
        Const::Bool(b) => Some(Canon::Bool(!b)),
        Const::Text(text) => Some(Canon::Str(format!("{text}x"))),
    };
    if let Some(other) = other {
        let planted = format!("{} holding {}", at.path, write(&other));
        faults.note(
            kind,
            edits.changed(&at.hops, other),
            "wrong_constant",
            Some(at.path.clone()),
            planted,
        );
    }
}

/// Raw values the field at `at` holds that decoding refuses for its
/// argument, the error naming `argument` (`a3`, or `a3[1]` for a list's
/// value).
fn raw_faults(
    kind: GarbledKind,
    at: &At,
    argument: String,
    encoder: &Encoder,
    shape: &Shape,
    edits: &Edits<'_>,
    faults: &mut Faults,
) {
    for (raw, code) in refused_raws(encoder, shape) {
        let planted = format!("{} holding {}", at.path, write(&raw));
        faults.note(
            kind,
            edits.changed(&at.hops, raw),
            code,
            Some(argument.clone()),
            planted,
        );
    }
}

/// Values of every JSON type a field of `shape` doesn't take but `null`,
/// which is a missing value (§5.3).
fn other_types(shape: &Shape) -> Vec<Canon> {
    let mut others = vec![Canon::Array(Vec::new()), Canon::Object(Vec::new())];
    match shape {
        Shape::Uint(_) | Shape::Int(..) | Shape::Float(_) => {
            others.extend([Canon::Str("7".into()), Canon::Bool(true)]);
        }
        Shape::Bool => others.extend([Canon::Int(1), Canon::Str("true".into())]),
        Shape::Text(..) | Shape::Bytes(_) => others.extend([Canon::Int(7), Canon::Bool(true)]),
    }
    others
}

/// Numbers an integer field can't take (§5.3, §2.3): a fraction, a double
/// past ±(2^53 − 1), an integer literal past its 64 bits, and for `uint`
/// a negative one.
fn unfit(shape: &Shape) -> Vec<&'static str> {
    match shape {
        Shape::Uint(_) => vec![
            "1.5",
            "1e20",
            "9007199254740992.0",
            "18446744073709551616",
            "-1",
        ],
        Shape::Int(..) => vec!["1.5", "1e20", "9007199254740992.0", "9223372036854775808"],
        _ => Vec::new(),
    }
}

/// An array element's faults: `null` (no value of its list, §7.4); of
/// another JSON type; an integer element's numbers it can't take; raw
/// values its list's argument refuses; and a record's fields' faults as a
/// field's, its constants changed, and objects its fields' pointers go
/// through removed.
fn element_faults(
    layout: &Layout,
    elements: &Elements,
    at: &At,
    index: usize,
    edits: &Edits<'_>,
    faults: &mut Faults,
) {
    let path = &at.path;
    // The list the element is a value of, or the first of a record's lists
    // in catalog order, which a `null` gives no value.
    let first_list = match elements {
        Elements::Scalar { argument, .. } => Some(*argument),
        Elements::Records { entries, .. } => entries
            .iter()
            .filter_map(|entry| match entry {
                Placed::Field {
                    source: Source::Argument(argument),
                    ..
                } => Some(*argument),
                _ => None,
            })
            .min(),
    };
    if let Some(argument) = first_list {
        faults.note(
            GarbledKind::Null,
            edits.changed(&at.hops, Canon::Null),
            "wrong_type",
            Some(format!("{}[{index}]", argument_name(argument))),
            format!("{path} null"),
        );
    }
    match elements {
        Elements::Scalar {
            argument, shape, ..
        } => {
            wrong_values(at, shape, edits, faults);
            let encoder = &layout.arguments[*argument].0;
            let named = format!("{}[{index}]", argument_name(*argument));
            raw_faults(
                GarbledKind::Element,
                at,
                named,
                encoder,
                shape,
                edits,
                faults,
            );
        }
        Elements::Records { entries, .. } => {
            for other in [
                Canon::Int(7),
                Canon::Str("{}".into()),
                Canon::Array(Vec::new()),
            ] {
                let planted = format!("{path} holding {}", write(&other));
                faults.note(
                    GarbledKind::WrongType,
                    edits.changed(&at.hops, other),
                    "wrong_type",
                    Some(path.clone()),
                    planted,
                );
            }
            for entry in entries {
                let Placed::Field {
                    name,
                    shape,
                    source,
                    pointer,
                    ..
                } = entry
                else {
                    continue;
                };
                let field = at.member(pointer, name);
                value_faults(&field, shape, edits, faults);
                match source {
                    Source::Constant(value) => {
                        constant_fault(
                            GarbledKind::RecordConstant,
                            &field,
                            value,
                            shape,
                            edits,
                            faults,
                        );
                    }
                    Source::Argument(argument) => {
                        let encoder = &layout.arguments[*argument].0;
                        let named = format!("{}[{index}]", argument_name(*argument));
                        raw_faults(
                            GarbledKind::Element,
                            &field,
                            named,
                            encoder,
                            shape,
                            edits,
                            faults,
                        );
                    }
                    _ => {}
                }
            }
            for (prefix, first, _) in containers(entries) {
                let mut hops = at.hops.clone();
                hops.extend(members(&prefix));
                faults.note(
                    GarbledKind::Missing,
                    edits.removed(&hops),
                    "missing_value",
                    Some(format!("{path}.{first}")),
                    format!("{path}'s object at /{} removed", prefix.join("/")),
                );
            }
        }
    }
}

/// Each object the entries' pointers go through, by its tokens, with the
/// first entry under it, in order, and whether a key is under it.
fn containers(entries: &[Placed]) -> Vec<(Vec<String>, String, bool)> {
    let mut found: Vec<(Vec<String>, String, bool)> = Vec::new();
    for entry in entries {
        let (name, pointer) = match entry {
            Placed::Field { name, pointer, .. } | Placed::Array { name, pointer, .. } => {
                (name, pointer)
            }
            Placed::Padding(_) | Placed::Checksum { .. } => continue,
        };
        for k in 1..pointer.len() {
            let prefix = pointer[..k].to_vec();
            match found.iter_mut().find(|(seen, ..)| *seen == prefix) {
                Some((_, _, keyed)) => *keyed |= is_key(entry),
                None => found.push((prefix, name.clone(), is_key(entry))),
            }
        }
    }
    found
}

fn members(tokens: &[String]) -> Vec<Hop> {
    tokens
        .iter()
        .map(|token| Hop::Member(token.clone()))
        .collect()
}

/// The payload made into text that isn't JSON MDL reads (§2.3): cut before
/// its closing brace, a token after it, a byte order mark before it, a
/// byte that isn't UTF-8 in it, a member twice, a number past binary64's
/// largest, an unpaired surrogate.
fn not_json(document: &Canon) -> Vec<(Vec<u8>, String)> {
    let text = write(document).into_bytes();
    let mut cut = text.clone();
    cut.pop();
    let mut trailing = text.clone();
    trailing.extend_from_slice(b" x");
    let mut marked = vec![0xEF, 0xBB, 0xBF];
    marked.extend_from_slice(&text);
    let mut stray = text.clone();
    stray.insert(1, 0xFF);
    let mut twice = document.clone();
    if let Canon::Object(members) = &mut twice {
        match members.first().cloned() {
            Some(first) => members.insert(0, first),
            // A packet with no entries writes `{}`: a member no field reads,
            // twice.
            None => {
                let unread = ("x9".to_string(), Canon::Int(0));
                members.extend([unread.clone(), unread]);
            }
        }
    }
    let with = |value: &str| {
        let mut extended = document.clone();
        extended.put(&["x9".to_string()], Canon::Literal(value.to_string()));
        write(&extended).into_bytes()
    };
    vec![
        (cut, "cut before its closing brace".into()),
        (trailing, "a token after the document".into()),
        (marked, "a byte order mark before it".into()),
        (stray, "a byte that isn't UTF-8".into()),
        (write(&twice).into_bytes(), "a member twice".into()),
        (with("1e400"), "a number past binary64's largest".into()),
        (with("\"\\ud800\""), "an unpaired surrogate".into()),
    ]
}

/// Raw values a field of `shape` holds that decoding refuses for the
/// encoder's argument (§7.4), as JSON, each with its code: an unlisted
/// state's integer, text naming no state, 2 for a bool, an integer past its
/// type or range, a raw value past what an f64 range's ends encode to.
fn refused_raws(encoder: &Encoder, shape: &Shape) -> Vec<(Canon, &'static str)> {
    let mut raws = Vec::new();
    let held = shape.integers();
    let integer = |n: i128| {
        held.as_ref()
            .filter(|range| range.contains(&n))
            .map(|_| Canon::Int(n))
    };
    match (&encoder.ty, &encoder.through) {
        (ArgType::Enum(_), Through::States(states)) => {
            let unaccepted = encoder.unaccepted.iter().filter_map(|state| state.raw);
            let listed: Vec<i128> = states
                .iter()
                .map(|(raw, _)| *raw)
                .chain(unaccepted)
                .collect();
            let unlisted = held
                .clone()
                .and_then(|range| range.take(64).find(|raw| !listed.contains(raw)));
            raws.extend(unlisted.and_then(integer).map(|raw| (raw, "unknown_state")));
            if let Some(raw) = encoder.unaccepted.iter().find_map(|state| state.raw) {
                raws.extend(integer(raw).map(|raw| (raw, "unsupported_state")));
            }
        }
        (ArgType::Enum(states), _) => {
            let named = |text: &String| {
                states.contains(text) || encoder.unaccepted.iter().any(|s| &s.name == text)
            };
            if let Some(text) = (0..).map(|k| format!("zz{k}")).find(|text| !named(text)) {
                raws.push((Canon::Str(text), "unknown_state"));
            }
            if let Some(state) = encoder.unaccepted.first() {
                raws.push((Canon::Str(state.name.clone()), "unsupported_state"));
            }
        }
        (ArgType::Bool, _) => raws.extend(integer(2).map(|raw| (raw, "out_of_range"))),
        (ArgType::U64 | ArgType::I64, _) => {
            let types = encoder.ty.integers();
            let (min, max) = encoder.integer_range;
            let high = max.unwrap_or(*types.end());
            let low = min.unwrap_or(*types.start());
            let past = integer(high + 1).or_else(|| integer(low - 1));
            raws.extend(past.map(|raw| (raw, "out_of_range")));
        }
        (ArgType::F64, _) => {
            if let (Some(min), Some(max)) = (encoder.min, encoder.max)
                && let (Some(a), Some(b)) = (unrounded(encoder, min), unrounded(encoder, max))
            {
                let (low, high) = (a.min(b), a.max(b));
                match shape {
                    Shape::Float(_) => {
                        let past = high.next_up();
                        if past.is_finite() {
                            raws.push((Canon::Float(past), "out_of_range"));
                        }
                    }
                    _ => {
                        let past = integer(high.round() as i128 + 1)
                            .or_else(|| integer(low.round() as i128 - 1));
                        raws.extend(past.map(|raw| (raw, "out_of_range")));
                    }
                }
            }
        }
        _ => {}
    }
    raws
}

#[cfg(test)]
mod tests {
    use super::{Canon, not_json, write};

    /// Each way `not_json` makes text that isn't JSON changes it, `{}`
    /// too, which a packet with no entries writes when metadata alone
    /// keys it. Any JSON reader refuses five of the ways; a member twice
    /// and `1e400` only one that holds to §2.3, so their text is checked.
    #[test]
    fn every_way_text_is_made_not_json_changes_it() {
        let cases = [
            (Canon::Object(Vec::new()), r#"{"x9":0,"x9":0}"#),
            (
                Canon::Object(vec![("a".to_string(), Canon::Int(1))]),
                r#"{"a":1,"a":1}"#,
            ),
        ];
        for (document, twice) in cases {
            let valid = write(&document).into_bytes();
            for (payload, planted) in not_json(&document) {
                let text = String::from_utf8_lossy(&payload).into_owned();
                assert_ne!(payload, valid, "{planted}: {text}");
                match planted.as_str() {
                    "a member twice" => assert_eq!(text, twice),
                    "a number past binary64's largest" => {
                        assert!(text.contains(r#""x9":1e400"#), "{text}");
                    }
                    _ => assert!(
                        serde_json::from_slice::<serde_json::Value>(&payload).is_err(),
                        "{planted}: {text}"
                    ),
                }
            }
        }
    }
}
