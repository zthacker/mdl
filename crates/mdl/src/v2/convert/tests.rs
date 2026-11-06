use proptest::prelude::*;

use super::*;

fn points(pairs: &[(f64, f64)]) -> Vec<Point> {
    pairs
        .iter()
        .map(|&(raw, value)| Point::new(raw, value))
        .collect()
}

fn table_of(pairs: &[(f64, f64)], interpolation: Interpolation, extrapolate: bool) -> Conversion {
    Conversion::Table {
        points: points(pairs),
        interpolation: Some(interpolation),
        extrapolate: Some(extrapolate),
    }
}

fn spline_of(pairs: &[(f64, f64)], extrapolate: bool) -> Conversion {
    Conversion::Spline {
        points: points(pairs),
        extrapolate: Some(extrapolate),
    }
}

const THERMISTOR: &[(f64, f64)] = &[(0.0, -40.0), (100.0, 0.0), (300.0, 60.0)];

#[test]
fn linear_and_polynomial_conversions() {
    let poly = Conversion::poly(vec![1.0, 2.0, 3.0]);
    // 1 + 2x + 3x^2 at x = 2.
    assert_eq!(convert(&poly, 2.0), Converted::Number(17.0));
    assert_eq!(
        convert(&Conversion::linear(0.5, -1.0), 4.0),
        Converted::Number(1.0)
    );
    assert_eq!(
        convert(&Conversion::states(Vec::new()), 1.0),
        Converted::NotNumeric
    );
}

#[test]
fn a_linear_table_draws_straight_lines_between_its_points() {
    let table = table_of(THERMISTOR, Interpolation::Linear, false);
    assert_eq!(convert(&table, 0.0), Converted::Number(-40.0));
    assert_eq!(convert(&table, 50.0), Converted::Number(-20.0));
    assert_eq!(convert(&table, 100.0), Converted::Number(0.0));
    assert_eq!(convert(&table, 200.0), Converted::Number(30.0));
    assert_eq!(convert(&table, 300.0), Converted::Number(60.0));
    assert_eq!(convert(&table, -1.0), Converted::Outside);
    assert_eq!(convert(&table, 300.5), Converted::Outside);
    // NaN is reported as not finite, not as outside.
    assert!(matches!(convert(&table, f64::NAN), Converted::Number(x) if x.is_nan()));
}

#[test]
fn an_extrapolating_table_extends_its_end_segments() {
    let table = table_of(THERMISTOR, Interpolation::Linear, true);
    assert_eq!(convert(&table, -100.0), Converted::Number(-80.0));
    assert_eq!(convert(&table, 400.0), Converted::Number(90.0));
}

#[test]
fn a_step_table_holds_each_value_until_the_next_point() {
    let table = table_of(THERMISTOR, Interpolation::Step, false);
    assert_eq!(convert(&table, 0.0), Converted::Number(-40.0));
    assert_eq!(convert(&table, 99.9), Converted::Number(-40.0));
    assert_eq!(convert(&table, 100.0), Converted::Number(0.0));
    assert_eq!(convert(&table, 299.0), Converted::Number(0.0));
    assert_eq!(convert(&table, 300.0), Converted::Number(60.0));
    assert_eq!(convert(&table, 301.0), Converted::Outside);
    let extended = table_of(THERMISTOR, Interpolation::Step, true);
    assert_eq!(convert(&extended, -5.0), Converted::Number(-40.0));
    assert_eq!(convert(&extended, 301.0), Converted::Number(60.0));
}

#[test]
fn a_natural_spline_passes_through_its_points_and_curves_between_them() {
    let spline = spline_of(&[(0.0, 0.0), (1.0, 1.0), (2.0, 0.0)], false);
    // The middle point's second derivative solves 4 m = 6 (-1 - 1): -3.
    assert_eq!(
        second_derivatives(&points(&[(0.0, 0.0), (1.0, 1.0), (2.0, 0.0)])),
        [0.0, -3.0, 0.0]
    );
    assert_eq!(convert(&spline, 0.0), Converted::Number(0.0));
    assert_eq!(convert(&spline, 1.0), Converted::Number(1.0));
    assert_eq!(convert(&spline, 2.0), Converted::Number(0.0));
    // (-3 * 0.5^3) / 6 + 0 + (1 + 3/6) * 0.5 = -0.0625 + 0.75.
    assert_eq!(convert(&spline, 0.5), Converted::Number(0.6875));
    assert_eq!(convert(&spline, 2.5), Converted::Outside);
    // Two points make a straight line.
    let two = spline_of(&[(0.0, 1.0), (4.0, 9.0)], false);
    assert_eq!(convert(&two, 1.0), Converted::Number(3.0));
}

#[test]
fn an_extrapolating_spline_goes_on_straight_at_its_end_slopes() {
    let spline = spline_of(&[(0.0, 0.0), (1.0, 1.0), (2.0, 0.0)], true);
    // End slopes: 1 - (1 * -3) / 6 = 1.5 at the start, -1 + (1 * -3) / 6 =
    // -1.5 at the end.
    assert_eq!(convert(&spline, -1.0), Converted::Number(-1.5));
    assert_eq!(convert(&spline, 3.0), Converted::Number(-1.5));
}

#[test]
fn only_monotonic_linear_tables_invert_exactly() {
    assert!(inverts_exactly(&Conversion::linear(2.0, 1.0)));
    assert!(inverts_exactly(&table_of(
        THERMISTOR,
        Interpolation::Linear,
        false
    )));
    let falling = table_of(&[(0.0, 10.0), (5.0, 0.0)], Interpolation::Linear, false);
    assert!(inverts_exactly(&falling));
    let peaked = table_of(
        &[(0.0, 0.0), (1.0, 5.0), (2.0, 0.0)],
        Interpolation::Linear,
        false,
    );
    assert!(!inverts_exactly(&peaked));
    assert!(!inverts_exactly(&table_of(
        THERMISTOR,
        Interpolation::Step,
        false
    )));
    assert!(!inverts_exactly(&spline_of(THERMISTOR, false)));
    assert!(!inverts_exactly(&Conversion::poly(vec![0.0, 1.0])));
}

#[test]
fn a_table_reads_backwards_from_value_to_raw() {
    let mut field = crate::v2::ir::Field::new("t", crate::v2::ir::ScalarType::Uint);
    field.conversion = Some(table_of(THERMISTOR, Interpolation::Linear, false));
    assert_eq!(inverse(&field, -20.0), Converted::Number(50.0));
    assert_eq!(inverse(&field, 30.0), Converted::Number(200.0));
    assert_eq!(inverse(&field, 61.0), Converted::Outside);
    field.conversion = Some(table_of(
        &[(0.0, 10.0), (5.0, 0.0)],
        Interpolation::Linear,
        false,
    ));
    assert_eq!(inverse(&field, 4.0), Converted::Number(3.0));
}

fn increasing_points() -> impl Strategy<Value = Vec<Point>> {
    proptest::collection::vec((0.1_f64..100.0, -1000.0_f64..1000.0), 2..12).prop_map(|steps| {
        let mut raw = -50.0;
        steps
            .into_iter()
            .map(|(step, value)| {
                raw += step;
                Point::new(raw, value)
            })
            .collect()
    })
}

proptest! {
    #[test]
    fn tables_and_splines_hit_every_point_exactly(points in increasing_points()) {
        let tables = [
            Conversion::Table { points: points.clone(), interpolation: None, extrapolate: None },
            Conversion::Table {
                points: points.clone(),
                interpolation: Some(Interpolation::Step),
                extrapolate: None,
            },
        ];
        for point in &points {
            for table in &tables {
                prop_assert_eq!(convert(table, point.raw), Converted::Number(point.value));
            }
        }
        let spline = Conversion::Spline { points: points.clone(), extrapolate: None };
        for point in &points {
            prop_assert_eq!(convert(&spline, point.raw), Converted::Number(point.value));
        }
    }

    #[test]
    fn a_linear_table_stays_between_its_neighbours(points in increasing_points(), t in 0.0_f64..1.0) {
        let table = Conversion::Table { points: points.clone(), interpolation: None, extrapolate: None };
        for pair in points.windows(2) {
            let x = pair[0].raw + t * (pair[1].raw - pair[0].raw);
            let value = convert(&table, x).number().unwrap();
            let (low, high) = (pair[0].value.min(pair[1].value), pair[0].value.max(pair[1].value));
            prop_assert!(low - 1e-9 <= value && value <= high + 1e-9);
        }
    }
}
