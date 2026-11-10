//! IR JSON round trips over any bundle the IR's shape allows
//! (`mdl_testkit::shape`): read and written back, a bundle's canonical JSON
//! is what was read; reading that text, as a store or artifact holds it,
//! gives the same bundle and digest; a loose spelling reads as the same
//! bundle; and two bundles are equal exactly when their canonical JSON is.

use mdl::canonical::to_canonical_string;
use mdl::read::{read_bundle, read_bundle_value};
use mdl_testkit::shape::{bundle, finite, loosely_spelled, scalar};
use proptest::prelude::*;
use proptest::sample::Index;
use serde_json::{Number, Value};

proptest! {
    #[test]
    fn a_bundle_writes_back_the_canonical_json_it_was_read_from(value in bundle()) {
        let read = read_bundle_value(&value)
            .map_err(|error| TestCaseError::fail(format!("read: {error}")))?;
        let written = serde_json::to_value(&read.value).unwrap();
        prop_assert_eq!(to_canonical_string(&written).unwrap(), read.canonical.clone());
        let again = read_bundle_value(&written).unwrap();
        prop_assert_eq!(&again.value, &read.value);
        prop_assert_eq!(&again.digest, &read.digest);
        // As text, where integers past 2^53 are digits a reader parses.
        let from_text = read_bundle(read.canonical.as_bytes())
            .map_err(|error| TestCaseError::fail(format!("reread: {error}")))?;
        prop_assert_eq!(&from_text.value, &read.value);
        prop_assert_eq!(&from_text.digest, &read.digest);
    }

    #[test]
    fn a_loosely_spelled_bundle_reads_as_the_bundle_and_writes_canonically(
        (strict, loose) in loosely_spelled()
    ) {
        let strict = read_bundle_value(&strict).unwrap();
        let read = read_bundle_value(&loose)
            .map_err(|error| TestCaseError::fail(format!("read: {error}")))?;
        prop_assert_eq!(&read.value, &strict.value);
        let written = serde_json::to_value(&read.value).unwrap();
        prop_assert_eq!(to_canonical_string(&written).unwrap(), strict.canonical);
    }

    /// One leaf changed to another value, or a number respelled (`3` as
    /// `3.0`): the two bundles are equal exactly when their canonical JSON
    /// is. A change that no longer reads says nothing.
    #[test]
    fn bundles_are_equal_exactly_when_their_canonical_json_is(
        value in bundle(),
        at in any::<Index>(),
        change in prop_oneof![
            1 => Just(None),
            1 => finite().prop_map(Some),
            1 => scalar().prop_map(Some),
            1 => any::<bool>().prop_map(|b| Some(Value::Bool(b))),
        ],
    ) {
        let mut changed = value.clone();
        let mut leaves = Vec::new();
        leaf_pointers(&changed, String::new(), &mut leaves);
        let pointer = &leaves[at.index(leaves.len())];
        if let Some(leaf) = changed.pointer_mut(pointer) {
            *leaf = match change {
                Some(replacement) => replacement,
                None => respelled(leaf),
            };
        }
        let (Ok(a), Ok(b)) = (read_bundle_value(&value), read_bundle_value(&changed)) else {
            return Ok(());
        };
        prop_assert_eq!(
            a.value == b.value,
            a.canonical == b.canonical,
            "{}: {:?} became {:?}",
            pointer,
            value.pointer(pointer),
            changed.pointer(pointer)
        );
    }
}

/// The JSON Pointer of every leaf: every string, number and bool.
fn leaf_pointers(value: &Value, at: String, out: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                leaf_pointers(item, format!("{at}/{index}"), out);
            }
        }
        Value::Object(members) => {
            for (key, item) in members {
                let key = key.replace('~', "~0").replace('/', "~1");
                leaf_pointers(item, format!("{at}/{key}"), out);
            }
        }
        _ => out.push(at),
    }
}

/// The same number spelled the other way, as canonical JSON writes it the
/// same: a whole double as the integer it's written as, an integer as the
/// double written as it. Anything else as it is.
fn respelled(leaf: &Value) -> Value {
    let Value::Number(n) = leaf else {
        return leaf.clone();
    };
    let Ok(canonical) = to_canonical_string(leaf) else {
        return leaf.clone();
    };
    if n.is_f64() {
        return canonical.parse::<i64>().map_or(leaf.clone(), Value::from);
    }
    n.as_f64()
        .and_then(Number::from_f64)
        .map(Value::Number)
        .filter(|double| to_canonical_string(double).ok().as_ref() == Some(&canonical))
        .unwrap_or_else(|| leaf.clone())
}
