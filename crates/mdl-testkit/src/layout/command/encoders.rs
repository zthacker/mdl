//! Catalog arguments and the fields that encode them: an argument of each
//! type, through each way `spec/encoding.md` §3.1 turns a value into a raw
//! value, in a field that holds every raw value its range becomes.

use std::collections::BTreeSet;

use proptest::collection::{btree_set, vec};
use proptest::prelude::*;
use proptest::sample::select;

use super::values::{double_at_least, double_at_most, to_argument, unrounded, value};
use super::{Arg, ArgType, Encoder, Len, Shape, TEXTS, Through, encoder_of};
use crate::layout::{Numeric, UNIT_GROUPS, scale, units};

/// An argument of any type and a field that encodes it, with a default or
/// none.
pub(super) fn encoder() -> BoxedStrategy<Encoder> {
    with_default(prop_oneof![
        1 => bool_encoder(),
        2 => integer_encoder(ArgType::U64),
        2 => integer_encoder(ArgType::I64),
        5 => f64_encoder(),
        2 => enum_encoder(),
        2 => text_encoder(false),
        1 => text_encoder(true),
    ])
}

/// The encoder, maybe with a default: a value like any other.
pub(super) fn with_default(
    encoders: impl Strategy<Value = Encoder> + 'static,
) -> BoxedStrategy<Encoder> {
    encoders
        .prop_flat_map(|encoder| {
            let default = proptest::option::of(value(&encoder));
            (Just(encoder), default)
        })
        .prop_map(|(mut encoder, default)| {
            encoder.default = default.map(as_literal);
            encoder
        })
        .boxed()
}

/// A value as the catalog reads it back (`spec/encoding.md` §2.2): a whole
/// number in the i64 range is an integer, so -0.0 is 0.
fn as_literal(value: Arg) -> Arg {
    match value {
        Arg::F64(x) if x == 0.0 && x.is_sign_negative() => Arg::F64(0.0),
        other => other,
    }
}

fn bool_encoder() -> BoxedStrategy<Encoder> {
    prop_oneof![
        Just(Shape::Bool),
        (1_u32..=64).prop_map(Shape::Uint),
        // A 1-bit int holds 0 and -1, not 1.
        (2_u32..=64).prop_map(Shape::Int),
    ]
    .prop_map(|shape| encoder_of(ArgType::Bool, shape))
    .boxed()
}

/// A unit code, any of those the units module converts.
pub(super) fn unit() -> BoxedStrategy<&'static str> {
    select(UNIT_GROUPS).prop_flat_map(select).boxed()
}

/// A u64 or i64 argument, unconverted, in its field's unit if it has one.
/// Each end of its range is one the field holds (the field's own, or a
/// small integer) within ±(2^53 − 1), as the IR writes bounds, and is left
/// out only where the type's own end fits the field.
fn integer_encoder(ty: ArgType) -> BoxedStrategy<Encoder> {
    // Any size, and often 64 bits, which a whole-range argument fills.
    let sizes = prop_oneof![3 => 1_u32..=64, 1 => Just(64_u32)];
    let shapes = if ty == ArgType::U64 {
        sizes.prop_map(Shape::Uint).boxed()
    } else {
        prop_oneof![
            sizes.clone().prop_map(Shape::Int),
            sizes.prop_map(Shape::Uint)
        ]
        .boxed()
    };
    (
        Just(ty),
        shapes,
        proptest::option::of(unit()),
        any::<(bool, bool)>(),
    )
        .prop_flat_map(|(ty, shape, unit, keep)| {
            let field = shape.integers().unwrap_or(0..=0);
            let types = ty.integers();
            let low = (*field.start()).max(*types.start());
            let high = (*field.end()).min(*types.end());
            const SAFE: i128 = (1 << 53) - 1;
            let bound = prop_oneof![
                Just(double_at_least(low.max(-SAFE))),
                Just(double_at_most(high.min(SAFE))),
                (low.max(-1000)..=high.min(1000)).prop_map(|n| n as f64),
            ];
            let needs = (types.start() < field.start(), types.end() > field.end());
            (
                Just(encoder_of(ty, shape)),
                Just(unit),
                Just(keep),
                Just(needs),
                bound.clone(),
                bound,
            )
        })
        .prop_map(|(mut encoder, unit, keep, needs, a, b)| {
            encoder.units = unit.map(|code| (code, code));
            encoder.min = (needs.0 || keep.0).then_some(a.min(b));
            encoder.max = (needs.1 || keep.1).then_some(a.max(b));
            encoder
        })
        .boxed()
}

/// An f64 argument through any of §3.1's ways.
pub(super) fn f64_encoder() -> BoxedStrategy<Encoder> {
    let linear = (scale(), -1e3..1e3_f64)
        .prop_map(|(scale, offset)| Through::Exact(Numeric::Linear { scale, offset }));
    prop_oneof![
        2 => direct(Just(Through::Plain).boxed()),
        2 => direct(linear.boxed()),
        2 => monotone_table(),
        3 => with_inverse(),
    ]
    .boxed()
}

/// Two values from `range`, in order.
/// A range's two ends, in order: values in `-span..span`, 0, small
/// integers, or one value twice.
fn range_ends(span: f64) -> BoxedStrategy<(f64, f64)> {
    let end = prop_oneof![
        3 => -span..span,
        1 => Just(0.0),
        1 => (-100_i32..=100).prop_map(f64::from),
    ];
    prop_oneof![
        9 => (end.clone(), end).prop_map(|(a, b)| (a.min(b), a.max(b))),
        1 => (-span..span).prop_map(|x| (x, x)),
    ]
    .boxed()
}

fn sorted_pair(range: std::ops::Range<f64>) -> BoxedStrategy<(f64, f64)> {
    (range.clone(), range)
        .prop_map(|(a, b)| (a.min(b), a.max(b)))
        .boxed()
}

/// What an f64 argument's field is: an integer one with up to three spare
/// bits, or a float one of binary64 or binary32.
#[derive(Debug, Clone, Copy)]
struct FieldChoice {
    integer: bool,
    wide: bool,
    spare: u32,
}

fn field_choice() -> BoxedStrategy<FieldChoice> {
    (any::<bool>(), any::<bool>(), 0_u32..=3)
        .prop_map(|(integer, wide, spare)| FieldChoice {
            integer,
            wide,
            spare,
        })
        .boxed()
}

/// The field for an f64 argument whose range's ends become the raw values
/// `low` and `high` before rounding: an integer field that holds them
/// rounded, if chosen and one of 64 bits can; else a float field.
fn field_for(choice: FieldChoice, low: f64, high: f64) -> Shape {
    if choice.integer
        && let Some(shape) = integer_shape(low.round() as i128, high.round() as i128, choice.spare)
    {
        return shape;
    }
    Shape::Float(if choice.wide || choice.integer {
        64
    } else {
        32
    })
}

/// The narrowest integer field holding `a..=b`, with `spare` more bits, if
/// one of at most 64 bits does.
fn integer_shape(a: i128, b: i128, spare: u32) -> Option<Shape> {
    let (a, b) = (a.min(b), a.max(b));
    let shape = if a >= 0 {
        let bits = (128 - b.leading_zeros()).max(1);
        Shape::Uint(bits + spare)
    } else {
        let magnitude = b.max(-a - 1);
        let bits = 128 - magnitude.leading_zeros() + 1;
        Shape::Int(bits + spare)
    };
    match shape {
        Shape::Uint(bits) | Shape::Int(bits) if bits <= 64 => Some(shape),
        Shape::Uint(_) | Shape::Int(_) => {
            let fits = shape.widened(64).integers()?;
            (fits.contains(&a) && fits.contains(&b)).then(|| shape.widened(64))
        }
        _ => None,
    }
}

/// An f64 argument written as it is or through a linear conversion: into
/// a float field with any range or none, or an integer field with a range.
fn direct(through: BoxedStrategy<Through>) -> BoxedStrategy<Encoder> {
    (
        through,
        proptest::option::of(units()),
        field_choice(),
        range_ends(1e6),
        any::<(bool, bool)>(),
    )
        .prop_map(|(through, units, choice, (low, high), keep)| {
            let mut encoder = encoder_of(ArgType::F64, Shape::Float(64));
            encoder.through = through;
            encoder.units = units;
            encoder.min = (choice.integer || keep.0).then_some(low);
            encoder.max = (choice.integer || keep.1).then_some(high);
            encoder.shape = shape_for(&encoder, choice);
            encoder
        })
        .boxed()
}

/// The field for an encoder whose range is known: an integer one only if
/// both ends are given.
fn shape_for(encoder: &Encoder, choice: FieldChoice) -> Shape {
    let ends = encoder
        .min
        .zip(encoder.max)
        .and_then(|(min, max)| unrounded(encoder, min).zip(unrounded(encoder, max)));
    match ends {
        Some((a, b)) if choice.integer => field_for(choice, a.min(b), a.max(b)),
        _ => Shape::Float(if choice.wide { 64 } else { 32 }),
    }
}

/// An f64 argument through a linear table whose values only rise or only
/// fall, read backwards (no inverse). Its range lies inside the table's
/// values, or past them when it extrapolates.
fn monotone_table() -> BoxedStrategy<Encoder> {
    (
        btree_set(-1000_i32..=1000, 2..=5),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_flat_map(|(raws, falling, extrapolate)| {
            let count = raws.len();
            let fractions = if extrapolate { -0.5..1.5 } else { 0.01..0.99 };
            (
                Just(raws),
                btree_set(-4_000_000_i32..=4_000_000, count),
                Just(falling),
                Just(extrapolate),
                proptest::option::of(units()),
                field_choice(),
                sorted_pair(fractions),
            )
        })
        .prop_map(
            |(raws, values, falling, extrapolate, units, choice, (f, g))| {
                let mut values: Vec<f64> = values.into_iter().map(|v| f64::from(v) / 4.0).collect();
                let (least, most) = (values[0], values[values.len() - 1]);
                if falling {
                    values.reverse();
                }
                let points: Vec<(f64, f64)> = raws.into_iter().map(f64::from).zip(values).collect();
                let mut encoder = encoder_of(ArgType::F64, Shape::Float(64));
                encoder.units = units;
                encoder.through = Through::Exact(Numeric::Table {
                    points,
                    step: false,
                    extrapolate,
                });
                encoder.min = Some(to_argument(&encoder, least + f * (most - least)));
                encoder.max = Some(to_argument(&encoder, least + g * (most - least)));
                encoder.shape = shape_for(&encoder, choice);
                encoder
            },
        )
        .boxed()
}

/// An f64 argument through a conversion that doesn't invert exactly (a
/// polynomial, a spline, a step table, a table that rises and falls) with
/// an explicit inverse that only rises or only falls (linear, a
/// polynomial, a table). The conversion's points lie around the raw values
/// the range becomes, unless it extrapolates.
fn with_inverse() -> BoxedStrategy<Encoder> {
    let inverse = prop_oneof![
        (scale(), -1e3..1e3_f64).prop_map(|(scale, offset)| Numeric::Linear { scale, offset }),
        monotone_poly(),
        inverse_table(),
    ];
    (
        inverse,
        proptest::option::of(units()),
        field_choice(),
        range_ends(1e3),
        sorted_pair(0.01..0.99),
    )
        .prop_flat_map(|(inverse, units, choice, (low, high), (f, g))| {
            let mut encoder = encoder_of(ArgType::F64, Shape::Float(64));
            encoder.units = units;
            encoder.through = Through::Inverse {
                conversion: Numeric::Plain,
                inverse: inverse.clone(),
            };
            encoder.min = Some(low);
            encoder.max = Some(high);
            // An inverse table that doesn't extrapolate takes values among
            // its points.
            if let Numeric::Table {
                points,
                extrapolate: false,
                ..
            } = &inverse
            {
                let (least, most) = (points[0].0, points[points.len() - 1].0);
                encoder.min = Some(to_argument(&encoder, least + f * (most - least)));
                encoder.max = Some(to_argument(&encoder, least + g * (most - least)));
            }
            let a = encoder
                .min
                .and_then(|min| unrounded(&encoder, min))
                .unwrap_or(0.0);
            let b = encoder
                .max
                .and_then(|max| unrounded(&encoder, max))
                .unwrap_or(0.0);
            let (a, b) = (a.min(b), a.max(b));
            encoder.shape = field_for(choice, a, b);
            (Just(encoder), forward_conversion(a, b))
        })
        .prop_map(|(mut encoder, forward)| {
            if let Through::Inverse { conversion, .. } = &mut encoder.through {
                *conversion = forward;
            }
            encoder
        })
        .boxed()
}

/// `c0 + c1·x`, or `c0 + c1·x + c3·x³` with `c3` of `c1`'s sign: a
/// polynomial that only rises or only falls everywhere.
fn monotone_poly() -> BoxedStrategy<Numeric> {
    (-1e3..1e3_f64, scale(), proptest::option::of(1e-9..1e-3_f64))
        .prop_map(|(c0, c1, cubic)| {
            Numeric::Poly(match cubic {
                Some(c3) => vec![c0, c1, 0.0, c3.copysign(c1)],
                None => vec![c0, c1],
            })
        })
        .boxed()
}

/// A linear table from engineering value to raw whose raw values only rise
/// or only fall.
fn inverse_table() -> BoxedStrategy<Numeric> {
    btree_set(-4000_i32..=4000, 2..=5)
        .prop_flat_map(|inputs| {
            let count = inputs.len();
            (
                Just(inputs),
                btree_set(-1_000_000_i32..=1_000_000, count),
                any::<bool>(),
                any::<bool>(),
            )
        })
        .prop_map(|(inputs, raws, falling, extrapolate)| {
            let mut raws: Vec<f64> = raws.into_iter().map(f64::from).collect();
            if falling {
                raws.reverse();
            }
            Numeric::Table {
                points: inputs
                    .into_iter()
                    .map(|input| f64::from(input) / 4.0)
                    .zip(raws)
                    .collect(),
                step: false,
                extrapolate,
            }
        })
        .boxed()
}

/// A conversion from raw values `low..=high` (before rounding) that needs
/// an inverse: a polynomial, a spline, a step table, or a linear table
/// whose values rise and fall.
fn forward_conversion(low: f64, high: f64) -> BoxedStrategy<Numeric> {
    let poly = vec(-10.0..10.0_f64, 1..=4).prop_map(Numeric::Poly);
    if !(low.abs() < 1e15 && high.abs() < 1e15) {
        return poly.boxed();
    }
    // Room for the field's rounding: to an integer, or to binary32.
    let margin = 1.0 + 1e-6 * low.abs().max(high.abs());
    let first = (low - margin).floor() as i64;
    let last = (high + margin).ceil() as i64;
    let with_values = calibration(first, last).prop_flat_map(|(raws, extrapolate)| {
        let count = raws.len();
        (Just(raws), vec(-1e6..1e6_f64, count), Just(extrapolate))
    });
    let spline = with_values
        .clone()
        .prop_map(|(raws, values, extrapolate)| Numeric::Spline {
            points: raws.into_iter().zip(values).collect(),
            extrapolate,
        });
    let step = with_values
        .clone()
        .prop_map(|(raws, values, extrapolate)| Numeric::Table {
            points: raws.into_iter().zip(values).collect(),
            step: true,
            extrapolate,
        });
    let bumpy = with_values.prop_map(|(mut raws, mut values, extrapolate)| {
        // A peak at the second point: the values rise, then fall.
        if raws.len() < 3 {
            raws.push(raws[raws.len() - 1] + 1.0);
            values.push(0.0);
        }
        values[1] = values.iter().fold(f64::MIN, |a, &b| a.max(b)) + 1.0;
        Numeric::Table {
            points: raws.into_iter().zip(values).collect(),
            step: false,
            extrapolate,
        }
    });
    prop_oneof![poly, spline, step, bumpy].boxed()
}

/// Calibration raws for a conversion the raw values `first..=last` go
/// through, and whether it extrapolates: around them when it doesn't, and
/// among them when it does, so decoding extrapolates.
fn calibration(first: i64, last: i64) -> BoxedStrategy<(Vec<f64>, bool)> {
    let room = usize::try_from(last - first + 1).unwrap_or(5).min(5);
    let around = (0_i64..3, 0_i64..3, btree_set(first..=last, 0..=room.min(3))).prop_map(
        move |(below, above, inside)| {
            let mut raws: BTreeSet<i64> = inside;
            raws.insert(first - below);
            raws.insert(last + above);
            (raws.into_iter().map(|raw| raw as f64).collect(), false)
        },
    );
    let among = btree_set(first..=last, 2..=room)
        .prop_map(|raws| (raws.into_iter().map(|raw| raw as f64).collect(), true));
    prop_oneof![around, among].boxed()
}

/// An enum argument: through states into an integer field, or its states'
/// names written into a string field.
fn enum_encoder() -> BoxedStrategy<Encoder> {
    let through_states = prop_oneof![
        (1_u32..=64).prop_map(Shape::Uint),
        (1_u32..=64).prop_map(Shape::Int)
    ]
    .prop_flat_map(|shape| {
        let raws = shape.integers().unwrap_or(0..=0);
        let most = if let Shape::Uint(1) | Shape::Int(1) = shape {
            2
        } else {
            4
        };
        (Just(shape), btree_set(raws, 1..=most))
    })
    .prop_flat_map(|(shape, raws)| {
        let names: Vec<String> = (0..raws.len()).map(|k| format!("S{k}")).collect();
        (Just(shape), Just(raws), Just(names).prop_shuffle())
    })
    .prop_map(|(shape, raws, shuffled)| {
        let mut catalog = shuffled.clone();
        catalog.sort();
        let mut encoder = encoder_of(ArgType::Enum(catalog), shape);
        encoder.through = Through::States(raws.into_iter().zip(shuffled).collect());
        encoder
    });
    let separators = select(&[0_u8, b' ', b'#', 0xFF][..]);
    let lengths = prop_oneof![
        (1_usize..=6).prop_map(Len::Fixed),
        (1_usize..=8, separators.clone()).prop_map(|(bytes, byte)| Len::Padded(bytes, byte)),
        separators.prop_map(Len::Terminator),
        length_field(),
    ];
    let as_text = (select(TEXTS), lengths)
        .prop_flat_map(|(text, len)| {
            let name = len
                .lengths()
                .prop_filter("a state's name isn't empty", |n| *n >= 1)
                .prop_flat_map(|n| vec(select(&b"abcdefghij"[..]), n))
                .prop_map(|letters| letters.into_iter().map(char::from).collect::<String>());
            (Just(Shape::Text(text, len)), btree_set(name, 1..=3))
        })
        .prop_map(|(shape, names)| encoder_of(ArgType::Enum(names.into_iter().collect()), shape));
    prop_oneof![through_states, as_text].boxed()
}

pub(super) fn length_field() -> BoxedStrategy<Len> {
    (4_u32..=16, 1_u32..=4, -3_i64..=3)
        .prop_map(|(bits, multiply, add)| Len::Field {
            bits,
            multiply,
            add,
        })
        .boxed()
}

/// A str argument, or with `bytes` a bytes one, in a field of any length
/// kind but `remaining` (which only the last entry has).
fn text_encoder(bytes: bool) -> BoxedStrategy<Encoder> {
    let lengths = prop_oneof![
        (1_usize..=10).prop_map(Len::Fixed),
        (1_usize..=10, any::<u8>()).prop_map(|(n, byte)| Len::Padded(n, byte)),
        any::<u8>().prop_map(Len::Terminator),
        length_field(),
    ];
    if bytes {
        lengths
            .prop_map(|len| encoder_of(ArgType::Bytes, Shape::Bytes(len)))
            .boxed()
    } else {
        (select(TEXTS), lengths)
            .prop_map(|(text, len)| encoder_of(ArgType::Str, Shape::Text(text, len)))
            .boxed()
    }
}
