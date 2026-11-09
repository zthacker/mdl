//! An argument's calibration, and the engineering values whose raw values
//! a field holds.
//!
//! Yamcs applies an argument type's calibrator to the engineering value to
//! get the raw value: the calibrator runs from engineering to raw, the
//! other way from a parameter's. An MDL field's conversion runs from raw to
//! engineering, and its inverse, where the conversion doesn't invert
//! exactly, from engineering to raw. So a calibrator of degree 1 is the
//! field's inverse as written, which MDL evaluates as Yamcs does, and its
//! algebraic inverse the conversion; a spline that only rises or only
//! falls is a table with its points swapped, which MDL reads backwards.

use mdl::v2::source::Span;

use crate::Code;
use crate::out::{List, Object};
use crate::report::Report;
use crate::types::{Calibration, Calibrator};

/// The largest integer every JSON reader holds exactly, which bounds an
/// MDL range's ends.
pub(crate) const SAFE: i128 = (1 << 53) - 1;

/// How MDL takes an argument's engineering value to its raw value, before
/// rounding.
#[derive(Debug, Clone)]
pub(crate) enum ToRaw {
    /// The inverse of a linear conversion: `(value - offset) / scale`.
    Linear { scale: f64, offset: f64 },
    /// An explicit linear inverse: `value * scale + offset`.
    Inverse { scale: f64, offset: f64 },
    /// A linear table read backwards: points `(engineering, raw)` in
    /// increasing engineering order, extrapolating past the ends or not.
    Table {
        points: Vec<(f64, f64)>,
        extrapolate: bool,
    },
}

impl ToRaw {
    /// The raw value, unrounded; `None` outside a table that doesn't
    /// extrapolate.
    pub(crate) fn raw(&self, value: f64) -> Option<f64> {
        match self {
            ToRaw::Linear { scale, offset } => Some((value - offset) / scale),
            ToRaw::Inverse { scale, offset } => Some(value * scale + offset),
            ToRaw::Table {
                points,
                extrapolate,
            } => through(points, value, *extrapolate),
        }
    }

    /// An engineering value near the one whose raw value is `raw`, as a
    /// starting point: the conversion's.
    fn engineering(&self, raw: f64) -> f64 {
        match self {
            ToRaw::Linear { scale, offset } => raw * scale + offset,
            ToRaw::Inverse { scale, offset } => (raw - offset) / scale,
            ToRaw::Table { points, .. } => {
                let mut swapped: Vec<(f64, f64)> = points.iter().map(|(v, r)| (*r, *v)).collect();
                swapped.sort_by(|a, b| a.0.total_cmp(&b.0));
                through(&swapped, raw, true).unwrap_or(raw)
            }
        }
    }

    /// The engineering values the table has points for, if it doesn't
    /// extrapolate past them.
    pub(crate) fn domain(&self) -> Option<(f64, f64)> {
        match self {
            ToRaw::Table {
                points,
                extrapolate: false,
            } => Some((points.first()?.0, points.last()?.0)),
            _ => None,
        }
    }
}

/// A linear table at `x`, as `decoding.md` §7.1 reads one: a point's value
/// exactly, else the segment's formula, extrapolated from the end segments
/// or not.
fn through(points: &[(f64, f64)], x: f64, extrapolate: bool) -> Option<f64> {
    let (first, last) = (points.first()?, points.last()?);
    if let Some(point) = points.iter().find(|(r, _)| *r == x) {
        return Some(point.1);
    }
    let segment = if x < first.0 || x > last.0 {
        if !extrapolate || points.len() < 2 {
            return None;
        }
        if x < first.0 { 0 } else { points.len() - 2 }
    } else {
        points.windows(2).position(|w| w[0].0 < x && x < w[1].0)?
    };
    let ((r0, v0), (r1, v1)) = (points[segment], points[segment + 1]);
    Some(v0 + ((x - r0) * (v1 - v0)) / (r1 - r0))
}

/// An argument type's calibration as an MDL field's.
pub(crate) struct ArgumentCalibration {
    /// Raw to engineering, for decoding a command.
    pub(crate) conversion: Object,
    /// Engineering to raw, where the conversion doesn't invert exactly.
    pub(crate) inverse: Option<Object>,
    pub(crate) to_raw: ToRaw,
}

/// A calibrator from engineering to raw as an argument's conversion and
/// inverse. A polynomial of degree 1 and a spline of order 1 that only
/// rises or only falls are supported; anything else has no exact
/// conversion from raw to engineering, which decoding a command needs.
pub(crate) fn argument_calibration(
    calibrator: &Calibrator,
    report: &mut Report,
) -> Option<ArgumentCalibration> {
    match &calibrator.calibration {
        Calibration::Polynomial { coefficients, span } => {
            let (offset, scale) = match coefficients.as_slice() {
                [offset, scale] if *scale != 0.0 => (*offset, *scale),
                [_] | [_, _] => {
                    report.error(
                        Code::Unsupported,
                        *span,
                        "a constant argument calibrator isn't supported: every value would be \
                         written as one raw value",
                    );
                    return None;
                }
                _ => {
                    report.error(
                        Code::Unsupported,
                        *span,
                        "an argument calibrator of degree 2 or more isn't supported: MDL decodes \
                         a command through the calibrator's inverse, which such a polynomial \
                         doesn't have exactly",
                    );
                    return None;
                }
            };
            let coefficients = [-offset / scale, 1.0 / scale];
            if coefficients.iter().any(|c| !c.is_finite()) {
                report.error(
                    Code::Unsupported,
                    *span,
                    "this argument calibrator's inverse isn't a finite polynomial",
                );
                return None;
            }
            let mut conversion = Object::at(*span);
            conversion.put("kind", "poly", None);
            conversion.put("coefficients", coefficients.to_vec(), None);
            let mut inverse = Object::at(*span);
            inverse.put("kind", "linear", None);
            inverse.put("scale", scale, None);
            inverse.put("offset", offset, None);
            Some(ArgumentCalibration {
                conversion,
                inverse: Some(inverse),
                to_raw: ToRaw::Inverse { scale, offset },
            })
        }
        Calibration::Spline {
            points,
            order,
            extrapolate,
            span,
        } => {
            if order.0 != 1 {
                report.error(
                    Code::Unsupported,
                    order.1,
                    format!(
                        "an order {} spline on an argument isn't supported: MDL decodes a \
                         command through the calibrator's inverse, which only a linear spline \
                         that only rises or only falls has",
                        order.0
                    ),
                );
                return None;
            }
            let mut sorted: Vec<_> = points.iter().collect();
            sorted.sort_by(|a, b| a.raw.0.total_cmp(&b.raw.0));
            let rises = sorted
                .windows(2)
                .all(|w| w[0].calibrated.0 < w[1].calibrated.0);
            let falls = sorted
                .windows(2)
                .all(|w| w[0].calibrated.0 > w[1].calibrated.0);
            let distinct = sorted.windows(2).all(|w| w[0].raw.0 < w[1].raw.0);
            if sorted.len() < 2 || !distinct || !(rises || falls) {
                report.error(
                    Code::Unsupported,
                    *span,
                    "an argument spline that doesn't only rise or only fall isn't supported: \
                     MDL decodes a command through the calibrator's inverse, which it doesn't \
                     have",
                );
                return None;
            }
            // The conversion runs from raw (the points' calibrated values)
            // to engineering (their raw values), in increasing raw order.
            let mut by_raw = sorted.clone();
            if falls {
                by_raw.reverse();
            }
            let mut table = List::default();
            for point in &by_raw {
                let mut object = Object::at(point.span);
                object.put("raw", point.calibrated.0, Some(point.calibrated.1));
                object.put("value", point.raw.0, Some(point.raw.1));
                table.push_object(object);
            }
            let mut conversion = Object::at(*span);
            conversion.put("kind", "table", None);
            conversion.put_list("points", table);
            match extrapolate {
                Some(at) => conversion.put("extrapolate", true, Some(*at)),
                None => report.warning(
                    Code::Differs,
                    *span,
                    "Yamcs extrapolates an argument's spline past its points whatever \
                     extrapolate says; MDL doesn't, as XTCE says, so a value outside them is \
                     refused",
                ),
            }
            Some(ArgumentCalibration {
                conversion,
                inverse: None,
                to_raw: ToRaw::Table {
                    points: sorted.iter().map(|p| (p.raw.0, p.calibrated.0)).collect(),
                    extrapolate: extrapolate.is_some(),
                },
            })
        }
    }
}

/// The raw values a field holds: integers (rounded to the nearest, halves
/// away from zero, as MDL rounds them) or floats of 32 or 64 bits.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Holds {
    Integers(i128, i128),
    Float32,
    Float64,
}

/// Why no range was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Empty {
    /// No engineering value gives a raw value that qualifies.
    NoValue,
    /// Those that do lie past ±(2^53 - 1), where an MDL range can't reach.
    PastSafe,
}

/// The engineering values from the least to the greatest whose raw values
/// the field holds, and lie in `raw` (an inclusive range of raw values)
/// when given; ends past ±(2^53 - 1) are cut there, the flag saying so.
pub(crate) fn engineering_range(
    to_raw: &ToRaw,
    holds: Holds,
    raw: Option<(f64, f64)>,
) -> Result<(f64, f64, bool), Empty> {
    let (raw_low, raw_high) = raw.unwrap_or((f64::NEG_INFINITY, f64::INFINITY));
    let (int_low, int_high) = match holds {
        Holds::Integers(low, high) => (low.max(ceiling(raw_low)), high.min(floor(raw_high))),
        _ => (0, 0),
    };
    let fits = |value: f64| -> bool {
        let Some(r) = to_raw.raw(value) else {
            return false;
        };
        match holds {
            Holds::Integers(..) => {
                let n = r.round();
                n.is_finite() && (int_low..=int_high).contains(&(n as i128))
            }
            Holds::Float32 => (r as f32).is_finite() && raw_low <= r && r <= raw_high,
            Holds::Float64 => r.is_finite() && raw_low <= r && r <= raw_high,
        }
    };
    // A value inside, to search out from.
    let middle = match holds {
        Holds::Integers(..) => {
            let raw = int_low / 2 + int_high / 2;
            to_raw.engineering(raw as f64)
        }
        _ => {
            let raw = if raw_low.is_finite() && raw_high.is_finite() {
                raw_low / 2.0 + raw_high / 2.0
            } else {
                0.0_f64.clamp(raw_low, raw_high)
            };
            to_raw.engineering(raw)
        }
    };
    let (first, last) = to_raw
        .domain()
        .unwrap_or((f64::NEG_INFINITY, f64::INFINITY));
    // Taken to engineering and back, a raw value can land a few doubles
    // off, so the start is looked for nearby.
    let start = near(middle.clamp(first, last), &fits).ok_or(Empty::NoValue)?;
    let safe = SAFE as f64;
    let (low, high) = (first.max(-safe), last.min(safe));
    let start = if start < low || start > high {
        let bound = start.clamp(low, high);
        if !fits(bound) {
            return Err(Empty::PastSafe);
        }
        bound
    } else {
        start
    };
    let least = if fits(low) {
        low
    } else {
        edge(low, start, &fits)
    };
    let greatest = if fits(high) {
        high
    } else {
        edge(high, start, &fits)
    };
    let cut = (least == -safe && fits(-safe)) || (greatest == safe && fits(safe));
    Ok((least, greatest, cut))
}

/// The value that fits nearest `start`, within 64 doubles of it either way.
fn near(start: f64, fits: &impl Fn(f64) -> bool) -> Option<f64> {
    let (mut up, mut down) = (start, start);
    for _ in 0..=64 {
        if fits(up) {
            return Some(up);
        }
        if fits(down) {
            return Some(down);
        }
        (up, down) = (up.next_up(), down.next_down());
    }
    None
}

/// The value nearest `outside` that fits, searching between `outside`
/// (which doesn't) and `inside` (which does), over the doubles in order.
fn edge(outside: f64, inside: f64, fits: &impl Fn(f64) -> bool) -> f64 {
    let (mut out, mut ins) = (key(outside), key(inside));
    while out.abs_diff(ins) > 1 {
        let middle = ((i128::from(out) + i128::from(ins)) / 2) as i64;
        if fits(value(middle)) {
            ins = middle;
        } else {
            out = middle;
        }
    }
    value(ins)
}

/// A double's place among all doubles in order, negative ones first.
fn key(value: f64) -> i64 {
    let bits = value.to_bits() as i64;
    if bits < 0 { bits ^ i64::MAX } else { bits }
}

fn value(key: i64) -> f64 {
    let bits = if key < 0 { key ^ i64::MAX } else { key };
    f64::from_bits(bits as u64)
}

fn ceiling(x: f64) -> i128 {
    if x.is_finite() {
        x.ceil() as i128
    } else if x < 0.0 {
        i128::MIN
    } else {
        i128::MAX
    }
}

fn floor(x: f64) -> i128 {
    if x.is_finite() {
        x.floor() as i128
    } else if x < 0.0 {
        i128::MIN
    } else {
        i128::MAX
    }
}

/// An `xs:duration` without years or months (whose lengths vary), in
/// seconds: an optional `-`, `P`, days, then after `T` hours, minutes and
/// seconds (which may have a fraction), each a number and its letter.
pub(crate) fn duration_seconds(text: &str) -> Option<f64> {
    let text = text.trim();
    let (sign, rest) = match text.strip_prefix('-') {
        Some(rest) => (-1.0, rest),
        None => (1.0, text),
    };
    let rest = rest.strip_prefix('P')?;
    let (date, time) = match rest.split_once('T') {
        Some((date, time)) if !time.is_empty() => (date, Some(time)),
        Some(_) => return None,
        None => (rest, None),
    };
    let mut seconds = 0.0;
    let mut any = false;
    let mut read = |part: &str, units: &[(char, f64)], fraction: char| -> Option<()> {
        let mut number = String::new();
        let mut next = 0;
        for c in part.chars() {
            if c.is_ascii_digit() || (c == '.' && fraction == 'S') {
                number.push(c);
                continue;
            }
            let at = units.iter().skip(next).position(|(unit, _)| *unit == c)? + next;
            if number.is_empty() || (number.contains('.') && c != fraction) {
                return None;
            }
            seconds += number.parse::<f64>().ok()? * units[at].1;
            number.clear();
            next = at + 1;
            any = true;
        }
        number.is_empty().then_some(())
    };
    read(date, &[('D', 86_400.0)], 'D')?;
    if let Some(time) = time {
        read(time, &[('H', 3_600.0), ('M', 60.0), ('S', 1.0)], 'S')?;
    }
    (any && seconds.is_finite()).then_some(sign * seconds)
}

/// Where a range's span comes from when it's worked out, not written.
pub(crate) fn worked_out(span: Span, range: (f64, f64)) -> crate::types::ValidRange {
    crate::types::ValidRange {
        min: Some((range.0, span)),
        max: Some((range.1, span)),
        raw: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_are_days_hours_minutes_and_seconds() {
        assert_eq!(duration_seconds("PT10S"), Some(10.0));
        assert_eq!(duration_seconds("P1DT1H1M1.5S"), Some(90_061.5));
        assert_eq!(duration_seconds("-PT0.25S"), Some(-0.25));
        assert_eq!(duration_seconds("P2D"), Some(172_800.0));
        for bad in [
            "", "P", "PT", "10S", "P1Y", "P1M", "PT1S1M", "PT1.5M", "PTS", "P1DT",
        ] {
            assert_eq!(duration_seconds(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_range_is_the_engineering_values_whose_raw_values_fit() {
        // raw = 10 v into a byte: 255.5 raw is past it, so 25.55 is out.
        let to_raw = ToRaw::Inverse {
            scale: 10.0,
            offset: 0.0,
        };
        let (low, high, cut) = engineering_range(&to_raw, Holds::Integers(0, 255), None).unwrap();
        assert!(!cut);
        assert_eq!(
            (
                to_raw.raw(low).unwrap().round(),
                to_raw.raw(high).unwrap().round()
            ),
            (0.0, 255.0)
        );
        assert!(to_raw.raw(low.next_down()).unwrap().round() < 0.0);
        assert!(to_raw.raw(high.next_up()).unwrap().round() > 255.0);
        // A raw range within the field.
        let (low, high, _) =
            engineering_range(&to_raw, Holds::Integers(0, 255), Some((10.0, 20.0))).unwrap();
        assert_eq!(
            (
                to_raw.raw(low).unwrap().round(),
                to_raw.raw(high).unwrap().round()
            ),
            (10.0, 20.0)
        );
        // A float field holds nearly everything, so the range is cut at
        // ±(2^53 - 1).
        let (low, high, cut) = engineering_range(&to_raw, Holds::Float32, None).unwrap();
        assert_eq!((low, high, cut), (-(SAFE as f64), SAFE as f64, true));
        // A falling table that doesn't extrapolate holds its points' values.
        let table = ToRaw::Table {
            points: vec![(0.0, 100.0), (10.0, 0.0)],
            extrapolate: false,
        };
        let (low, high, _) = engineering_range(&table, Holds::Integers(0, 255), None).unwrap();
        assert_eq!((low, high), (0.0, 10.0));
        assert_eq!(
            engineering_range(&to_raw, Holds::Integers(0, 255), Some((300.0, 400.0))),
            Err(Empty::NoValue)
        );
        // A raw end taken to engineering and back can land a double outside
        // the range; the search starts nearby.
        let skewed = ToRaw::Inverse {
            scale: 3.0,
            offset: 0.1,
        };
        let (low, _, _) =
            engineering_range(&skewed, Holds::Float64, Some((1.0 / 3.0, f64::INFINITY))).unwrap();
        assert!(skewed.raw(low).unwrap() >= 1.0 / 3.0 && low < 0.2);
        // Every value a byte holds through v - 1e17 is past 2^53.
        let far = ToRaw::Inverse {
            scale: 1.0,
            offset: -1e17,
        };
        assert_eq!(
            engineering_range(&far, Holds::Integers(0, 255), None),
            Err(Empty::PastSafe)
        );
    }
}
