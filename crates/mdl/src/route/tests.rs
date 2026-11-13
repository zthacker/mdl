use serde_json::{Value, json};

use super::*;
use crate::artifact::{Artifacts, Written, build, rebuild};
use crate::bindings::Bindings;
use crate::read::{read_artifact, read_bundle};

const FULL_BUNDLE: &str = include_str!("../../tests/fixtures/full_bundle.json");

fn fixture() -> Value {
    serde_json::from_str(FULL_BUNDLE).unwrap()
}

fn built(value: &Value) -> Artifacts {
    let bundle = read_bundle(&serde_json::to_vec(value).unwrap())
        .unwrap()
        .value;
    build(&validate(bundle).unwrap()).unwrap()
}

fn load(written: &Written) -> Loaded<Artifact> {
    read_artifact(written.canonical.as_bytes()).unwrap()
}

fn sets_of(artifacts: &Artifacts, family: &str) -> Vec<Loaded<Artifact>> {
    artifacts
        .sets
        .values()
        .filter(|w| matches!(&w.artifact, Artifact::Set { set, .. } if set.family == family))
        .map(load)
        .collect()
}

fn family_sets(artifacts: &Artifacts, family: &str) -> FamilySets {
    FamilySets::new(
        &load(&artifacts.families[family]),
        &sets_of(artifacts, family),
        &[load(&artifacts.catalog)],
    )
    .unwrap()
}

const REGISTRY: &str = r#"
mdl_version = "2.0"

[[binding]]
family = "vendor_x"
native_id = 101
asset = "SAT-1"
effective_from = "2026-01-01T00:00:00Z"

[[binding]]
family = "vendor_x"
native_id = 102
asset = "SAT-2"
set = "vendor_x_v2"
effective_from = "2026-01-01T00:00:00Z"

[[binding]]
family = "vendor_j"
native_id = "PX-9"
asset = "SAT-57"
set = "vendor_j_v1"
effective_from = "2026-01-01T00:00:00Z"

[[binding]]
family = "vendor_j"
native_id = "PX-8"
asset = "SAT-58"
effective_from = "2026-01-01T00:00:00Z"

[[binding]]
family = "vendor_j"
native_id = "PX-7"
asset = "SAT-59"
set = "vendor_j_v9"
effective_from = "2026-01-01T00:00:00Z"

[[binding]]
family = "legacy"
native_id = "box-1"
asset = "BOX-1"
set = "legacy_v1"
effective_from = "2026-01-01T00:00:00Z"
"#;

fn registry() -> Bindings {
    Bindings::from_toml(REGISTRY).unwrap()
}

fn now() -> Timestamp {
    "2026-06-01T00:00:00Z".parse().unwrap()
}

/// A vendor_x payload: the alias is bits 48 to 63, big-endian.
fn vendor_x_payload(native_id: u16) -> [u8; 16] {
    let mut payload = [0; 16];
    payload[6..8].copy_from_slice(&native_id.to_be_bytes());
    payload
}

const FMT_2: &[(&str, MetaValue)] = &[("apid", MetaValue::Uint(100)), ("fmt", MetaValue::Uint(2))];

fn code<T: fmt::Debug>(result: Result<T, RouteError>) -> &'static str {
    match result {
        Ok(route) => panic!("expected an error, routed {route:?}"),
        Err(err) => err.code(),
    }
}

#[test]
fn a_family_with_a_version_locator_routes_by_each_messages_version() {
    let artifacts = built(&fixture());
    let sets = family_sets(&artifacts, "vendor_x");
    let payload = vendor_x_payload(101);
    let message = Message::new(Payload::Binary(&payload), now()).with_metadata(FMT_2);
    let route = sets.route(&message, &registry()).unwrap();
    assert_eq!(route.native_id, NativeId::from(101));
    assert_eq!(route.binding.asset, "SAT-1");
    assert_eq!(route.set.name, "vendor_x_v2");
    assert_eq!(route.def_version, &artifacts.sets["vendor_x_v2"].digest);

    // Metadata compares by value: the text "2" is the version 2.
    let text: &[(&str, MetaValue)] = &[("fmt", MetaValue::Text("2"))];
    let message = Message::new(Payload::Binary(&payload), now()).with_metadata(text);
    assert_eq!(
        sets.route(&message, &registry()).unwrap().set.name,
        "vendor_x_v2"
    );
    let signed = [("fmt", MetaValue::from(2))];
    let message = Message::new(Payload::Binary(&payload), now()).with_metadata(&signed);
    assert_eq!(
        sets.route(&message, &registry()).unwrap().set.name,
        "vendor_x_v2"
    );
    // "02" isn't an integer's one spelling, so it's text: no such version.
    let padded = [("fmt", MetaValue::Text("02"))];
    let message = Message::new(Payload::Binary(&payload), now()).with_metadata(&padded);
    assert_eq!(code(sets.route(&message, &registry())), "unknown_version");
}

#[test]
fn a_family_without_one_routes_by_the_binding() {
    let artifacts = built(&fixture());
    let vendor_j = family_sets(&artifacts, "vendor_j");
    let payload = br#"{"sat": "PX-9", "anything": [1, 2, 3]}"#;
    let route = vendor_j
        .route(&Message::new(Payload::Json(payload), now()), &registry())
        .unwrap();
    assert_eq!(route.binding.asset, "SAT-57");
    assert_eq!(route.set.name, "vendor_j_v1");
    assert_eq!(route.def_version, &artifacts.sets["vendor_j_v1"].digest);

    // A fixed alias reads nothing from the payload, so even an empty one routes.
    let legacy = family_sets(&artifacts, "legacy");
    let route = legacy
        .route(&Message::new(Payload::Binary(&[]), now()), &registry())
        .unwrap();
    assert_eq!(route.native_id, NativeId::from_text("box-1"));
    assert_eq!(route.set.name, "legacy_v1");
}

#[test]
fn routing_reads_only_what_the_locators_need() {
    let artifacts = built(&fixture());
    let sets = family_sets(&artifacts, "vendor_x");
    // Eight bytes hold the alias, though a packet needs more.
    let payload = &vendor_x_payload(101)[..8];
    let message = Message::new(Payload::Binary(payload), now()).with_metadata(FMT_2);
    assert!(sets.route(&message, &registry()).is_ok());
}

#[test]
fn each_way_a_message_fails_to_route_has_its_own_code() {
    let artifacts = built(&fixture());
    let vendor_x = family_sets(&artifacts, "vendor_x");
    let vendor_j = family_sets(&artifacts, "vendor_j");
    let registry = registry();
    let binary = |id: u16, metadata: &'static [(&'static str, MetaValue<'static>)]| {
        let payload = vendor_x_payload(id);
        let message = Message::new(Payload::Binary(&payload), now()).with_metadata(metadata);
        vendor_x
            .route(&message, &registry)
            .map(|r| r.set.name.clone())
    };
    let json = |text: &[u8]| {
        vendor_j
            .route(&Message::new(Payload::Json(text), now()), &registry)
            .map(|r| r.set.name.clone())
    };

    let wrong = Message::new(Payload::Json(b"{}"), now()).with_metadata(FMT_2);
    assert_eq!(code(vendor_x.route(&wrong, &registry)), "wrong_encoding");
    let short = Message::new(Payload::Binary(&[0; 7]), now()).with_metadata(FMT_2);
    assert_eq!(
        code(vendor_x.route(&short, &registry)),
        "unreadable_locator"
    );
    assert_eq!(code(binary(101, &[])), "unreadable_locator");
    let twice: &'static [(&str, MetaValue)] =
        &[("fmt", MetaValue::Uint(2)), ("fmt", MetaValue::Uint(2))];
    assert_eq!(code(binary(101, twice)), "unreadable_locator");
    assert_eq!(
        code(binary(101, &[("fmt", MetaValue::Uint(3))])),
        "unknown_version"
    );
    assert_eq!(code(binary(103, FMT_2)), "unknown_asset");
    assert_eq!(code(binary(102, FMT_2)), "unexpected_set");
    let payload = vendor_x_payload(101);
    let before_binding = "2025-12-31T23:59:59Z".parse().unwrap();
    let early = Message::new(Payload::Binary(&payload), before_binding).with_metadata(FMT_2);
    assert_eq!(code(vendor_x.route(&early, &registry)), "unknown_asset");

    assert_eq!(code(json(b"{")), "invalid_json");
    assert_eq!(
        code(json(b"\xef\xbb\xbf{\"sat\": \"PX-9\"}")),
        "invalid_json"
    );
    assert_eq!(
        code(json(br#"{"sat": "PX-9", "sat": "PX-8"}"#)),
        "invalid_json"
    );
    assert_eq!(code(json(br#"{"other": 1}"#)), "unreadable_locator");
    assert_eq!(code(json(br#"{"sat": true}"#)), "unreadable_locator");
    assert_eq!(code(json(br#"{"sat": 1.5}"#)), "unreadable_locator");
    assert_eq!(code(json(br#"{"sat": "PX-8"}"#)), "missing_set");
    assert_eq!(code(json(br#"{"sat": "PX-7"}"#)), "unknown_set");
    assert_eq!(code(json(br#"{"sat": "PX-6"}"#)), "unknown_asset");
}

#[test]
fn route_errors_carry_the_encoding_and_the_locator() {
    let artifacts = built(&fixture());
    let vendor_x = family_sets(&artifacts, "vendor_x");
    let vendor_j = family_sets(&artifacts, "vendor_j");
    let registry = registry();

    let wrong = Message::new(Payload::Json(b"{}"), now()).with_metadata(FMT_2);
    let err = vendor_x.route(&wrong, &registry).unwrap_err();
    let expected = RouteError::WrongEncoding {
        family: "vendor_x".into(),
        expected: Encoding::Binary,
        given: Encoding::Json,
    };
    assert_eq!(err, expected);
    assert_eq!(
        err.to_string(),
        "family vendor_x is binary, but the message has a json payload"
    );

    // vendor_x reads its version from metadata, after its alias.
    let payload = vendor_x_payload(101);
    let bare = Message::new(Payload::Binary(&payload), now());
    let err = vendor_x.route(&bare, &registry).unwrap_err();
    let expected = RouteError::UnreadableLocator {
        family: "vendor_x".into(),
        locator: LocatorRole::Version,
        reason: "the message has no metadata fmt".into(),
    };
    assert_eq!(err, expected);
    assert_eq!(
        err.to_string(),
        "family vendor_x's version locator can't be read: the message has no metadata fmt"
    );

    let message = Message::new(Payload::Json(br#"{"sat": true}"#), now());
    let err = vendor_j.route(&message, &registry).unwrap_err();
    let expected = RouteError::UnreadableLocator {
        family: "vendor_j".into(),
        locator: LocatorRole::Alias,
        reason: "found a boolean, not an integer or a string".into(),
    };
    assert_eq!(err, expected);
    assert_eq!(
        err.to_string(),
        "family vendor_j's alias locator can't be read: found a boolean, not an integer or a \
         string"
    );
    // A number in a reason reads the same whatever serde_json's features.
    let message = Message::new(Payload::Json(br#"{"sat": 1.50}"#), now());
    let err = vendor_j.route(&message, &registry).unwrap_err();
    assert_eq!(
        err.to_string(),
        "family vendor_j's alias locator can't be read: 1.5 isn't an integer that can be read \
         exactly"
    );
}

#[test]
fn route_errors_say_what_to_do() {
    let artifacts = built(&fixture());
    let vendor_j = family_sets(&artifacts, "vendor_j");
    let message = Message::new(Payload::Json(br#"{"sat": "PX-8"}"#), now());
    let err = vendor_j.route(&message, &registry()).unwrap_err();
    assert_eq!(
        err.to_string(),
        "asset SAT-58's binding names no set, and family vendor_j has no version locator to \
         choose one; name the set in the binding"
    );
}

#[test]
fn json_numbers_are_read_by_value() {
    let artifacts = built(&fixture());
    let vendor_j = family_sets(&artifacts, "vendor_j");
    let registry = Bindings::from_toml(
        "mdl_version = \"2.0\"\n[[binding]]\nfamily = \"vendor_j\"\nnative_id = 42\nasset = \"A\"\n\
         set = \"vendor_j_v1\"\neffective_from = \"2026-01-01T00:00:00Z\"",
    )
    .unwrap();
    for payload in [
        &br#"{"sat": 42}"#[..],
        br#"{"sat": 42.0}"#,
        br#"{"sat": "42"}"#,
    ] {
        let message = Message::new(Payload::Json(payload), now());
        assert_eq!(
            vendor_j.route(&message, &registry).unwrap().binding.asset,
            "A"
        );
    }
}

#[test]
fn a_shared_resolver_routes_too() {
    let artifacts = built(&fixture());
    let sets = family_sets(&artifacts, "vendor_x");
    let resolver: Arc<dyn Resolver> = Arc::new(registry());
    let payload = vendor_x_payload(101);
    let message = Message::new(Payload::Binary(&payload), now()).with_metadata(FMT_2);
    assert_eq!(
        sets.route(&message, &resolver).unwrap().binding.asset,
        "SAT-1"
    );
    assert_eq!(
        sets.route(&message, &*resolver).unwrap().binding.asset,
        "SAT-1"
    );
}

#[test]
fn sets_from_different_builds_are_checked_together() {
    let first = built(&fixture());
    let mut renamed = fixture();
    renamed["sets"][0]["name"] = json!("vendor_x_v2_alt");
    let second = built(&renamed);
    let family = load(&first.families["vendor_x"]);
    assert_eq!(family.digest, second.families["vendor_x"].digest);
    let catalog = [load(&first.catalog)];

    let both = [
        load(&first.sets["vendor_x_v2"]),
        load(&second.sets["vendor_x_v2_alt"]),
    ];
    match FamilySets::new(&family, &both, &catalog) {
        Err(AssembleError::DuplicateVersion {
            version,
            first,
            second,
            ..
        }) => assert_eq!(
            (version.as_str(), first.as_str(), second.as_str()),
            ("2", "vendor_x_v2", "vendor_x_v2_alt")
        ),
        other => panic!("expected a duplicate version, got {other:?}"),
    }

    let twice = [
        load(&first.sets["vendor_x_v2"]),
        load(&first.sets["vendor_x_v2"]),
    ];
    assert!(matches!(
        FamilySets::new(&family, &twice, &catalog),
        Err(AssembleError::Duplicate { what: "set", .. })
    ));
    assert!(matches!(
        FamilySets::new(&family, &[load(&first.sets["vendor_x_v2"])], &[]),
        Err(AssembleError::Missing {
            what: "catalog",
            ..
        })
    ));
    assert!(matches!(
        FamilySets::new(&family, &[load(&first.sets["vendor_j_v1"])], &catalog),
        Err(AssembleError::DigestMismatch { what: "family", .. })
    ));
    assert!(matches!(
        FamilySets::new(&catalog[0], &[], &catalog),
        Err(AssembleError::WrongKind { expected: "family" })
    ));
    // A family alone is fine: it routes nothing until a set is loaded.
    let empty = FamilySets::new(&family, &[], &[]).unwrap();
    let payload = vendor_x_payload(101);
    let message = Message::new(Payload::Binary(&payload), now()).with_metadata(FMT_2);
    assert_eq!(code(empty.route(&message, &registry())), "unknown_version");
}

#[test]
fn a_kept_set_routes_with_the_older_catalog_it_pins() {
    let first = built(&fixture());
    let mut added = fixture();
    added["catalog"]["params"]["zzz.added"] = json!({ "type": "f64" });
    let bundle = read_bundle(&serde_json::to_vec(&added).unwrap())
        .unwrap()
        .value;
    let second = rebuild(&validate(bundle).unwrap(), &first).unwrap();
    let sets = FamilySets::new(
        &load(&second.families["vendor_x"]),
        &sets_of(&second, "vendor_x"),
        &[load(&second.catalog), load(&first.catalog)],
    )
    .unwrap();
    let payload = vendor_x_payload(101);
    let message = Message::new(Payload::Binary(&payload), now()).with_metadata(FMT_2);
    let route = sets.route(&message, &registry()).unwrap();
    assert_eq!(route.def_version, &first.sets["vendor_x_v2"].digest);
}
