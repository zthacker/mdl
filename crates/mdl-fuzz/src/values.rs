//! Targets for values written as text: unit codes and instants.

use mdl::time::Timestamp;
use mdl::units::Unit;

/// Values each pair of units converts, from zero to near either end of a
/// double.
const VALUES: [f64; 6] = [0.0, 1.0, -40.0, 273.15, 1e-300, 1e300];

/// One unit code, or two on separate lines (`spec/units.md` §1). A code
/// that parses is compatible with itself and converts a value to itself
/// unchanged (§4). Two that parse are compatible both ways or neither
/// (§3), and convert exactly when they're compatible.
pub fn units(data: &[u8]) {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let (first, second) = match text.split_once('\n') {
        Some((first, second)) => (first, Some(second)),
        None => (text, None),
    };
    let mut parsed = Vec::new();
    for code in [Some(first), second].into_iter().flatten() {
        match Unit::parse(code) {
            Ok(unit) => {
                assert!(
                    unit.is_compatible(&unit),
                    "{code:?} isn't compatible with itself"
                );
                for value in VALUES {
                    assert_eq!(
                        unit.convert(value, &unit),
                        Some(value),
                        "{code:?} to itself"
                    );
                }
                parsed.push(unit);
            }
            Err(error) => assert!(!error.to_string().is_empty(), "{code:?}: {error:?}"),
        }
    }
    if let [from, to] = parsed.as_slice() {
        let compatible = from.is_compatible(to);
        assert_eq!(
            compatible,
            to.is_compatible(from),
            "{text:?}: compatible one way"
        );
        for value in VALUES {
            assert_eq!(
                from.convert(value, to).is_some(),
                compatible,
                "{text:?}: converts {value} exactly when compatible"
            );
        }
    }
}

/// An instant, as MDL spells one (`spec/bindings.md` §2) and as RFC 3339
/// text from data MDL doesn't control. MDL's spelling reads only as itself;
/// an instant read either way is written in MDL's spelling, which reads
/// back as the same instant both ways.
pub fn timestamp(data: &[u8]) {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    match text.parse::<Timestamp>() {
        Ok(instant) => {
            assert_eq!(
                instant.to_string(),
                text,
                "MDL's spelling reads only as itself"
            );
            reads_back(instant);
        }
        Err(error) => assert!(!error.to_string().is_empty(), "{text:?}: {error:?}"),
    }
    match Timestamp::from_rfc3339(text) {
        Ok(instant) => reads_back(instant),
        Err(error) => assert!(!error.to_string().is_empty(), "{text:?}: {error:?}"),
    }
}

/// The instant's spelling reads back as it, both ways.
fn reads_back(instant: Timestamp) {
    let written = instant.to_string();
    assert_eq!(
        written.parse::<Timestamp>().ok(),
        Some(instant),
        "{written}"
    );
    assert_eq!(
        Timestamp::from_rfc3339(&written).ok(),
        Some(instant),
        "{written}"
    );
}
