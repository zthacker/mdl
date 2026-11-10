//! Random bindings files (`mdl_testkit::bindings`): a valid file resolves
//! every native id at every instant as `spec/bindings.md` §4 says, in any
//! order of its rows; a faulty one gives exactly the diagnostics §1, §3,
//! §4 and §7 call for, duplicates found across the ways one id is written
//! and only among the rows that take part in a timeline.

use mdl::bindings::{Bindings, BindingsError, NativeId, Resolver};
use mdl::time::Timestamp;
use mdl_testkit::bindings::{Key, Row, expected, faulty, ids, instants, resolve, text, valid};
use proptest::prelude::*;

proptest! {
    #[test]
    fn a_bindings_file_means_the_same_in_any_order(
        (rows, shuffled) in valid().prop_flat_map(|rows| (Just(rows.clone()), Just(rows).prop_shuffle()))
    ) {
        let first = read(&rows)?;
        let second = read(&shuffled)?;
        for (family, key) in ids(&rows) {
            for at in instants(&rows) {
                let want = resolve(&rows, family, &key, at);
                prop_assert_eq!(&bound(&first, family, &key, at), &want, "{} {:?} at {}", family, key, at);
                prop_assert_eq!(&bound(&second, family, &key, at), &want, "{} {:?} at {}", family, key, at);
            }
        }
    }

    #[test]
    fn a_bindings_file_reports_every_problem_in_its_rows(rows in faulty()) {
        let found: Vec<(String, String)> = match Bindings::from_toml(&text(&rows)) {
            Err(BindingsError::Invalid(diagnostics)) => {
                let mut found: Vec<(String, String)> = diagnostics
                    .iter()
                    .map(|d| (d.code.as_str().to_string(), d.path.clone()))
                    .collect();
                found.sort();
                found
            }
            Err(error) => return Err(TestCaseError::fail(format!("{error}\n{}", text(&rows)))),
            Ok(_) => Vec::new(),
        };
        prop_assert_eq!(found, expected(&rows), "{}", text(&rows));
    }
}

fn read(rows: &[Row]) -> Result<Bindings, TestCaseError> {
    Bindings::from_toml(&text(rows))
        .map_err(|error| TestCaseError::fail(format!("{error}\n{}", text(rows))))
}

fn native(key: &Key) -> NativeId {
    match key {
        Key::Int(n) => match i64::try_from(*n) {
            Ok(n) => NativeId::from(n),
            Err(_) => NativeId::from(u64::try_from(*n).unwrap_or(0)),
        },
        Key::Text(text) => NativeId::from_text(text.clone()),
    }
}

fn bound(
    bindings: &Bindings,
    family: &str,
    key: &Key,
    at: i128,
) -> Option<(String, Option<String>)> {
    let at = Timestamp::from_unix_nanos(i64::try_from(at).unwrap());
    bindings
        .resolve(family, &native(key), at)
        .map(|binding| (binding.asset.clone(), binding.set.clone()))
}
