//! What a raw value means (`spec/decoding.md` §7): units and their
//! conversions (`units.md` §4), numeric conversions (§7.1), sentinels
//! (§7.2) and the sample a field's raw value gives (§7.3).

use super::{Expected, Meaning, Numeric, Placed, Raw, Sample, Sentinel};

// ---- Units ------------------------------------------------------------------

/// Codes of one dimension each, which convert into each other.
pub(super) const UNIT_GROUPS: &[&[&str]] = &[
    &["mV", "V", "kV"],
    &["mA", "A"],
    &["K", "Cel", "[degF]"],
    &["ms", "s", "min", "h"],
    &["m", "km", "[ft_i]"],
    &["rad", "mrad", "deg"],
];

/// A unit's factor (`units.md` §2): its prefix's factor times its atom's,
/// in binary64, for the codes of [`UNIT_GROUPS`].
pub(super) fn factor(code: &str) -> f64 {
    match code {
        "mV" => 1e-3 * 1000.0,
        "V" => 1000.0,
        "kV" => 1e3 * 1000.0,
        "mA" | "ms" | "mrad" => 1e-3 * 1.0,
        "km" => 1e3 * 1.0,
        "min" => 60.0,
        "h" => 3600.0,
        "[ft_i]" => 0.3048,
        "deg" => 0.017453292519943295,
        _ => 1.0,
    }
}

/// `v` from unit `from` into `to` (`units.md` §4).
pub(super) fn convert_unit(v: f64, from: &str, to: &str) -> f64 {
    if from == to {
        return v;
    }
    let temperature = |code: &str| matches!(code, "Cel" | "[degF]");
    if !temperature(from) && !temperature(to) {
        return (v * factor(from)) / factor(to);
    }
    let kelvin = match from {
        "Cel" => v + 273.15,
        "[degF]" => ((v + 459.67) * 5.0) / 9.0,
        other => v * factor(other),
    };
    match to {
        "Cel" => kelvin - 273.15,
        "[degF]" => ((kelvin * 9.0) / 5.0) - 459.67,
        other => kelvin / factor(other),
    }
}

// ---- Conversions --------------------------------------------------------

/// `x` through a numeric conversion (§7.1); `None` outside a table's or
/// spline's points when it doesn't extrapolate.
pub(super) fn convert(conversion: &Numeric, x: f64) -> Option<f64> {
    match conversion {
        Numeric::Plain => Some(x),
        Numeric::Linear { scale, offset } => Some(x * scale + offset),
        Numeric::Poly(coefficients) => {
            let mut highest_first = coefficients.iter().rev();
            let mut y = highest_first.next().copied().unwrap_or(0.0);
            for c in highest_first {
                y = y * x + c;
            }
            Some(y)
        }
        Numeric::Table {
            points,
            step,
            extrapolate,
        } => table(points, *step, *extrapolate, x),
        Numeric::Spline {
            points,
            extrapolate,
        } => spline(points, *extrapolate, x),
    }
}

/// The segment `x` falls in: `Ok(i)` between points `i` and `i + 1`,
/// `Err(true)` below the first point, `Err(false)` above the last.
pub(super) fn segment(points: &[(f64, f64)], x: f64) -> Result<usize, bool> {
    let last = points.len() - 1;
    if x < points[0].0 {
        return Err(true);
    }
    if x > points[last].0 {
        return Err(false);
    }
    Ok((0..last)
        .find(|&i| points[i].0 < x && x < points[i + 1].0)
        .unwrap_or(0))
}

pub(super) fn table(points: &[(f64, f64)], step: bool, extrapolate: bool, x: f64) -> Option<f64> {
    if let Some(&(_, value)) = points.iter().find(|(raw, _)| *raw == x) {
        return Some(value);
    }
    let last = points.len() - 1;
    let i = match segment(points, x) {
        Ok(i) => i,
        Err(_) if !extrapolate => return None,
        // Extrapolated, a step table gives the nearest end point's value.
        Err(below) if step => return Some(if below { points[0].1 } else { points[last].1 }),
        Err(below) => {
            if below {
                0
            } else {
                last - 1
            }
        }
    };
    let ((ri, vi), (rj, vj)) = (points[i], points[i + 1]);
    if step {
        return Some(vi);
    }
    Some(vi + ((x - ri) * (vj - vi)) / (rj - ri))
}

/// The natural cubic spline through the points, by §7.1's sweep.
pub(super) fn spline(points: &[(f64, f64)], extrapolate: bool, x: f64) -> Option<f64> {
    if let Some(&(_, value)) = points.iter().find(|(raw, _)| *raw == x) {
        return Some(value);
    }
    let n = points.len() - 1;
    let r: Vec<f64> = points.iter().map(|point| point.0).collect();
    let v: Vec<f64> = points.iter().map(|point| point.1).collect();
    let h: Vec<f64> = (0..n).map(|i| r[i + 1] - r[i]).collect();
    let s: Vec<f64> = (0..n).map(|i| (v[i + 1] - v[i]) / h[i]).collect();
    let mut c = vec![0.0; n];
    let mut d = vec![0.0; n];
    for i in 1..n {
        let p = 2.0 * (h[i - 1] + h[i]) - h[i - 1] * c[i - 1];
        c[i] = h[i] / p;
        d[i] = (6.0 * (s[i] - s[i - 1]) - h[i - 1] * d[i - 1]) / p;
    }
    let mut m = vec![0.0; n + 1];
    if n >= 2 {
        m[n - 1] = d[n - 1];
        for i in (1..n - 1).rev() {
            m[i] = d[i] - c[i] * m[i + 1];
        }
    }
    let i = match segment(points, x) {
        Ok(i) => i,
        Err(_) if !extrapolate => return None,
        Err(true) => return Some(v[0] + (x - r[0]) * (s[0] - h[0] * m[1] / 6.0)),
        Err(false) => {
            return Some(v[n] + (x - r[n]) * (s[n - 1] + h[n - 1] * m[n - 1] / 6.0));
        }
    };
    let left = r[i + 1] - x;
    let right = x - r[i];
    Some(
        (m[i] * left * left * left + m[i + 1] * right * right * right) / (6.0 * h[i])
            + ((v[i] - m[i] * h[i] * h[i] / 6.0) * left) / h[i]
            + ((v[i + 1] - m[i + 1] * h[i] * h[i] / 6.0) * right) / h[i],
    )
}

// ---- What a value means -----------------------------------------------

/// The raw value as a sample states it when its quality isn't `good`.
pub(super) fn as_raw(raw: &Raw) -> Expected {
    match raw {
        Raw::Uint(n) => Expected::U64(*n),
        Raw::Int(n) => Expected::I64(*n),
        Raw::Float(x) => Expected::F64(*x),
        Raw::Bool(b) => Expected::Bool(*b),
        Raw::Str(text) => Expected::Str(text.clone()),
        Raw::Bytes(bytes) => Expected::Bytes(bytes.clone()),
        // Only scalars give samples.
        Raw::Array(_) | Raw::Record(_) => Expected::Str(format!("{raw:?}")),
    }
}

/// Whether the raw value is one of the field's sentinels (§7.2): text
/// exactly, an integer or a float numerically.
pub(super) fn is_sentinel(field: &Placed, raw: &Raw) -> bool {
    field.invalid.iter().any(|sentinel| match (sentinel, raw) {
        (Sentinel::Int(n), Raw::Uint(m)) => i128::from(*m) == *n,
        (Sentinel::Int(n), Raw::Int(m)) => i128::from(*m) == *n,
        // Sentinels for floats are small, so the double holds them.
        (Sentinel::Int(n), Raw::Float(x)) => *x == *n as f64,
        (Sentinel::Text(text), Raw::Str(value)) => text == value,
        _ => false,
    })
}

/// What a field's or element's raw value at `path` gives its param (§7.1
/// to §7.3); `None` if it maps to none.
pub(super) fn sample(
    field: &Placed,
    raw: &Raw,
    path: String,
    index: Option<u64>,
    time_offset_ns: Option<i64>,
) -> Option<Sample> {
    let param = field.param.clone()?;
    let (value, quality) = meaning_of(field, raw);
    Some(Sample {
        param,
        value,
        path,
        index,
        time_offset_ns,
        quality,
    })
}

/// A composite member's raw value as a number, what it becomes through
/// its conversion and then into `target`, and its quality (§7.4, §7.3).
pub(super) fn member_value(
    member: &Placed,
    raw: &Raw,
    target: Option<&str>,
) -> (f64, f64, &'static str) {
    let x = match raw {
        Raw::Uint(n) => *n as f64,
        Raw::Int(n) => *n as f64,
        Raw::Float(x) => *x,
        _ => f64::NAN,
    };
    let (conversion, unit) = match &member.meaning {
        Meaning::Number { conversion, units } => (conversion, units.map(|(unit, _)| unit)),
        _ => (&Numeric::Plain, None),
    };
    if is_sentinel(member, raw) {
        return (x, x, "invalid");
    }
    // NaN goes through a table as NaN, and an infinity read is non_finite
    // before it's outside one.
    if !x.is_finite() {
        return (x, x, "non_finite");
    }
    let Some(converted) = convert(conversion, x) else {
        return (x, x, "out_of_calibration");
    };
    let value = match (unit, target) {
        (Some(from), Some(to)) => convert_unit(converted, from, to),
        _ => converted,
    };
    if value.is_finite() {
        (x, value, "good")
    } else {
        (x, value, "non_finite")
    }
}

/// A quality's place in §7.3's order, from the most trustworthy.
pub(super) fn rank(quality: &str) -> usize {
    [
        "good",
        "unknown_state",
        "out_of_calibration",
        "out_of_range",
        "non_finite",
        "invalid",
    ]
    .iter()
    .position(|name| *name == quality)
    .unwrap_or(usize::MAX)
}

pub(super) fn meaning_of(field: &Placed, raw: &Raw) -> (Expected, &'static str) {
    if is_sentinel(field, raw) {
        return (as_raw(raw), "invalid");
    }
    let integer = match raw {
        Raw::Uint(n) => Some(i128::from(*n)),
        Raw::Int(n) => Some(i128::from(*n)),
        _ => None,
    };
    let good = |value| (value, "good");
    match (&field.meaning, raw) {
        (Meaning::U64, Raw::Uint(n)) => good(Expected::U64(*n)),
        (Meaning::I64, _) => good(Expected::I64(
            integer.and_then(|n| i64::try_from(n).ok()).unwrap_or(0),
        )),
        (Meaning::Bool, Raw::Bool(b)) => good(Expected::Bool(*b)),
        (Meaning::Bool, _) => good(Expected::Bool(integer.unwrap_or(0) != 0)),
        (Meaning::Str, Raw::Str(text)) => good(Expected::Str(text.clone())),
        (Meaning::Bytes, Raw::Bytes(bytes)) => good(Expected::Bytes(bytes.clone())),
        (Meaning::TextState(names), Raw::Str(text)) if names.contains(text) => {
            good(Expected::Enum(text.clone()))
        }
        (Meaning::States(states), _) => {
            match states.iter().find(|(state, _)| Some(*state) == integer) {
                Some((_, name)) => good(Expected::Enum(name.clone())),
                None => (as_raw(raw), "unknown_state"),
            }
        }
        (Meaning::Number { conversion, units }, _) => {
            let x = match raw {
                Raw::Uint(n) => *n as f64,
                Raw::Int(n) => *n as f64,
                Raw::Float(x) => *x,
                _ => f64::NAN,
            };
            // NaN or an infinity read is the raw value, whatever converts
            // it; NaN is never outside a table.
            if !x.is_finite() {
                return (as_raw(raw), "non_finite");
            }
            let Some(converted) = convert(conversion, x) else {
                return (as_raw(raw), "out_of_calibration");
            };
            let value = match units {
                Some((from, to)) => convert_unit(converted, from, to),
                None => converted,
            };
            if value.is_finite() {
                good(Expected::F64(value))
            } else {
                (as_raw(raw), "non_finite")
            }
        }
        // A text state the param doesn't list.
        (_, _) => (as_raw(raw), "unknown_state"),
    }
}
