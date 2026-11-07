use proptest::prelude::*;

use super::*;

fn unit(code: &str) -> Unit {
    Unit::parse(code).unwrap()
}

fn convert(value: f64, from: &str, to: &str) -> Option<f64> {
    unit(from).convert(value, &unit(to))
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * (1.0 + b.abs())
}

#[test]
fn prefixes_scale_metric_units() {
    // mV is 1 base unit and V 1000 (the gram is the base): 1500 / 1000.
    assert_eq!(convert(1500.0, "mV", "V"), Some(1.5));
    assert_eq!(convert(2.0, "km", "m"), Some(2000.0));
    assert_eq!(convert(1.0, "hPa", "Pa"), Some(100.0));
    assert_eq!(convert(1.0, "daN", "N"), Some(10.0));
    assert_eq!(convert(3.0, "uA", "A"), Some(3e-6));
    assert_eq!(convert(1.0, "kg", "g"), Some(1000.0));
    assert!(close(convert(36.0, "km/h", "m/s").unwrap(), 10.0));
}

#[test]
fn products_quotients_and_exponents() {
    assert!(unit("kg.m/s2").is_compatible(&unit("N")));
    assert!(unit("m.s-2").is_compatible(&unit("m/s2")));
    assert!(unit("/s").is_compatible(&unit("Hz")));
    assert!(unit("(kg.m)/(s.s)").is_compatible(&unit("N")));
    assert_eq!(convert(1.0, "m2", "cm2"), Some(10000.0));
    assert_eq!(convert(1.0, "10*3", "1"), Some(1000.0));
    assert_eq!(convert(1.0, "10^-2", "%"), Some(1.0));
    assert_eq!(convert(50.0, "%", "1"), Some(0.5));
}

#[test]
fn dimensions_keep_charge_apart_from_energy() {
    // A volt is a joule per coulomb, so they don't convert.
    assert!(!unit("V").is_compatible(&unit("J")));
    assert!(unit("V").is_compatible(&unit("J/C")));
    assert!(unit("Ohm").is_compatible(&unit("V/A")));
    assert!(unit("S").is_compatible(&unit("/Ohm")));
    assert!(unit("F").is_compatible(&unit("C/V")));
    assert!(unit("Wb").is_compatible(&unit("V.s")));
    assert!(unit("W").is_compatible(&unit("V.A")));
    assert!(!unit("m").is_compatible(&unit("s")));
    assert_eq!(convert(1.0, "m", "s"), None);
}

#[test]
fn celsius_and_fahrenheit_convert_through_kelvin() {
    assert_eq!(convert(25.0, "Cel", "K"), Some(298.15));
    assert!(close(convert(298.15, "K", "Cel").unwrap(), 25.0));
    assert!(close(convert(77.0, "[degF]", "Cel").unwrap(), 25.0));
    assert!(close(convert(-40.0, "Cel", "[degF]").unwrap(), -40.0));
    assert!(close(convert(1.0, "Cel", "mK").unwrap(), 274150.0));
}

#[test]
fn us_customary_and_angles() {
    assert_eq!(convert(1.0, "[ft_i]", "m"), Some(0.3048));
    // (1 × 6894757.293168361) / 1000, rounded once more by the division.
    assert_eq!(
        convert(1.0, "[psi]", "Pa"),
        Some(6894757.293168361 / 1000.0)
    );
    assert!(close(convert(1.0, "[kn_i]", "km/h").unwrap(), 1.852));
    assert!(close(
        convert(180.0, "deg", "rad").unwrap(),
        std::f64::consts::PI
    ));
    assert!(close(convert(60.0, "'", "deg").unwrap(), 1.0));
}

#[test]
fn annotations_must_match() {
    assert!(unit("{counts}").is_compatible(&unit("{counts}")));
    assert!(!unit("{counts}").is_compatible(&unit("1")));
    assert!(!unit("{counts}").is_compatible(&unit("{dBm}")));
    assert!(unit("kg{dry}").is_compatible(&unit("g{dry}")));
    assert!(unit("{a}.{b}").is_compatible(&unit("{b}.{a}")));
}

#[test]
fn codes_mdl_doesnt_know_are_refused() {
    for code in [
        "", "foo", "dB", "dBm", "Ma", "mmin", "Cel/s", "m^2", "10*", "(m", "m)", "kg{", "{a}b",
        "m.", "m//s", "0", "m+", "m999", "[degF].s", "m0", "s-0", "{}", "m{}", "010", "ym13",
        "Ym99",
    ] {
        assert!(Unit::parse(code).is_err(), "{code:?}");
    }
    assert_eq!(
        Unit::parse("dBm").unwrap_err().to_string(),
        "\"dBm\" isn't a unit MDL knows: \"dBm\" is no unit or prefixed metric unit MDL knows"
    );
}

#[test]
fn between_codes_identical_or_missing_units_pass_through() {
    assert_eq!(convert_between(1.5, Some("V"), Some("V")), Some(1.5));
    assert_eq!(convert_between(1.5, None, Some("V")), Some(1.5));
    assert_eq!(convert_between(1500.0, Some("mV"), Some("V")), Some(1.5));
    assert_eq!(convert_between(1.0, Some("m"), Some("s")), None);
}

proptest! {
    #[test]
    fn a_conversion_comes_back(
        value in -1e9_f64..1e9,
        pair in prop_oneof![
            Just(("mV", "V")), Just(("km/h", "[kn_i]")), Just(("Cel", "[degF]")),
            Just(("[psi]", "kPa")), Just(("deg", "rad")), Just(("%", "[ppm]")),
        ],
    ) {
        let (a, b) = pair;
        let there = convert(value, a, b).unwrap();
        let back = convert(there, b, a).unwrap();
        prop_assert!((back - value).abs() <= 1e-9 * (1.0 + value.abs()), "{} {} {}", a, b, back);
    }
}

#[test]
fn data_units_take_decimal_and_binary_prefixes() {
    assert_eq!(convert(1.0, "By", "bit"), Some(8.0));
    assert_eq!(convert(1.0, "kbit/s", "bit/s"), Some(1000.0));
    assert_eq!(convert(1.0, "KiBy", "By"), Some(1024.0));
    assert_eq!(convert(1.0, "MiBy", "KiBy"), Some(1024.0));
    assert!(unit("kBd").is_compatible(&unit("Hz")));
}

#[test]
fn a_unit_converted_to_itself_is_unchanged() {
    // The formula would round: (v + 273.15) - 273.15 isn't always v.
    for code in ["Cel", "[degF]", "mV", "[ft_i]", "km/h"] {
        let same = unit(code);
        for value in [750.8585, -40.0, 0.1, 1e300] {
            assert_eq!(same.convert(value, &unit(code)), Some(value), "{code}");
        }
    }
}
