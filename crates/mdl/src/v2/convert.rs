//! Conversions between raw and engineering values, as `spec/decoding.md`
//! §7.1 and `spec/encoding.md` §3.1 define them. Every step is binary64
//! arithmetic, each operation rounded on its own, in the order written
//! here: a second implementation that follows the spec gets the same bits.

use crate::v2::ir::{Conversion, Field, Interpolation, Point};

/// What a conversion makes of a number.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Converted {
    Number(f64),
    /// Outside a table's or spline's points, which it doesn't extrapolate.
    Outside,
    /// A states conversion, which gives a name, not a number.
    NotNumeric,
}

impl Converted {
    pub(crate) fn number(self) -> Option<f64> {
        match self {
            Converted::Number(x) => Some(x),
            Converted::Outside | Converted::NotNumeric => None,
        }
    }
}

/// A raw number through a conversion. NaN goes through as NaN, so it's
/// reported as not finite rather than as outside a table.
pub(crate) fn convert(conversion: &Conversion, x: f64) -> Converted {
    match conversion {
        Conversion::Linear { scale, offset } => Converted::Number(x * scale + offset),
        // Horner's rule: c0 + x(c1 + x(c2 + ...)).
        Conversion::Poly { coefficients } => Converted::Number(
            coefficients
                .iter()
                .rev()
                .fold(0.0, |sum, coefficient| sum * x + coefficient),
        ),
        Conversion::States { .. } => Converted::NotNumeric,
        Conversion::Table {
            points,
            interpolation,
            extrapolate,
        } => table(
            points,
            interpolation.unwrap_or(Interpolation::Linear),
            *extrapolate == Some(true),
            x,
        ),
        Conversion::Spline {
            points,
            extrapolate,
        } => spline(points, *extrapolate == Some(true), x),
    }
}

/// Whether a conversion's inverse is exact and implied, so a field that
/// encodes an argument through it has no `inverse`: linear (with a scale
/// that isn't 0), states, and a linear table whose values only rise or
/// only fall. Others need an explicit inverse.
pub(crate) fn inverts_exactly(conversion: &Conversion) -> bool {
    match conversion {
        Conversion::Linear { .. } | Conversion::States { .. } => true,
        Conversion::Table {
            points,
            interpolation,
            ..
        } => {
            interpolation.unwrap_or(Interpolation::Linear) == Interpolation::Linear
                && strictly_monotonic(points)
        }
        Conversion::Poly { .. } | Conversion::Spline { .. } => false,
    }
}

/// An engineering value through the inverse of a field's conversion, for
/// encoding: as it is with no conversion, `(x - offset) / scale` for a
/// linear one, the table read backwards for a table that inverts exactly,
/// the explicit `inverse` otherwise.
pub(crate) fn inverse(field: &Field, x: f64) -> Converted {
    match (&field.conversion, &field.inverse) {
        (None, _) => Converted::Number(x),
        (Some(Conversion::Linear { scale, offset }), None) => {
            Converted::Number((x - offset) / scale)
        }
        (
            Some(
                conversion @ Conversion::Table {
                    points,
                    extrapolate,
                    ..
                },
            ),
            None,
        ) if inverts_exactly(conversion) => invert_table(points, *extrapolate == Some(true), x),
        (Some(_), Some(inverse)) => convert(inverse, x),
        _ => Converted::NotNumeric,
    }
}

/// Values that only rise or only fall from point to point.
fn strictly_monotonic(points: &[Point]) -> bool {
    let rising = points.windows(2).all(|pair| pair[0].value < pair[1].value);
    let falling = points.windows(2).all(|pair| pair[0].value > pair[1].value);
    points.len() >= 2 && (rising || falling)
}

/// Where a raw value falls among points of increasing raw value.
enum Located {
    /// In the segment from point `i` to point `i + 1`; the last segment
    /// holds its end.
    Segment(usize),
    Below,
    Above,
}

/// `None` for fewer than two points.
fn locate(points: &[Point], x: f64) -> Option<Located> {
    let (first, last) = (points.first()?, points.last()?);
    if points.len() < 2 {
        return None;
    }
    if x < first.raw {
        return Some(Located::Below);
    }
    if x > last.raw {
        return Some(Located::Above);
    }
    // The last point at or below x, kept off the last point so it starts a
    // segment.
    let at_or_below = points.partition_point(|point| point.raw <= x);
    Some(Located::Segment(
        at_or_below.saturating_sub(1).min(points.len() - 2),
    ))
}

/// At a point's raw value, the point's value exactly, which the segment's
/// formula may miss by rounding.
fn at_point(points: &[Point], located: &Located, x: f64) -> Option<f64> {
    let Located::Segment(i) = located else {
        return None;
    };
    [points.get(*i), points.get(i + 1)]
        .into_iter()
        .flatten()
        .find(|point| point.raw == x)
        .map(|point| point.value)
}

/// The straight line through `a` and `b`, at `x`:
/// `a.value + ((x - a.raw) * (b.value - a.value)) / (b.raw - a.raw)`.
fn line(a: Point, b: Point, x: f64) -> f64 {
    a.value + (x - a.raw) * (b.value - a.value) / (b.raw - a.raw)
}

fn table(points: &[Point], interpolation: Interpolation, extrapolate: bool, x: f64) -> Converted {
    if x.is_nan() {
        return Converted::Number(x);
    }
    let Some(located) = locate(points, x) else {
        return Converted::Outside;
    };
    if let Some(value) = at_point(points, &located, x) {
        return Converted::Number(value);
    }
    // locate found at least two points and a segment index of at most
    // n - 2, so every index here is inside.
    let n = points.len();
    let value = match (located, interpolation) {
        (Located::Segment(i), Interpolation::Linear) => line(points[i], points[i + 1], x),
        // The value of the last point at or below x.
        (Located::Segment(i), Interpolation::Step) if x >= points[i + 1].raw => points[i + 1].value,
        (Located::Segment(i), Interpolation::Step) => points[i].value,
        (Located::Below | Located::Above, _) if !extrapolate => return Converted::Outside,
        (Located::Below, Interpolation::Linear) => line(points[0], points[1], x),
        (Located::Above, Interpolation::Linear) => line(points[n - 2], points[n - 1], x),
        (Located::Below, Interpolation::Step) => points[0].value,
        (Located::Above, Interpolation::Step) => points[n - 1].value,
    };
    Converted::Number(value)
}

/// A table that inverts exactly, read from value to raw.
fn invert_table(points: &[Point], extrapolate: bool, x: f64) -> Converted {
    let mut swapped: Vec<Point> = points
        .iter()
        .map(|point| Point::new(point.value, point.raw))
        .collect();
    if swapped.first().map(|p| p.raw) > swapped.last().map(|p| p.raw) {
        swapped.reverse();
    }
    table(&swapped, Interpolation::Linear, extrapolate, x)
}

/// The forward sweep of the natural cubic spline's tridiagonal system, as
/// `spec/decoding.md` §7.1 writes it out: `(cᵢ, dᵢ)` for each point, `(0, 0)`
/// at the ends.
fn sweep(points: &[Point]) -> Vec<(f64, f64)> {
    let n = points.len();
    let mut cd = vec![(0.0, 0.0); n];
    if n < 3 {
        return cd;
    }
    let h = |i: usize| points[i + 1].raw - points[i].raw;
    let slope = |i: usize| (points[i + 1].value - points[i].value) / h(i);
    for i in 1..n - 1 {
        let (c, d) = cd[i - 1];
        let pivot = 2.0 * (h(i - 1) + h(i)) - h(i - 1) * c;
        let r = 6.0 * (slope(i) - slope(i - 1));
        cd[i] = (h(i) / pivot, (r - h(i - 1) * d) / pivot);
    }
    cd
}

/// The second derivatives `Mᵢ` and `Mᵢ₊₁` at the ends of segment `i`, by
/// back substitution from the last point: `M` is 0 at both ends, the one
/// before the last is its `d`, and each earlier one is `d − c × M` of the
/// next.
fn curvature(cd: &[(f64, f64)], i: usize) -> (f64, f64) {
    let n = cd.len();
    let mut above = 0.0;
    for k in (i..n).rev() {
        let m = match cd.get(k) {
            Some(_) if k == 0 || k + 1 >= n => 0.0,
            Some((_, d)) if k + 2 == n => *d,
            Some((c, d)) => d - c * above,
            None => 0.0,
        };
        if k == i {
            return (m, above);
        }
        above = m;
    }
    (0.0, 0.0)
}

/// Every second derivative, for validation and tests.
pub(crate) fn second_derivatives(points: &[Point]) -> Vec<f64> {
    let cd = sweep(points);
    let mut m = vec![0.0; points.len()];
    for (i, slot) in m.iter_mut().enumerate() {
        *slot = curvature(&cd, i).0;
    }
    m
}

/// Whether a spline's slopes and second derivatives are all finite, so
/// finite raw values between its points give finite values.
pub(crate) fn spline_is_finite(points: &[Point]) -> bool {
    let slopes_finite = points.windows(2).all(|p| {
        let h = p[1].raw - p[0].raw;
        h.is_finite() && ((p[1].value - p[0].value) / h).is_finite()
    });
    slopes_finite && second_derivatives(points).iter().all(|m| m.is_finite())
}

fn spline(points: &[Point], extrapolate: bool, x: f64) -> Converted {
    if x.is_nan() {
        return Converted::Number(x);
    }
    let Some(located) = locate(points, x) else {
        return Converted::Outside;
    };
    if let Some(value) = at_point(points, &located, x) {
        return Converted::Number(value);
    }
    // As in table: at least two points, and i at most n - 2.
    let cd = sweep(points);
    let n = points.len();
    let value = match located {
        Located::Segment(i) => {
            let (a, b) = (points[i], points[i + 1]);
            let (m_a, m_b) = curvature(&cd, i);
            let h = b.raw - a.raw;
            let left = b.raw - x;
            let right = x - a.raw;
            (m_a * left * left * left + m_b * right * right * right) / (6.0 * h)
                + (a.value - m_a * h * h / 6.0) * left / h
                + (b.value - m_b * h * h / 6.0) * right / h
        }
        Located::Below | Located::Above if !extrapolate => return Converted::Outside,
        // Natural ends have no curvature, so the spline goes on straight.
        Located::Below => {
            let (a, b) = (points[0], points[1]);
            let (_, m_b) = curvature(&cd, 0);
            let h = b.raw - a.raw;
            let slope = (b.value - a.value) / h - h * m_b / 6.0;
            a.value + (x - a.raw) * slope
        }
        Located::Above => {
            let (a, b) = (points[n - 2], points[n - 1]);
            let (m_a, _) = curvature(&cd, n - 2);
            let h = b.raw - a.raw;
            let slope = (b.value - a.value) / h + h * m_a / 6.0;
            b.value + (x - b.raw) * slope
        }
    };
    Converted::Number(value)
}

#[cfg(test)]
mod tests;
