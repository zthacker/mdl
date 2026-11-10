//! Any bundle the IR's shape allows (`mdl_testkit::shape`), spelled in TOML
//! as `toml` writes its JSON, reads as the bundle the JSON reads as: the
//! TOML frontend is the IR spelled in TOML, every key and value of it.

use mdl::read::read_bundle_value;
use mdl::source::Sources;
use proptest::prelude::*;

proptest! {
    #[test]
    fn any_bundle_spelled_in_toml_reads_as_the_same_bundle(
        value in mdl_testkit::shape::bundle()
    ) {
        let from_json = read_bundle_value(&value).unwrap().value;
        let text = toml::to_string_pretty(&value).unwrap();
        let mut sources = Sources::new();
        sources.add("bundle.toml", text.as_str());
        let parsed = mdl_toml::read(sources.iter()).map_err(|errors| {
            let shown: Vec<String> = errors.iter().map(|error| format!("{error:?}")).collect();
            TestCaseError::fail(format!("{}\n---\n{text}", shown.join("\n")))
        })?;
        prop_assert_eq!(parsed.bundle, from_json, "{}", text);
    }
}
