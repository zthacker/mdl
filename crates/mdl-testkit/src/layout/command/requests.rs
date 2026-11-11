//! Requests, the payload each encodes to (`spec/encoding.md` §3, §4), and
//! what decoding that payload gives back (§7.4).

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::select;

use super::values::{list_value, spell, text_for, to_argument, unrounded, value, value_of};
use super::{
    Arg, ArgType, Const, Counted, Decoded, Elements, Encoder, Given, Layout, Len, Placed, Request,
    Shape, Source, Target, Through,
};
use crate::layout::{Raw, convert, encode, fit, put_bits};

/// A valid request, before what it encodes to is worked out: each
/// argument's spelled value, if given, by catalog index; the target; and
/// the given arguments in the order the request gives them.
#[derive(Debug, Clone)]
pub(super) struct Parts {
    pub(super) given: Vec<Option<Given>>,
    pub(super) target: Option<Target>,
    pub(super) named: Vec<(String, Given)>,
}

/// A request for a packet's command, with the payload it encodes to and
/// what decoding that payload gives back.
pub(super) fn request(layout: Layout) -> BoxedStrategy<Request> {
    (Just(layout.clone()), parts(&layout))
        .prop_map(|(layout, parts)| outcome(&layout, &parts.given, parts.target, parts.named))
        .boxed()
}

/// A valid request's parts: values the catalog allows, arguments with
/// defaults sometimes left out, in any order. The lists of a group take one
/// count, their defaults' when the request leaves one of them out, each
/// list as long as that count makes its array.
pub(super) fn parts(layout: &Layout) -> BoxedStrategy<Parts> {
    let choices: Vec<BoxedStrategy<(i64, Vec<bool>)>> = layout
        .groups
        .iter()
        .map(|group| {
            let leave = vec(any::<bool>(), group.members.len());
            (select(group.counts.clone()), leave).boxed()
        })
        .collect();
    let target = match layout.target() {
        Some(shape) => target_value(shape).prop_map(Some).boxed(),
        None => Just(None).boxed(),
    };
    (Just(layout.clone()), choices, target)
        .prop_flat_map(|(layout, choices, target)| {
            let given: Vec<BoxedStrategy<Option<Given>>> = (0..layout.arguments.len())
                .map(|index| given_value(&layout, &choices, index))
                .collect();
            (given, Just(target))
        })
        .prop_flat_map(|(given, target)| {
            let named: Vec<(String, Given)> = given
                .iter()
                .enumerate()
                .filter_map(|(index, given)| {
                    given.clone().map(|given| (format!("a{index}"), given))
                })
                .collect();
            (Just(given), Just(target), Just(named).prop_shuffle())
        })
        .prop_map(|(given, target, named)| Parts {
            given,
            target,
            named,
        })
        .boxed()
}

/// The value a request gives an argument, or none: a list of the length its
/// group's count gives it, unless the request leaves it out.
fn given_value(
    layout: &Layout,
    choices: &[(i64, Vec<bool>)],
    index: usize,
) -> BoxedStrategy<Option<Given>> {
    let (encoder, encoded) = &layout.arguments[index];
    let spelled = |values: BoxedStrategy<Arg>| {
        (values, any::<u8>())
            .prop_map(|(value, how)| spell(&value, how))
            .boxed()
    };
    let in_group = layout
        .groups
        .iter()
        .zip(choices)
        .find_map(|(group, choice)| {
            let place = group
                .members
                .iter()
                .position(|(member, _)| *member == index)?;
            Some((group, choice, place))
        });
    if let Some((group, (count, leave), place)) = in_group {
        // A list with a default may be left out, and takes it; the group's
        // other lists then take the count it gives.
        let left_out = |at: usize| {
            let (member, _) = group.members[at];
            leave[at] && layout.arguments[member].0.default.is_some()
        };
        if left_out(place) {
            return Just(None).boxed();
        }
        let count = match group.default {
            Some(default) if (0..group.members.len()).any(left_out) => default,
            _ => *count,
        };
        let (_, scale) = group.members[place];
        return spelled(list_value(encoder, scale.length(count)))
            .prop_map(Some)
            .boxed();
    }
    match (&encoder.default, *encoded) {
        (None, _) => spelled(value(encoder)).prop_map(Some).boxed(),
        (Some(_), true) => proptest::option::of(spelled(value(encoder))).boxed(),
        // A set that leaves an argument out takes only its default.
        (Some(default), false) => {
            proptest::option::of(spelled(Just(default.clone()).boxed())).boxed()
        }
    }
}

/// A native id a target field holds: an integer it holds (maybe spelled as
/// text), or text it takes, which may spell an integer.
fn target_value(shape: &Shape) -> BoxedStrategy<Target> {
    match shape {
        Shape::Text(text, len) => {
            let avoid = len.terminator();
            let integer = len.lengths().prop_flat_map(decimal).prop_filter_map(
                "an integer the buffer takes",
                move |digits| {
                    let fits = !avoid.is_some_and(|byte| digits.as_bytes().contains(&byte));
                    match native(&digits) {
                        Target::Int(n) if fits => Some(n),
                        _ => None,
                    }
                },
            );
            prop_oneof![
                text_for(*text, len).prop_map(Target::Text),
                (integer, any::<bool>()).prop_map(|(n, as_text)| spelled_id(n, as_text)),
            ]
            .boxed()
        }
        _ => {
            let held = shape.integers().unwrap_or(0..=0);
            (held, any::<bool>())
                .prop_map(|(n, as_text)| spelled_id(n, as_text))
                .boxed()
        }
    }
}

fn spelled_id(n: i128, as_text: bool) -> Target {
    if as_text {
        Target::Text(n.to_string())
    } else {
        Target::Int(n)
    }
}

/// An integer's decimal spelling of exactly `n` characters, maybe negative.
fn decimal(n: usize) -> BoxedStrategy<String> {
    (any::<bool>(), any::<u64>())
        .prop_map(move |(negative, seed)| {
            let negative = negative && n >= 2;
            let digits = if negative { n - 1 } else { n };
            if digits == 0 || digits > 19 {
                return String::new();
            }
            let least = if digits == 1 {
                u64::from(negative)
            } else {
                10_u64.pow(digits as u32 - 1)
            };
            let most = 10_u128.pow(digits as u32) - 1;
            let span = most - u128::from(least) + 1;
            let magnitude = u128::from(least) + u128::from(seed) % span;
            if negative {
                format!("-{magnitude}")
            } else {
                magnitude.to_string()
            }
        })
        .boxed()
}

/// A native id as text (`spec/bindings.md` §3): an integer when it's in an
/// integer's one decimal spelling and between −2^63 and 2^64 − 1.
pub(super) fn native(text: &str) -> Target {
    let digits = text.strip_prefix('-').unwrap_or(text);
    let canonical = !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && (digits == "0" || !digits.starts_with('0'))
        && text != "-0";
    match text.parse::<i128>() {
        Ok(n) if canonical && (i128::from(i64::MIN)..=i128::from(u64::MAX)).contains(&n) => {
            Target::Int(n)
        }
        _ => Target::Text(text.to_string()),
    }
}

/// What a request encodes to, and what decoding that gives back.
fn outcome(
    layout: &Layout,
    given: &[Option<Given>],
    target: Option<Target>,
    named: Vec<(String, Given)>,
) -> Request {
    let values = values(layout, given);
    let packed = pack(layout, &values, target.as_ref());
    let decoded_values: Vec<Option<Arg>> = layout
        .arguments
        .iter()
        .zip(&packed.arguments)
        .map(|((encoder, _), raw)| raw.as_ref().map(|raw| decoded_value(encoder, raw)))
        .collect();
    let arguments: Vec<(String, Arg)> = layout
        .arguments
        .iter()
        .zip(&decoded_values)
        .enumerate()
        .filter_map(|(index, ((encoder, encoded), decoded))| {
            let value = if *encoded {
                decoded.clone()
            } else {
                encoder.default.clone()
            };
            value.map(|value| (format!("a{index}"), value))
        })
        .collect();
    let decoded_target = packed.target.as_ref().map(|raw| match raw {
        Raw::Uint(n) => Target::Int(i128::from(*n)),
        Raw::Int(n) => Target::Int(i128::from(*n)),
        Raw::Str(text) => native(text),
        other => Target::Text(format!("not generated: {other:?}")),
    });
    let again = pack(layout, &decoded_values, decoded_target.as_ref()).payload;
    let names = |pick: &dyn Fn(usize, bool) -> bool| -> Vec<String> {
        layout
            .arguments
            .iter()
            .enumerate()
            .filter(|(index, (_, encoded))| pick(*index, *encoded))
            .map(|(index, _)| format!("a{index}"))
            .collect()
    };
    Request {
        command: layout.command.clone(),
        arguments: named,
        target,
        metadata: layout
            .apid
            .map(|apid| vec![("apid".to_string(), apid)])
            .unwrap_or_default(),
        payload: packed.payload,
        record: packed.record,
        defaulted: names(&|index, _| given[index].is_none()),
        decoded: Decoded {
            arguments,
            defaulted: names(&|_, encoded| !encoded),
            target: decoded_target,
        },
        again,
    }
}

/// Each encoded argument's value, given or its default, by catalog index.
pub(super) fn values(layout: &Layout, given: &[Option<Given>]) -> Vec<Option<Arg>> {
    layout
        .arguments
        .iter()
        .zip(given)
        .map(|((encoder, encoded), given)| match (encoded, given) {
            (false, _) => None,
            (true, Some(given)) => Some(value_of(given, &encoder.ty)),
            (true, None) => encoder.default.clone(),
        })
        .collect()
}

/// A payload and what its fields hold.
pub(super) struct Packed {
    pub(super) payload: Vec<u8>,
    pub(super) record: Vec<(String, Raw)>,
    /// Each encoded argument's raw value, by argument index.
    pub(super) arguments: Vec<Option<Raw>>,
    pub(super) target: Option<Raw>,
    /// Each field's and checksum's place in the payload, by entry index:
    /// its first bit and how many bits it takes.
    pub(super) places: Vec<Option<(u64, u64)>>,
}

/// Packs a packet (`spec/encoding.md` §3, §4): each field's raw value, an
/// array's elements or records, a length field's from the field it
/// measures and a count field's from the arrays it counts, written bit by
/// bit in place, then each checksum in order of position over the bytes it
/// covers.
pub(super) fn pack(layout: &Layout, values: &[Option<Arg>], target: Option<&Target>) -> Packed {
    let field_raw = |shape: &Shape, source: &Source, item: Option<usize>| match source {
        Source::Constant(value) => Some(constant_raw(shape, value)),
        Source::Argument(index) => {
            let value = values[*index].as_ref()?;
            let value = match (value, item) {
                (Arg::List(items), Some(item)) => items.get(item)?,
                (value, _) => value,
            };
            Some(raw_of(&layout.arguments[*index].0, shape, value))
        }
        Source::Target => target.map(|target| target_raw(shape, target)),
        Source::Length(_) | Source::Count => None,
    };
    let mut raws: Vec<Option<Raw>> = layout
        .entries
        .iter()
        .map(|entry| match entry {
            Placed::Field { shape, source, .. } => field_raw(shape, source, None),
            Placed::Array { elements, .. } => Some(Raw::Array(match elements {
                Elements::Scalar {
                    argument, shape, ..
                } => match &values[*argument] {
                    Some(Arg::List(items)) => items
                        .iter()
                        .map(|item| raw_of(&layout.arguments[*argument].0, shape, item))
                        .collect(),
                    _ => Vec::new(),
                },
                Elements::Records { entries, .. } => {
                    // As many records as its lists have values, which is
                    // one length.
                    let count = entries
                        .iter()
                        .find_map(|entry| match entry {
                            Placed::Field {
                                source: Source::Argument(index),
                                ..
                            } => match &values[*index] {
                                Some(Arg::List(items)) => Some(items.len()),
                                _ => None,
                            },
                            _ => None,
                        })
                        .unwrap_or(0);
                    (0..count)
                        .map(|item| {
                            Raw::Record(
                                entries
                                    .iter()
                                    .filter_map(|entry| match entry {
                                        Placed::Field {
                                            name,
                                            shape,
                                            source,
                                            ..
                                        } => Some((
                                            name.clone(),
                                            field_raw(shape, source, Some(item))?,
                                        )),
                                        _ => None,
                                    })
                                    .collect(),
                            )
                        })
                        .collect()
                }
            })),
            _ => None,
        })
        .collect();
    // A count field holds the number of elements of the arrays it counts,
    // which is one number.
    for k in 0..layout.entries.len() {
        let Placed::Field {
            name: field,
            source: Source::Count,
            ..
        } = &layout.entries[k]
        else {
            continue;
        };
        let counted =
            layout
                .entries
                .iter()
                .zip(&raws)
                .find_map(|(entry, raw)| match (entry, raw) {
                    (
                        Placed::Array {
                            count:
                                Counted::Field {
                                    name,
                                    multiply,
                                    add,
                                },
                            ..
                        },
                        Some(Raw::Array(items)),
                    ) if name == field => Some((items.len() as i64 - add) / i64::from(*multiply)),
                    _ => None,
                });
        raws[k] = counted.map(|raw| Raw::Uint(raw as u64));
    }
    // A length field holds the byte length of the field it names.
    for k in 0..layout.entries.len() {
        let Placed::Field {
            source: Source::Length(measured),
            ..
        } = &layout.entries[k]
        else {
            continue;
        };
        let named = layout
            .entries
            .iter()
            .position(|entry| matches!(entry, Placed::Field { name, .. } if name == measured));
        if let Some(j) = named
            && let Placed::Field { shape, .. } = &layout.entries[j]
            && let (Some(Len::Field { multiply, add, .. }), Some(raw)) =
                (shape.len(), raws[j].clone())
        {
            let n = value_bytes(&raw, shape).len() as i64;
            raws[k] = Some(Raw::Uint(((n - add) / i64::from(*multiply)) as u64));
        }
    }
    let mut payload = Vec::new();
    let mut at = 0_u64;
    let mut places = vec![None; layout.entries.len()];
    for (k, entry) in layout.entries.iter().enumerate() {
        let start = at;
        match (entry, &raws[k]) {
            (Placed::Padding(bits), _) => at += bits,
            (Placed::Field { shape, little, .. }, Some(raw)) => {
                at += write(&mut payload, at, shape, *little, raw);
            }
            (Placed::Field { .. }, None) => continue,
            (Placed::Checksum { algorithm, .. }, _) => {
                let size = u64::from(algorithm.bits());
                grow(&mut payload, at + size);
                at += size;
            }
            (Placed::Array { elements, .. }, Some(Raw::Array(items))) => {
                for item in items {
                    match (elements, item) {
                        (Elements::Scalar { shape, little, .. }, raw) => {
                            at += write(&mut payload, at, shape, *little, raw)
                        }
                        (Elements::Records { entries, .. }, Raw::Record(fields)) => {
                            // A record's entries in order, from its start,
                            // each field's raw value found by its name.
                            for entry in entries {
                                match entry {
                                    Placed::Padding(bits) => at += bits,
                                    Placed::Field {
                                        name,
                                        shape,
                                        little,
                                        ..
                                    } => {
                                        let raw = fields.iter().find(|(field, _)| field == name);
                                        if let Some((_, raw)) = raw {
                                            at += write(&mut payload, at, shape, *little, raw);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            (Placed::Array { .. }, _) => continue,
        }
        places[k] = Some((start, at - start));
    }
    grow(&mut payload, at);
    for (k, value) in write_checksums(layout, &mut payload, &places) {
        raws[k] = Some(Raw::Uint(value));
    }
    let mut record = Vec::new();
    let mut arguments = vec![None; layout.arguments.len()];
    let mut target_raw = None;
    for (entry, raw) in layout.entries.iter().zip(raws) {
        let (name, raw) = match (entry, raw) {
            (Placed::Field { name, source, .. }, Some(raw)) => {
                match source {
                    Source::Argument(index) => arguments[*index] = Some(raw.clone()),
                    Source::Target => target_raw = Some(raw.clone()),
                    _ => {}
                }
                (name, raw)
            }
            (Placed::Checksum { name, .. }, Some(raw)) => (name, raw),
            (Placed::Array { name, elements, .. }, Some(raw)) => {
                if let Raw::Array(items) = &raw {
                    lists(elements, items, &mut arguments);
                }
                (name, raw)
            }
            _ => continue,
        };
        record.push((name.clone(), raw));
    }
    Packed {
        payload,
        record,
        arguments,
        target: target_raw,
        places,
    }
}

/// Writes each checksum, in order of position, over the bytes it covers
/// as they are by then (`spec/encoding.md` §4), and gives each one's value
/// by entry index.
pub(super) fn write_checksums(
    layout: &Layout,
    payload: &mut [u8],
    places: &[Option<(u64, u64)>],
) -> Vec<(usize, u64)> {
    let mut values = Vec::new();
    for (k, entry) in layout.entries.iter().enumerate() {
        if let (
            Placed::Checksum {
                algorithm,
                from_bits,
                ..
            },
            Some((start, _)),
        ) = (entry, places[k])
        {
            let (start, from) = ((start / 8) as usize, (from_bits / 8) as usize);
            let value = algorithm.compute(&payload[from..start]);
            let stored = algorithm.stored(value, layout.little);
            payload[start..start + stored.len()].copy_from_slice(&stored);
            values.push((k, value));
        }
    }
    values
}

/// Each list argument an array holds, as the raw values it takes: a scalar
/// array's elements, or a record field's value in each record.
fn lists(elements: &Elements, items: &[Raw], arguments: &mut [Option<Raw>]) {
    match elements {
        Elements::Scalar { argument, .. } => {
            arguments[*argument] = Some(Raw::Array(items.to_vec()))
        }
        Elements::Records { entries, .. } => {
            for entry in entries {
                if let Placed::Field {
                    name,
                    source: Source::Argument(index),
                    ..
                } = entry
                {
                    let values = items
                        .iter()
                        .filter_map(|item| match item {
                            Raw::Record(fields) => fields
                                .iter()
                                .find(|(field, _)| field == name)
                                .map(|(_, raw)| raw.clone()),
                            _ => None,
                        })
                        .collect();
                    arguments[*index] = Some(Raw::Array(values));
                }
            }
        }
    }
}

/// Writes a raw value at `at` as a field of `shape` writes it, and gives
/// the bits it takes.
fn write(payload: &mut Vec<u8>, at: u64, shape: &Shape, little: bool, raw: &Raw) -> u64 {
    match shape {
        Shape::Text(_, len) | Shape::Bytes(len) => {
            let bytes = buffer(value_bytes(raw, shape), len);
            grow(payload, at + 8 * bytes.len() as u64);
            let first = (at / 8) as usize;
            payload[first..first + bytes.len()].copy_from_slice(&bytes);
            8 * bytes.len() as u64
        }
        _ => {
            let size = shape.bits().unwrap_or(0);
            grow(payload, at + u64::from(size));
            put_bits(payload, at, size, pattern(raw, shape), little);
            u64::from(size)
        }
    }
}

/// Makes the payload at least `bits` long, in whole bytes of zeros.
fn grow(payload: &mut Vec<u8>, bits: u64) {
    let bytes = bits.div_ceil(8) as usize;
    if payload.len() < bytes {
        payload.resize(bytes, 0);
    }
}

/// An integer, bool or float raw value as the bits a field writes.
fn pattern(raw: &Raw, shape: &Shape) -> u64 {
    match (raw, shape) {
        (Raw::Uint(n), _) => *n,
        (Raw::Int(n), Shape::Int(bits)) => fit(*n as u64, *bits),
        (Raw::Bool(b), _) => u64::from(*b),
        (Raw::Float(x), Shape::Float(32)) => u64::from((*x as f32).to_bits()),
        (Raw::Float(x), _) => x.to_bits(),
        _ => 0,
    }
}

/// A string's or bytes value's bytes, before its length's padding.
fn value_bytes(raw: &Raw, shape: &Shape) -> Vec<u8> {
    match (raw, shape) {
        (Raw::Str(value), Shape::Text(text, _)) => encode(value, *text),
        (Raw::Bytes(bytes), _) => bytes.clone(),
        _ => Vec::new(),
    }
}

/// A value's bytes as its length writes them (§3.4): a terminator padding
/// a fixed buffer, or written after the value.
pub(super) fn buffer(mut bytes: Vec<u8>, len: &Len) -> Vec<u8> {
    match len {
        Len::Padded(size, byte) => bytes.resize(*size, *byte),
        Len::Terminator(byte) => bytes.push(*byte),
        _ => {}
    }
    bytes
}

fn constant_raw(shape: &Shape, value: &Const) -> Raw {
    match (shape, value) {
        (Shape::Int(_), Const::Int(n)) => Raw::Int(*n as i64),
        (_, Const::Int(n)) => Raw::Uint(*n as u64),
        (_, Const::Bool(b)) => Raw::Bool(*b),
        (_, Const::Text(text)) => Raw::Str(text.clone()),
    }
}

/// The raw value an argument's value becomes in its field (§3.1, §3.4).
pub(super) fn raw_of(encoder: &Encoder, shape: &Shape, value: &Arg) -> Raw {
    let integer = |n: i128| match shape {
        Shape::Int(_) => Raw::Int(n as i64),
        Shape::Bool => Raw::Bool(n != 0),
        _ => Raw::Uint(n as u64),
    };
    match value {
        Arg::Bool(b) => integer(i128::from(*b)),
        Arg::U64(n) => integer(i128::from(*n)),
        Arg::I64(n) => integer(i128::from(*n)),
        Arg::F64(v) => {
            let x = unrounded(encoder, *v).unwrap_or(f64::NAN);
            match shape {
                Shape::Float(32) => Raw::Float(f64::from(x as f32)),
                Shape::Float(_) => Raw::Float(x),
                // Ties away from zero.
                _ => integer(x.round() as i128),
            }
        }
        Arg::Text(name) => match &encoder.through {
            Through::States(states) => states
                .iter()
                .find(|(_, state)| state == name)
                .map_or(Raw::Str(name.clone()), |(raw, _)| integer(*raw)),
            _ => Raw::Str(name.clone()),
        },
        Arg::Bytes(bytes) => Raw::Bytes(bytes.clone()),
        Arg::List(items) => Raw::Array(
            items
                .iter()
                .map(|item| raw_of(encoder, shape, item))
                .collect(),
        ),
    }
}

/// A target field's raw value (§3.3): the id's integer, or its text, an
/// integer written in decimal.
fn target_raw(shape: &Shape, target: &Target) -> Raw {
    let id = match target {
        Target::Text(text) => native(text),
        Target::Int(n) => Target::Int(*n),
    };
    match (shape, id) {
        (Shape::Int(_), Target::Int(n)) => Raw::Int(n as i64),
        (Shape::Uint(_), Target::Int(n)) => Raw::Uint(n as u64),
        (_, Target::Int(n)) => Raw::Str(n.to_string()),
        (_, Target::Text(text)) => Raw::Str(text),
    }
}

/// What an argument's raw value decodes to (`spec/encoding.md` §7.4): an
/// f64 through the field's conversion, into the argument's unit, held to
/// its range.
pub(super) fn decoded_value(encoder: &Encoder, raw: &Raw) -> Arg {
    // A list argument, each element as a value of the argument's type.
    if let Raw::Array(items) = raw {
        return Arg::List(
            items
                .iter()
                .map(|item| decoded_value(encoder, item))
                .collect(),
        );
    }
    let integer = match raw {
        Raw::Uint(n) => i128::from(*n),
        Raw::Int(n) => i128::from(*n),
        Raw::Bool(b) => i128::from(*b),
        _ => 0,
    };
    match &encoder.ty {
        ArgType::Bool => Arg::Bool(integer != 0),
        ArgType::U64 => Arg::U64(integer as u64),
        ArgType::I64 => Arg::I64(integer as i64),
        ArgType::F64 => {
            let r = match raw {
                Raw::Float(x) => *x,
                _ => integer as f64,
            };
            let x = match &encoder.through {
                Through::Exact(conversion) | Through::Inverse { conversion, .. } => {
                    convert(conversion, r).unwrap_or(f64::NAN)
                }
                _ => r,
            };
            let mut value = to_argument(encoder, x);
            if let Some(max) = encoder.max
                && value > max
            {
                value = max;
            }
            if let Some(min) = encoder.min
                && value < min
            {
                value = min;
            }
            Arg::F64(value)
        }
        ArgType::Enum(_) => match (&encoder.through, raw) {
            (Through::States(states), _) => Arg::Text(
                states
                    .iter()
                    .find(|(state_raw, _)| *state_raw == integer)
                    .map(|(_, name)| name.clone())
                    .unwrap_or_default(),
            ),
            (_, Raw::Str(text)) => Arg::Text(text.clone()),
            _ => Arg::Text(String::new()),
        },
        ArgType::Str => match raw {
            Raw::Str(text) => Arg::Text(text.clone()),
            _ => Arg::Text(String::new()),
        },
        ArgType::Bytes => match raw {
            Raw::Bytes(bytes) => Arg::Bytes(bytes.clone()),
            _ => Arg::Bytes(Vec::new()),
        },
    }
}
