//! Faults planted in valid requests and payloads, with the error each one
//! gives. A request is refused at the first problem `spec/encoding.md`
//! reaches: its set and command (§1), then §2.1's two passes (the given
//! names in the order given, then every argument in catalog order), then
//! §3's entries in order, length fields last. A payload's decoding fails
//! where its fault is (§7). Its checksums are worked out again after the
//! fault, so the fault is the only thing wrong with it, unless the fault
//! is a checksum's.

use std::collections::BTreeSet;

use proptest::prelude::*;
use proptest::sample::{Index, select};

use super::requests::{Packed, Parts, buffer, pack, parts, values, write_checksums};
use super::values::{list_value, spell, unrounded, value};
use super::{
    Arg, ArgType, Const, Encoder, Encoding, Fault, Garbled, Given, Layout, Len, Placed, Refused,
    Shape, Source, Stage, Target, Through,
};
use crate::layout::{Numeric, Text, fit, put_bits};

// ---- Requests --------------------------------------------------------------

/// A fault to plant in a valid request.
#[derive(Debug, Clone)]
enum Plant {
    /// A set the family doesn't have.
    Set,
    /// A command the set doesn't encode.
    Command,
    /// An argument the command doesn't have, at a place in the request.
    Unknown { value: Given, place: Index },
    /// An argument given again, at a place in the request.
    Duplicate {
        argument: usize,
        value: Given,
        place: Index,
    },
    /// An argument without a default, left out.
    Missing { argument: usize },
    /// An argument given a value that fails at `stage` with `code`.
    Value {
        argument: usize,
        value: Given,
        stage: Stage,
        code: &'static str,
    },
    /// The target replaced, or left out, failing at its field.
    Target {
        target: Option<Target>,
        entry: usize,
        code: &'static str,
    },
}

/// What a plant touches. Two plants that touch one argument, or the
/// target, aren't both planted; a set or command plant goes alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Site {
    Request,
    Argument(usize),
    Target,
}

impl Plant {
    fn site(&self) -> Option<Site> {
        match self {
            Plant::Set | Plant::Command => Some(Site::Request),
            Plant::Unknown { .. } => None,
            Plant::Duplicate { argument, .. }
            | Plant::Missing { argument }
            | Plant::Value { argument, .. } => Some(Site::Argument(*argument)),
            Plant::Target { .. } => Some(Site::Target),
        }
    }

    fn clashes(&self, other: &Plant) -> bool {
        let request = |plant: &Plant| plant.site() == Some(Site::Request);
        request(self) || request(other) || (self.site().is_some() && self.site() == other.site())
    }
}

fn argument_name(index: usize) -> String {
    format!("a{index}")
}

/// A valid request for the layout's command with one or two faults
/// planted in it, and the error encoding gives. A quarter of the time
/// with a length field's fault, it's paired with a fault at a later entry,
/// which §3.5 catches first: the length fields come after every entry.
pub(super) fn refusal(layout: Layout) -> BoxedStrategy<Refused> {
    let staged = plants(&layout);
    let count = staged.len();
    let pairs: Vec<(usize, usize)> = (0..count)
        .flat_map(|first| (0..count).map(move |second| (first, second)))
        .filter(
            |&(first, second)| match (staged[first].0, staged[second].0) {
                (Stage::Length(measured), Stage::Field(later)) => later > measured,
                _ => false,
            },
        )
        .collect();
    let any_picks = proptest::collection::vec(any::<Index>(), 1..=2).prop_map(move |picks| {
        picks
            .iter()
            .map(|pick| pick.index(count))
            .collect::<Vec<_>>()
    });
    let picks = if pairs.is_empty() {
        any_picks.boxed()
    } else {
        prop_oneof![
            3 => any_picks,
            1 => select(pairs).prop_map(|(first, second)| vec![first, second]),
        ]
        .boxed()
    };
    let plants: Vec<BoxedStrategy<Plant>> = staged.into_iter().map(|(_, plant)| plant).collect();
    (Just(layout.clone()), parts(&layout), picks)
        .prop_flat_map(move |(layout, parts, picks)| {
            let chosen: Vec<BoxedStrategy<Plant>> =
                picks.iter().map(|pick| plants[*pick].clone()).collect();
            (Just(layout), Just(parts), chosen)
        })
        .prop_map(|(layout, parts, chosen)| refused(&layout, parts, chosen))
        .boxed()
}

/// The request with the plants that don't clash planted, and the first
/// error it reaches.
fn refused(layout: &Layout, parts: Parts, chosen: Vec<Plant>) -> Refused {
    let mut plants: Vec<Plant> = Vec::new();
    for plant in chosen {
        if !plants.iter().any(|kept| kept.clashes(&plant)) {
            plants.push(plant);
        }
    }
    let mut set = "fam_v1".to_string();
    let mut command = layout.command.clone();
    let mut named = parts.named;
    let mut target = parts.target;
    let mut faults: Vec<Fault> = Vec::new();
    let mut fault = |code: &'static str, stage: Stage, argument: Option<usize>| {
        faults.push(Fault {
            code,
            stage,
            argument,
        });
    };
    for plant in plants {
        match plant {
            Plant::Set => {
                set = "fam_v9".into();
                fault("unknown_set", Stage::Request, None);
            }
            Plant::Command => {
                command = "cmd.nope".into();
                fault("unknown_command", Stage::Request, None);
            }
            Plant::Unknown { value, place } => {
                named.insert(place.index(named.len() + 1), ("zz".into(), value));
            }
            Plant::Duplicate {
                argument,
                value,
                place,
            } => {
                let name = argument_name(argument);
                if !named.iter().any(|(given, _)| *given == name) {
                    named.push((name.clone(), value.clone()));
                }
                named.insert(place.index(named.len() + 1), (name, value));
            }
            Plant::Missing { argument } => {
                named.retain(|(given, _)| *given != argument_name(argument));
            }
            Plant::Value {
                argument,
                value,
                stage,
                code,
            } => {
                let name = argument_name(argument);
                match named.iter_mut().find(|(given, _)| *given == name) {
                    Some(given) => given.1 = value,
                    None => named.push((name, value)),
                }
                fault(code, stage, Some(argument));
            }
            Plant::Target {
                target: replaced,
                entry,
                code,
            } => {
                target = replaced;
                fault(code, Stage::Field(entry), None);
            }
        }
    }
    // The first pass: the given names, in the order given.
    let known: BTreeSet<String> = (0..layout.arguments.len()).map(argument_name).collect();
    let mut seen = BTreeSet::new();
    let index_of = |name: &str| (0..layout.arguments.len()).find(|i| argument_name(*i) == name);
    for (place, (name, _)) in named.iter().enumerate() {
        if !known.contains(name) {
            fault("unknown_argument", Stage::Name(place), None);
        } else if !seen.insert(name.clone()) {
            fault("duplicate_argument", Stage::Name(place), index_of(name));
        }
    }
    // The second pass's arguments left out without a default.
    for (index, (encoder, _)) in layout.arguments.iter().enumerate() {
        let name = argument_name(index);
        if encoder.default.is_none() && !named.iter().any(|(given, _)| *given == name) {
            fault("missing_argument", Stage::Argument(index), Some(index));
        }
    }
    let (stage, code) = faults
        .iter()
        .min_by_key(|fault| fault.stage)
        .map_or((Stage::Request, "none"), |fault| (fault.stage, fault.code));
    let at = match stage {
        Stage::Request => None,
        Stage::Name(place) => named.get(place).map(|(name, _)| name.clone()),
        Stage::Argument(index) => Some(argument_name(index)),
        Stage::Field(entry) | Stage::Length(entry) => entry_name(layout, entry),
    };
    Refused {
        set,
        command,
        arguments: named,
        target,
        code,
        at,
        faults,
    }
}

fn entry_name(layout: &Layout, entry: usize) -> Option<String> {
    match layout.entries.get(entry) {
        Some(Placed::Field { name, .. } | Placed::Checksum { name, .. }) => Some(name.clone()),
        _ => None,
    }
}

/// The index of the field whose value comes from `source`.
fn entry_of(layout: &Layout, wanted: impl Fn(&Source) -> bool) -> Option<usize> {
    layout
        .entries
        .iter()
        .position(|entry| matches!(entry, Placed::Field { source, .. } if wanted(source)))
}

/// Every fault the layout's command can be given, each with where it's
/// caught (a name's place is only known once it's planted).
fn plants(layout: &Layout) -> Vec<(Stage, BoxedStrategy<Plant>)> {
    let mut plants: Vec<(Stage, BoxedStrategy<Plant>)> = vec![
        (Stage::Request, Just(Plant::Set).boxed()),
        (Stage::Request, Just(Plant::Command).boxed()),
        (
            Stage::Name(0),
            (any::<bool>(), any::<Index>())
                .prop_map(|(flag, place)| Plant::Unknown {
                    value: Given::Bool(flag),
                    place,
                })
                .boxed(),
        ),
    ];
    for (argument, (encoder, encoded)) in layout.arguments.iter().enumerate() {
        let valid = match encoder.list {
            Some(bounds) => list_value(encoder, bounds.min.unwrap_or(0) as usize),
            None => value(encoder),
        };
        let valid = (valid, any::<u8>()).prop_map(|(value, how)| spell(&value, how));
        plants.push((
            Stage::Name(0),
            (valid, any::<Index>())
                .prop_map(move |(value, place)| Plant::Duplicate {
                    argument,
                    value,
                    place,
                })
                .boxed(),
        ));
        if encoder.default.is_none() {
            plants.push((
                Stage::Argument(argument),
                Just(Plant::Missing { argument }).boxed(),
            ));
        }
        // A list's values aren't given faults here.
        if encoder.list.is_some() {
            continue;
        }
        let refused_value = |values: BoxedStrategy<Given>, stage: Stage, code: &'static str| {
            let plant = values
                .prop_map(move |value| Plant::Value {
                    argument,
                    value,
                    stage,
                    code,
                })
                .boxed();
            (stage, plant)
        };
        for (values, code) in argument_faults(encoder) {
            plants.push(refused_value(values, Stage::Argument(argument), code));
        }
        if !encoded {
            if let Some(values) = not_default(encoder) {
                plants.push(refused_value(
                    values,
                    Stage::Argument(argument),
                    "not_default",
                ));
            }
            continue;
        }
        let Some(entry) = entry_of(
            layout,
            |source| matches!(source, Source::Argument(a) if *a == argument),
        ) else {
            continue;
        };
        for (values, stage, code) in field_faults(layout, encoder, entry) {
            plants.push(refused_value(values, stage, code));
        }
    }
    if let Some(entry) = entry_of(layout, |source| matches!(source, Source::Target))
        && let Some(Placed::Field { shape, .. }) = layout.entries.get(entry)
    {
        for (target, code) in target_faults(shape) {
            plants.push((
                Stage::Field(entry),
                Just(Plant::Target {
                    target,
                    entry,
                    code,
                })
                .boxed(),
            ));
        }
    }
    plants
}

/// An integer spelled exactly when it can be, else as a double as far
/// past the type as it is.
fn spell_integer(n: i128) -> Given {
    match (i64::try_from(n), u64::try_from(n)) {
        (Ok(n), _) => Given::Int(n),
        (_, Ok(n)) => Given::Uint(n),
        _ if n > 0 => Given::Float(4e19),
        _ => Given::Float(-1e19),
    }
}

/// Values §2.2 and §2.3 refuse for the argument, each with its code.
fn argument_faults(encoder: &Encoder) -> Vec<(BoxedStrategy<Given>, &'static str)> {
    let mut faults: Vec<(BoxedStrategy<Given>, &'static str)> = Vec::new();
    let wrong = match &encoder.ty {
        ArgType::F64 => select(vec![Given::Text("1".into()), Given::Bool(true)]).boxed(),
        ArgType::U64 | ArgType::I64 => select(vec![
            Given::Float(0.5),
            Given::Text("1".into()),
            Given::Bool(true),
        ])
        .boxed(),
        ArgType::Bool => select(vec![Given::Int(1), Given::Text("true".into())]).boxed(),
        ArgType::Str => Just(Given::Bytes(vec![0x61])).boxed(),
        ArgType::Bytes => Just(Given::Text("ab".into())).boxed(),
        ArgType::Enum(_) => Just(Given::Int(0)).boxed(),
    };
    faults.push((wrong, "wrong_type"));
    match &encoder.ty {
        ArgType::F64 => {
            let infinite = select(vec![f64::NAN, f64::INFINITY, f64::NEG_INFINITY]);
            faults.push((infinite.prop_map(Given::Float).boxed(), "non_finite"));
            if let Some(max) = encoder.max {
                faults.push((Just(Given::Float(max.next_up())).boxed(), "out_of_range"));
            }
            if let Some(min) = encoder.min {
                faults.push((Just(Given::Float(min.next_down())).boxed(), "out_of_range"));
            }
        }
        ArgType::U64 | ArgType::I64 => {
            let types = encoder.ty.integers();
            let (min, max) = encoder.integer_range;
            let high = max.unwrap_or(*types.end());
            let low = min.unwrap_or(*types.start());
            faults.push((Just(spell_integer(high + 1)).boxed(), "out_of_range"));
            faults.push((Just(spell_integer(low - 1)).boxed(), "out_of_range"));
        }
        ArgType::Enum(_) => {
            faults.push((Just(Given::Text("NOPE".into())).boxed(), "unknown_state"));
        }
        _ => {}
    }
    faults
}

/// Values other than its default, for an argument the set leaves out
/// (§2.4), when it has any.
fn not_default(encoder: &Encoder) -> Option<BoxedStrategy<Given>> {
    let default = encoder.default.clone()?;
    let possible = match &encoder.ty {
        ArgType::Enum(states) => states.len() >= 2,
        ArgType::U64 | ArgType::I64 => {
            let types = encoder.ty.integers();
            let (min, max) = encoder.integer_range;
            min.unwrap_or(*types.start()) < max.unwrap_or(*types.end())
        }
        ArgType::F64 => encoder
            .min
            .zip(encoder.max)
            .is_none_or(|(min, max)| min < max),
        _ => true,
    };
    possible.then(|| {
        (value(encoder), any::<u8>())
            .prop_filter("a value other than the default", move |(value, _)| {
                !same_value(value, &default)
            })
            .prop_map(|(value, how)| spell(&value, how))
            .boxed()
    })
}

/// Two values the same by value, as §2.2 compares them: -0.0 is 0.
fn same_value(a: &Arg, b: &Arg) -> bool {
    match (a, b) {
        (Arg::F64(a), Arg::F64(b)) => a == b,
        _ => a == b,
    }
}

/// Values the catalog allows that the argument's field refuses (§3), each
/// with where it's caught and its code.
fn field_faults(
    layout: &Layout,
    encoder: &Encoder,
    entry: usize,
) -> Vec<(BoxedStrategy<Given>, Stage, &'static str)> {
    let mut faults: Vec<(BoxedStrategy<Given>, Stage, &'static str)> = Vec::new();
    let at = Stage::Field(entry);
    // Written as letters other than a terminator.
    let letters = |n: usize, avoid: Option<u8>| -> Vec<u8> {
        let letter = if avoid == Some(b'q') { b'r' } else { b'q' };
        vec![letter; n]
    };
    let given = |bytes: Vec<u8>, text: bool| {
        if text {
            Given::Text(bytes.into_iter().map(char::from).collect())
        } else {
            Given::Bytes(bytes)
        }
    };
    let invertible = matches!(
        encoder.through,
        Through::Plain | Through::Exact(Numeric::Linear { .. })
    );
    match (&encoder.ty, &encoder.shape) {
        // An unranged f64 past binary32's largest (§3.1).
        (ArgType::F64, Shape::Float(32))
            if encoder.min.is_none() && encoder.max.is_none() && invertible =>
        {
            faults.push((
                select(vec![1e300, -1e300]).prop_map(Given::Float).boxed(),
                at,
                "raw_range",
            ));
        }
        (ArgType::Str, Shape::Text(_, len)) | (ArgType::Bytes, Shape::Bytes(len))
            if !matches!(len, Len::Remaining) =>
        {
            let text_value = matches!(encoder.ty, ArgType::Str);
            let encoding = match &encoder.shape {
                Shape::Text(text, _) => Some(*text),
                _ => None,
            };
            // A character its encoding can't write (§3.4).
            match encoding {
                Some(Text::Ascii) => {
                    faults.push((Just(Given::Text("éa".into())).boxed(), at, "invalid_text"));
                }
                Some(Text::Latin1) => {
                    faults.push((Just(Given::Text("€a".into())).boxed(), at, "invalid_text"));
                }
                _ => {}
            }
            // Its terminator (§3.4), when the text can hold it.
            if let Some(terminator) = len.terminator()
                && (!text_value || encoding == Some(Text::Latin1) || terminator < 0x80)
            {
                faults.push((
                    Just(given(vec![terminator], text_value)).boxed(),
                    at,
                    "contains_terminator",
                ));
            }
            match len {
                Len::Fixed(bytes) => {
                    let mut lengths = vec![bytes + 1];
                    if *bytes >= 1 {
                        lengths.push(bytes - 1);
                    }
                    faults.push((
                        select(lengths)
                            .prop_map(move |n| given(letters(n, None), text_value))
                            .boxed(),
                        at,
                        "bad_length",
                    ));
                }
                Len::Padded(bytes, terminator) => {
                    faults.push((
                        Just(given(letters(bytes + 1, Some(*terminator)), text_value)).boxed(),
                        at,
                        "bad_length",
                    ));
                }
                Len::Field { multiply, add, .. } => {
                    // A length the length field can't hold (§3.5), caught
                    // with the length fields, after every entry.
                    let field_bits = length_field_bits(layout, entry);
                    let mut lengths: Vec<usize> = Vec::new();
                    if *add >= 1 {
                        lengths.push((add - 1) as usize);
                    }
                    if *multiply >= 2 && *add >= -1 {
                        lengths.push((add + 1) as usize);
                    }
                    if let Some(bits) = field_bits.filter(|bits| *bits <= 8) {
                        let past = i64::from(*multiply) * (1_i64 << bits) + add;
                        lengths.push(past as usize);
                    }
                    if !lengths.is_empty() {
                        faults.push((
                            select(lengths)
                                .prop_map(move |n| given(letters(n, None), text_value))
                                .boxed(),
                            Stage::Length(entry),
                            "bad_length",
                        ));
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
    faults
}

/// The bits of the field that holds an entry's length, as it's laid out.
fn length_field_bits(layout: &Layout, entry: usize) -> Option<u32> {
    let measured = entry_name(layout, entry)?;
    layout.entries.iter().find_map(|placed| match placed {
        Placed::Field {
            shape: Shape::Uint(bits),
            source: Source::Length(name),
            ..
        } if *name == measured => Some(*bits),
        _ => None,
    })
}

/// Targets the target field refuses (§3.3, §3.4), each with its code.
fn target_faults(shape: &Shape) -> Vec<(Option<Target>, &'static str)> {
    let mut faults = vec![(None, "missing_target")];
    match shape {
        Shape::Uint(bits) | Shape::Int(bits) => {
            faults.push((Some(Target::Text("SN-x".into())), "target_type"));
            let signed = matches!(shape, Shape::Int(_));
            let past = if signed {
                1_i128 << (bits - 1)
            } else {
                1_i128 << bits
            };
            if past <= i128::from(u64::MAX) {
                faults.push((Some(Target::Int(past)), "raw_range"));
            }
        }
        Shape::Text(text, len) => {
            match text {
                Text::Ascii => faults.push((Some(Target::Text("éa".into())), "invalid_text")),
                Text::Latin1 => faults.push((Some(Target::Text("€a".into())), "invalid_text")),
                Text::Utf8 => {}
            }
            let letter = if len.terminator() == Some(b'q') {
                'r'
            } else {
                'q'
            };
            if let Len::Fixed(bytes) | Len::Padded(bytes, _) = len {
                let long: String = std::iter::repeat_n(letter, bytes + 1).collect();
                faults.push((Some(Target::Text(long)), "bad_length"));
            }
        }
        _ => {}
    }
    faults
}

// ---- Payloads --------------------------------------------------------------

/// A valid payload for the layout's command with one fault planted in it,
/// and the code decoding it gives. `keys` are every packet's key values.
pub(super) fn garbling(layout: Layout, keys: Vec<Const>) -> BoxedStrategy<Garbled> {
    (
        Just(layout.clone()),
        parts(&layout),
        any::<Index>(),
        any::<Index>(),
    )
        .prop_map(move |(layout, parts, which, detail)| {
            let values = values(&layout, &parts.given);
            let packed = pack(&layout, &values, parts.target.as_ref());
            let faults = garbled(&layout, &keys, &packed, &detail);
            let pick = which.index(faults.len());
            faults.into_iter().nth(pick).unwrap_or_else(|| Garbled {
                set: "fam_v9".into(),
                payload: packed.payload.clone(),
                encoding: Encoding::Binary,
                metadata: Vec::new(),
                code: "unknown_set",
                at: None,
                planted: "an unknown set".into(),
            })
        })
        .boxed()
}

/// Every fault the payload can be given, with the code each gives and the
/// argument or entry that error names.
fn garbled(layout: &Layout, keys: &[Const], packed: &Packed, detail: &Index) -> Vec<Garbled> {
    let metadata: Vec<(String, i64)> = layout
        .apid
        .map(|apid| vec![("apid".to_string(), apid)])
        .unwrap_or_default();
    let fault =
        |payload: Vec<u8>, code: &'static str, at: Option<String>, planted: String| Garbled {
            set: "fam_v1".into(),
            payload,
            encoding: Encoding::Binary,
            metadata: metadata.clone(),
            code,
            at,
            planted,
        };
    // The payload with raw bits written at an entry, its checksums worked
    // out again.
    let rewritten = |entry: usize, bits_value: u64| -> Option<Vec<u8>> {
        let (start, size) = packed.places.get(entry).copied().flatten()?;
        let Placed::Field { little, .. } = layout.entries.get(entry)? else {
            return None;
        };
        let mut payload = packed.payload.clone();
        put_bits(&mut payload, start, size as u32, bits_value, *little);
        write_checksums(layout, &mut payload, &packed.places);
        Some(payload)
    };
    let mut faults = vec![
        Garbled {
            set: "fam_v9".into(),
            ..fault(
                packed.payload.clone(),
                "unknown_set",
                None,
                "an unknown set".into(),
            )
        },
        Garbled {
            encoding: Encoding::Json,
            ..fault(
                b"{}".to_vec(),
                "wrong_encoding",
                None,
                "a JSON payload".into(),
            )
        },
        fault(Vec::new(), "no_packet", None, "an empty payload".into()),
    ];
    if !metadata.is_empty() {
        faults.push(Garbled {
            metadata: Vec::new(),
            ..fault(
                packed.payload.clone(),
                "no_packet",
                None,
                "no metadata".into(),
            )
        });
    }
    if let Some(payload) = unknown_key(layout, keys, packed) {
        faults.push(fault(
            payload,
            "no_packet",
            None,
            "a key no packet has".into(),
        ));
    }
    // A stored checksum changed (§5.6): no algorithm accepts its value
    // with one bit flipped. It's the checksum that fails first: one before
    // it covers only bytes before that one, and a later one is static only
    // when it is too, and static ones are verified in order of position.
    let sums: Vec<usize> = (0..layout.entries.len())
        .filter(|entry| matches!(layout.entries[*entry], Placed::Checksum { .. }))
        .collect();
    if !sums.is_empty() {
        let entry = sums[detail.index(sums.len())];
        if let (Some(Placed::Checksum { algorithm, .. }), Some((start, _))) =
            (layout.entries.get(entry), packed.places[entry])
        {
            let stored_at = (start / 8) as usize;
            let width = (algorithm.bits() / 8) as usize;
            let mut payload = packed.payload.clone();
            payload[stored_at + width - 1] ^= 1;
            faults.push(fault(
                payload,
                "bad_checksum",
                entry_name(layout, entry),
                format!("checksum {entry}"),
            ));
        }
    }
    // A constant that isn't a key, holding something else (§7.3).
    let loose: Vec<usize> = (0..layout.entries.len())
        .filter(|entry| {
            !layout.statics[*entry]
                && matches!(
                    layout.entries[*entry],
                    Placed::Field {
                        source: Source::Constant(_),
                        ..
                    }
                )
        })
        .collect();
    if !loose.is_empty() {
        let entry = loose[detail.index(loose.len())];
        if let Some(payload) = other_constant(layout, packed, entry) {
            faults.push(fault(
                payload,
                "wrong_constant",
                entry_name(layout, entry),
                format!("constant {entry}"),
            ));
        }
    }
    // Arguments' raw values decoding refuses (§7.4).
    for (entry, placed) in layout.entries.iter().enumerate() {
        let Placed::Field {
            shape,
            source: Source::Argument(index),
            ..
        } = placed
        else {
            continue;
        };
        let encoder = &layout.arguments[*index].0;
        for (raw, code) in refused_raws(encoder, shape) {
            if let Some(payload) = rewritten(entry, raw) {
                faults.push(fault(
                    payload,
                    code,
                    Some(argument_name(*index)),
                    format!("raw {raw:#x} at {entry}"),
                ));
            }
        }
    }
    // Cut short inside an entry after the keys (§7.3): every constant at a
    // place each payload has is a key (§7.2), which a cut would leave
    // unread, so that no packet matched. The cut is at the entry's start or
    // past it, so no entry before it is cut short.
    let key_bytes = (0..layout.entries.len())
        .filter(|entry| {
            layout.statics[*entry]
                && matches!(
                    layout.entries[*entry],
                    Placed::Field {
                        source: Source::Constant(_),
                        ..
                    }
                )
        })
        .filter_map(|entry| packed.places[entry])
        .map(|(start, size)| (start + size).div_ceil(8))
        .max()
        .unwrap_or(1);
    let cuts: Vec<(u64, u64)> = (1..layout.entries.len())
        .filter_map(|entry| {
            let fixed = match &layout.entries[entry] {
                Placed::Field { shape, .. } => shape.bits().is_some(),
                Placed::Checksum { .. } => true,
                Placed::Padding(_) | Placed::Array { .. } => false,
            };
            let (start, size) = packed.places[entry]?;
            let low = key_bytes.max(start.div_ceil(8));
            let high = (start + size).div_ceil(8).checked_sub(1)?;
            (fixed && size > 0 && low <= high).then_some((low, high))
        })
        .collect();
    if !cuts.is_empty() {
        let (low, high) = cuts[detail.index(cuts.len())];
        let length = (low + (high - low) / 2) as usize;
        faults.push(fault(
            packed.payload[..length].to_vec(),
            "past_end",
            cut_entry(layout, packed, 8 * length as u64),
            format!("cut to {length} bytes"),
        ));
    }
    faults
}

/// The entry a payload cut to `bits` fails at (§5.6): the first, in
/// order, that reaches past the cut. A static checksum past it isn't a
/// problem when static checksums are verified; the walk finds it in order.
fn cut_entry(layout: &Layout, packed: &Packed, bits: u64) -> Option<String> {
    let past = |entry: usize| packed.places[entry].is_some_and(|(start, size)| start + size > bits);
    (0..layout.entries.len())
        .find(|&entry| past(entry))
        .and_then(|entry| entry_name(layout, entry))
}

/// The payload with its key holding a value no packet has.
fn unknown_key(layout: &Layout, keys: &[Const], packed: &Packed) -> Option<Vec<u8>> {
    let (start, size) = packed.places.first().copied().flatten()?;
    let Placed::Field { shape, little, .. } = layout.entries.first()? else {
        return None;
    };
    let mut payload = packed.payload.clone();
    match shape {
        Shape::Uint(bits) | Shape::Int(bits) => {
            let unused = shape
                .integers()?
                .find(|n| !keys.contains(&Const::Int(*n)))?;
            put_bits(
                &mut payload,
                start,
                size as u32,
                fit(unused as u64, *bits),
                *little,
            );
        }
        Shape::Text(_, len) => {
            // Tags are letters a to j, so z's are no tag.
            let length = keys.iter().find_map(|key| match key {
                Const::Text(text) => Some(text.len()),
                _ => None,
            })?;
            let bytes = buffer(vec![b'z'; length], len);
            let first = (start / 8) as usize;
            payload[first..first + bytes.len()].copy_from_slice(&bytes);
        }
        _ => return None,
    }
    Some(payload)
}

/// The payload with a constant holding another value, its checksums
/// worked out again.
fn other_constant(layout: &Layout, packed: &Packed, entry: usize) -> Option<Vec<u8>> {
    let (start, size) = packed.places.get(entry).copied().flatten()?;
    let Placed::Field {
        shape,
        little,
        source: Source::Constant(value),
        ..
    } = layout.entries.get(entry)?
    else {
        return None;
    };
    let mut payload = packed.payload.clone();
    match (shape, value) {
        (Shape::Text(_, len), Const::Text(text)) => {
            let letter = b"qrs".iter().copied().find(|letter| {
                Some(*letter) != len.terminator() && text.as_bytes().first() != Some(letter)
            })?;
            let bytes = match len {
                Len::Fixed(n) => vec![letter; *n],
                _ => buffer(vec![letter], len),
            };
            let first = (start / 8) as usize;
            payload[first..first + bytes.len()].copy_from_slice(&bytes);
        }
        (_, Const::Int(n)) => {
            let bits = size as u32;
            put_bits(&mut payload, start, bits, fit(*n as u64, bits) ^ 1, *little);
        }
        (_, Const::Bool(flag)) => {
            put_bits(&mut payload, start, size as u32, u64::from(!flag), *little);
        }
        _ => return None,
    }
    write_checksums(layout, &mut payload, &packed.places);
    Some(payload)
}

/// Raw values an argument's field holds that decoding refuses (§7.4), as
/// the bits the field writes, each with its code.
fn refused_raws(encoder: &Encoder, shape: &Shape) -> Vec<(u64, &'static str)> {
    let mut raws = Vec::new();
    let bits = shape.bits().unwrap_or(0);
    let held = shape.integers();
    let integer = |n: i128| -> Option<u64> {
        let range = held.clone()?;
        range.contains(&n).then(|| fit(n as u64, bits))
    };
    match (&encoder.ty, &encoder.through) {
        // A state's raw value the conversion doesn't list.
        (ArgType::Enum(_), Through::States(states)) => {
            let listed: Vec<i128> = states.iter().map(|(raw, _)| *raw).collect();
            if let Some(range) = held.clone()
                && let Some(unlisted) = range.take(64).find(|raw| !listed.contains(raw))
            {
                raws.extend(integer(unlisted).map(|raw| (raw, "unknown_state")));
            }
        }
        // 2, which no bool is.
        (ArgType::Bool, _) => raws.extend(integer(2).map(|raw| (raw, "out_of_range"))),
        // Past its range or its type.
        (ArgType::U64 | ArgType::I64, _) => {
            let types = encoder.ty.integers();
            let (min, max) = encoder.integer_range;
            let high = max.unwrap_or(*types.end());
            let low = min.unwrap_or(*types.start());
            let past = integer(high + 1).or_else(|| integer(low - 1));
            raws.extend(past.map(|raw| (raw, "out_of_range")));
        }
        (ArgType::F64, _) => {
            let float = |x: f64| match shape {
                Shape::Float(32) => u64::from((x as f32).to_bits()),
                _ => x.to_bits(),
            };
            if matches!(shape, Shape::Float(_)) {
                for infinite in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                    raws.push((float(infinite), "non_finite"));
                }
            }
            // One past what the range's ends encode to.
            if let (Some(min), Some(max)) = (encoder.min, encoder.max)
                && let (Some(a), Some(b)) = (unrounded(encoder, min), unrounded(encoder, max))
            {
                let (low, high) = (a.min(b), a.max(b));
                match shape {
                    Shape::Float(32) => {
                        let past = f64::from((high as f32).next_up());
                        if past.is_finite() {
                            raws.push((float(past), "out_of_range"));
                        }
                    }
                    Shape::Float(_) => raws.push((float(high.next_up()), "out_of_range")),
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
