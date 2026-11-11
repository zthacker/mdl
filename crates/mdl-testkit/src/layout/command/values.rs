//! Values a request gives an argument, the ways it spells them (§2.2),
//! and the raw value each becomes before rounding (§3.1).

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::select;
use proptest::strategy::Union;

use super::{Arg, ArgType, Encoder, Given, Len, Shape, Through};
use crate::layout::{Numeric, Text, convert, convert_unit, encode, table};

/// Values the catalog lets a caller give the argument and its field holds
/// (`spec/encoding.md` §2, §3).
pub(super) fn value(encoder: &Encoder) -> BoxedStrategy<Arg> {
    match (&encoder.ty, &encoder.shape) {
        (ArgType::Bool, _) => any::<bool>().prop_map(Arg::Bool).boxed(),
        (ArgType::U64 | ArgType::I64, _) => {
            let (low, high) = integer_span(encoder);
            let unsigned = encoder.ty == ArgType::U64;
            // Near each end, an integer a double holds, which a request
            // may spell as that double.
            let near_low = (double_at_least(low) as i128).min(high);
            let near_high = (double_at_most(high) as i128).max(low);
            prop_oneof![
                Just(low),
                Just(high),
                Just(near_low),
                Just(near_high),
                low..=high
            ]
            .prop_map(move |n| {
                if unsigned {
                    Arg::U64(n as u64)
                } else {
                    Arg::I64(n as i64)
                }
            })
            .boxed()
        }
        (ArgType::F64, _) => f64_values(encoder),
        (ArgType::Enum(states), _) => select(states.clone()).prop_map(Arg::Text).boxed(),
        (ArgType::Str, Shape::Text(text, len)) => text_for(*text, len).prop_map(Arg::Text).boxed(),
        (ArgType::Bytes, Shape::Bytes(len)) => bytes_for(len).prop_map(Arg::Bytes).boxed(),
        (ArgType::Str, _) => Just(Arg::Text(String::new())).boxed(),
        (ArgType::Bytes, _) => Just(Arg::Bytes(Vec::new())).boxed(),
    }
}

/// The integers a u64 or i64 argument can take: its range's, the type's
/// and the field's.
fn integer_span(encoder: &Encoder) -> (i128, i128) {
    let field = encoder.shape.integers().unwrap_or(0..=0);
    let types = encoder.ty.integers();
    let mut low = (*field.start()).max(*types.start());
    let mut high = (*field.end()).min(*types.end());
    if let Some(min) = encoder.min {
        low = low.max(min.ceil() as i128);
    }
    if let Some(max) = encoder.max {
        high = high.min(max.floor() as i128);
    }
    (low, high)
}

/// Values in an f64 argument's range: its ends, anything between, -0.0,
/// and values whose raw value falls halfway between two the field holds.
fn f64_values(encoder: &Encoder) -> BoxedStrategy<Arg> {
    let low = encoder
        .min
        .unwrap_or(encoder.max.map_or(-1e6, |max| max - 2e6));
    let high = encoder.max.unwrap_or(low + 2e6);
    let between = if low < high {
        (low..=high).boxed()
    } else {
        Just(low).boxed()
    };
    let mut choices: Vec<BoxedStrategy<f64>> = vec![between];
    if let Some(min) = encoder.min {
        choices.push(Just(min).boxed());
    }
    if let Some(max) = encoder.max {
        choices.push(Just(max).boxed());
    }
    if low <= 0.0 && 0.0 <= high {
        choices.push(Just(-0.0).boxed());
    }
    if let (Some(min), Some(max)) = (encoder.min, encoder.max)
        && let Some(ties) = ties(encoder, min, max)
    {
        choices.push(ties);
    }
    Union::new(choices).prop_map(Arg::F64).boxed()
}

/// Values in `min..=max` whose raw value is near halfway between two the
/// field holds: two integers, or two neighbouring binary32 values.
fn ties(encoder: &Encoder, min: f64, max: f64) -> Option<BoxedStrategy<f64>> {
    let a = unrounded(encoder, min)?;
    let b = unrounded(encoder, max)?;
    let (a, b) = (a.min(b), a.max(b));
    toward(encoder, a)?;
    let near = encoder.clone();
    match encoder.shape {
        Shape::Float(32) => Some(
            (0.0..=1.0_f64)
                .prop_filter_map("a value in range", move |u| {
                    let f = (a + u * (b - a)) as f32;
                    let tie = (f64::from(f) + f64::from(f.next_up())) / 2.0;
                    toward(&near, tie).map(|v| v.clamp(min, max))
                })
                .boxed(),
        ),
        Shape::Uint(_) | Shape::Int(_) => {
            let (first, last) = (a.round() as i64, b.round() as i64);
            (first < last).then(|| {
                (first..last)
                    .prop_filter_map("a value in range", move |k| {
                        toward(&near, k as f64 + 0.5).map(|v| v.clamp(min, max))
                    })
                    .boxed()
            })
        }
        _ => None,
    }
}

/// The argument value whose raw value is about `raw` before rounding, for
/// the ways that are easy to run backwards.
fn toward(encoder: &Encoder, raw: f64) -> Option<f64> {
    let x = match &encoder.through {
        Through::Plain => Some(raw),
        Through::Exact(conversion) => convert(conversion, raw),
        Through::Inverse {
            inverse: Numeric::Linear { scale, offset },
            ..
        } => Some((raw - offset) / scale),
        _ => None,
    }?;
    x.is_finite().then(|| to_argument(encoder, x))
}

/// Text of a length the field takes, in its encoding, without its
/// terminator.
pub(super) fn text_for(text: Text, len: &Len) -> BoxedStrategy<String> {
    let avoid = len.terminator();
    len.lengths()
        .prop_flat_map(move |n| text_of(n, text, avoid))
        .boxed()
}

/// Text of exactly `bytes` bytes in `text`, without the byte `avoid`:
/// characters the encoding holds, then letters.
fn text_of(bytes: usize, text: Text, avoid: Option<u8>) -> BoxedStrategy<String> {
    let chars = match text {
        Text::Ascii => proptest::char::range('\u{0}', '\u{7f}').boxed(),
        Text::Latin1 => proptest::char::range('\u{0}', '\u{ff}').boxed(),
        Text::Utf8 => any::<char>().boxed(),
    };
    vec(chars, 0..=bytes)
        .prop_map(move |chars| {
            let mut value = String::new();
            let mut used = 0;
            for c in chars {
                let encoded = encode(&c.to_string(), text);
                if used + encoded.len() > bytes || avoid.is_some_and(|byte| encoded.contains(&byte))
                {
                    continue;
                }
                used += encoded.len();
                value.push(c);
            }
            let letters: Vec<u8> = b"abcdefghijk"
                .iter()
                .copied()
                .filter(|letter| Some(*letter) != avoid)
                .collect();
            for k in used..bytes {
                value.push(char::from(letters[k % letters.len()]));
            }
            value
        })
        .boxed()
}

/// Bytes of a length the field takes, without its terminator.
fn bytes_for(len: &Len) -> BoxedStrategy<Vec<u8>> {
    let avoid = len.terminator();
    len.lengths()
        .prop_flat_map(move |n| {
            vec(any::<u8>(), n).prop_map(move |bytes| {
                bytes
                    .into_iter()
                    .map(|byte| {
                        if Some(byte) == avoid {
                            byte.wrapping_add(1)
                        } else {
                            byte
                        }
                    })
                    .collect()
            })
        })
        .boxed()
}

/// A value spelled one of the ways §2.2 allows, by `how`: an integer as
/// the other kind of integer or as a double that holds it exactly, a whole
/// double as an integer.
pub(super) fn spell(value: &Arg, how: u8) -> Given {
    // 2^63 and 2^64, past which a whole double is no i64 or u64.
    const I64_END: f64 = 9_223_372_036_854_775_808.0;
    const U64_END: f64 = 18_446_744_073_709_551_616.0;
    let whole = |x: f64| x.fract() == 0.0;
    match (value, how % 3) {
        (Arg::U64(n), 1) if i64::try_from(*n).is_ok() => Given::Int(*n as i64),
        (Arg::U64(n), 2) if exact_double(i128::from(*n)) => Given::Float(*n as f64),
        (Arg::U64(n), _) => Given::Uint(*n),
        (Arg::I64(n), 1) if *n >= 0 => Given::Uint(*n as u64),
        (Arg::I64(n), 2) if exact_double(i128::from(*n)) => Given::Float(*n as f64),
        (Arg::I64(n), _) => Given::Int(*n),
        (Arg::F64(x), 1) if whole(*x) && (0.0..U64_END).contains(x) => Given::Uint(*x as u64),
        (Arg::F64(x), 2) if whole(*x) && (-I64_END..I64_END).contains(x) => Given::Int(*x as i64),
        (Arg::F64(x), _) => Given::Float(*x),
        (Arg::Bool(b), _) => Given::Bool(*b),
        (Arg::Text(text), _) => Given::Text(text.clone()),
        (Arg::Bytes(bytes), _) => Given::Bytes(bytes.clone()),
    }
}

/// The least double at or above `n`.
pub(super) fn double_at_least(n: i128) -> f64 {
    let x = n as f64;
    if (x as i128) < n { x.next_up() } else { x }
}

/// The greatest double at or below `n`.
pub(super) fn double_at_most(n: i128) -> f64 {
    let x = n as f64;
    if (x as i128) > n { x.next_down() } else { x }
}

/// Whether a double holds `n` exactly.
fn exact_double(n: i128) -> bool {
    n as f64 as i128 == n
}

/// What a spelled value is, as the argument's type (§2.2).
pub(super) fn value_of(given: &Given, ty: &ArgType) -> Arg {
    match (given, ty) {
        (Given::Int(n), ArgType::F64) => Arg::F64(*n as f64),
        (Given::Uint(n), ArgType::F64) => Arg::F64(*n as f64),
        (Given::Float(x), ArgType::F64) => Arg::F64(*x),
        (Given::Int(n), ArgType::U64) => Arg::U64(*n as u64),
        (Given::Uint(n), ArgType::U64) => Arg::U64(*n),
        (Given::Float(x), ArgType::U64) => Arg::U64(*x as u64),
        (Given::Int(n), _) => Arg::I64(*n),
        (Given::Uint(n), _) => Arg::I64(*n as i64),
        (Given::Float(x), _) => Arg::I64(*x as i64),
        (Given::Bool(b), _) => Arg::Bool(*b),
        (Given::Text(text), _) => Arg::Text(text.clone()),
        (Given::Bytes(bytes), _) => Arg::Bytes(bytes.clone()),
    }
}

/// An argument's value in its field's unit (`spec/encoding.md` §3.1).
fn to_field(encoder: &Encoder, value: f64) -> f64 {
    match encoder.units {
        Some((argument, field)) if encoder.ty == ArgType::F64 => {
            convert_unit(value, argument, field)
        }
        _ => value,
    }
}

/// A value in the field's unit, in the argument's (§7.4).
pub(super) fn to_argument(encoder: &Encoder, value: f64) -> f64 {
    match encoder.units {
        Some((argument, field)) if encoder.ty == ArgType::F64 => {
            convert_unit(value, field, argument)
        }
        _ => value,
    }
}

/// An f64 argument's raw value before rounding (§3.1): in the field's
/// unit, through the inverse of the field's conversion. `None` outside an
/// inverse table that doesn't extrapolate.
pub(super) fn unrounded(encoder: &Encoder, value: f64) -> Option<f64> {
    let x = to_field(encoder, value);
    match &encoder.through {
        Through::Plain => Some(x),
        Through::Exact(Numeric::Linear { scale, offset }) => Some((x - offset) / scale),
        Through::Exact(Numeric::Table {
            points,
            extrapolate,
            ..
        }) => {
            // The table read backwards: raw and value swapped, in rising
            // order of value.
            let mut swapped: Vec<(f64, f64)> = points.iter().map(|&(raw, v)| (v, raw)).collect();
            if swapped[0].0 > swapped[swapped.len() - 1].0 {
                swapped.reverse();
            }
            table(&swapped, false, *extrapolate, x)
        }
        Through::Exact(_) | Through::States(_) => None,
        Through::Inverse { inverse, .. } => convert(inverse, x),
    }
}
