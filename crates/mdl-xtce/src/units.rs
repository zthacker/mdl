//! XTCE units, which are free text, as UCUM codes.

use mdl::units::Unit;

/// Spellings XTCE documents use for units MDL knows, lowercase, and the
/// UCUM code for each.
const SPELLINGS: &[(&str, &str)] = &[
    ("degc", "Cel"),
    ("deg c", "Cel"),
    ("°c", "Cel"),
    ("celsius", "Cel"),
    ("degrees c", "Cel"),
    ("degf", "[degF]"),
    ("°f", "[degF]"),
    ("fahrenheit", "[degF]"),
    ("kelvin", "K"),
    ("degree", "deg"),
    ("degrees", "deg"),
    ("°", "deg"),
    ("radian", "rad"),
    ("radians", "rad"),
    ("volt", "V"),
    ("volts", "V"),
    ("amp", "A"),
    ("amps", "A"),
    ("ampere", "A"),
    ("amperes", "A"),
    ("watt", "W"),
    ("watts", "W"),
    ("hertz", "Hz"),
    ("second", "s"),
    ("seconds", "s"),
    ("sec", "s"),
    ("secs", "s"),
    ("millisecond", "ms"),
    ("milliseconds", "ms"),
    ("msec", "ms"),
    ("meter", "m"),
    ("meters", "m"),
    ("metre", "m"),
    ("metres", "m"),
    ("percent", "%"),
    ("byte", "By"),
    ("bytes", "By"),
    ("octet", "By"),
    ("octets", "By"),
    ("bits", "bit"),
];

/// What a unit's text is in UCUM.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Ucum {
    Code(String),
    /// A UCUM code that documents often use for another unit.
    Ambiguous {
        reads: &'static str,
        meant: &'static str,
        write: &'static str,
    },
    Unknown,
}

/// The UCUM code for a unit's text: the text itself when it's a code MDL
/// knows, else a spelling from the table. `C` and `F` are codes (the
/// coulomb and the farad) that XTCE documents mostly use for degrees.
pub(crate) fn ucum(text: &str) -> Ucum {
    let text = text.trim();
    match text {
        "C" => {
            return Ucum::Ambiguous {
                reads: "coulomb",
                meant: "degrees Celsius",
                write: "degC",
            };
        }
        "F" => {
            return Ucum::Ambiguous {
                reads: "farad",
                meant: "degrees Fahrenheit",
                write: "degF",
            };
        }
        _ => {}
    }
    if Unit::parse(text).is_ok() {
        return Ucum::Code(text.to_string());
    }
    let lower = text.to_lowercase();
    SPELLINGS
        .iter()
        .find(|(spelling, _)| *spelling == lower)
        .map_or(Ucum::Unknown, |(_, code)| Ucum::Code((*code).to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_is_a_code_mdl_knows() {
        for (spelling, code) in SPELLINGS {
            assert!(Unit::parse(code).is_ok(), "{spelling} -> {code}");
            assert_eq!(spelling.to_lowercase(), *spelling);
        }
    }

    #[test]
    fn codes_pass_and_spellings_map() {
        let code = |text: &str| Ucum::Code(text.to_string());
        assert_eq!(ucum("V"), code("V"));
        assert_eq!(ucum("m/s"), code("m/s"));
        assert_eq!(ucum(" Cel "), code("Cel"));
        assert_eq!(ucum("degC"), code("Cel"));
        assert_eq!(ucum("Volts"), code("V"));
        assert_eq!(ucum("Octets"), code("By"));
        assert_eq!(ucum("units:Becquerel"), Ucum::Unknown);
        assert!(matches!(
            ucum("C"),
            Ucum::Ambiguous {
                reads: "coulomb",
                ..
            }
        ));
        assert!(matches!(ucum("F"), Ucum::Ambiguous { reads: "farad", .. }));
    }
}
