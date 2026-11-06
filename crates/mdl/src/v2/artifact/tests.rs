use serde_json::{Value, json};

use super::*;
use crate::v2::diagnostics::Code;
use crate::v2::ir::Bundle;
use crate::v2::read::{read_artifact, read_bundle};
use crate::v2::validate::validate;

const FULL_BUNDLE: &str = include_str!("../../../tests/fixtures/v2/full_bundle.json");

fn built(value: &Value) -> Artifacts {
    let bundle: Bundle = read_bundle(&serde_json::to_vec(value).unwrap())
        .unwrap()
        .value;
    build(&validate(bundle).unwrap()).unwrap()
}

fn fixture() -> Value {
    serde_json::from_str(FULL_BUNDLE).unwrap()
}

#[test]
fn a_bundle_becomes_one_artifact_per_catalog_family_and_set() {
    let artifacts = built(&fixture());
    assert_eq!(
        artifacts.families.keys().collect::<Vec<_>>(),
        ["legacy", "vendor_j", "vendor_x"]
    );
    assert_eq!(
        artifacts.sets.keys().collect::<Vec<_>>(),
        ["legacy_v1", "vendor_j_v1", "vendor_x_v2"]
    );
    let set = &artifacts.sets["vendor_x_v2"];
    let Artifact::Set {
        catalog, family, ..
    } = &set.artifact
    else {
        panic!("expected a set artifact");
    };
    assert_eq!(catalog, &artifacts.catalog.digest);
    assert_eq!(family, &artifacts.families["vendor_x"].digest);
}

#[test]
fn every_artifact_reads_back_with_the_same_identity() {
    let artifacts = built(&fixture());
    let all = std::iter::once(&artifacts.catalog)
        .chain(artifacts.families.values())
        .chain(artifacts.sets.values());
    for written in all {
        let loaded = read_artifact(written.canonical.as_bytes()).unwrap();
        assert_eq!(loaded.digest, written.digest);
        assert_eq!(loaded.canonical, written.canonical);
        assert_eq!(loaded.value, written.artifact);
    }
}

#[test]
fn identity_depends_on_content_not_on_bundle_order() {
    let first = built(&fixture());
    let mut reordered = fixture();
    reordered["families"].as_array_mut().unwrap().reverse();
    reordered["sets"].as_array_mut().unwrap().reverse();
    assert_eq!(built(&reordered), first);

    // A change to one set changes that set's identity only.
    let mut edited = fixture();
    edited["sets"][1]["packets"][0]["entries"][0]["conversion"]["scale"] = json!(0.01);
    let after = built(&edited);
    assert_ne!(
        after.sets["vendor_j_v1"].digest,
        first.sets["vendor_j_v1"].digest
    );
    assert_eq!(
        after.sets["vendor_x_v2"].digest,
        first.sets["vendor_x_v2"].digest
    );
    assert_eq!(after.catalog.digest, first.catalog.digest);
}

/// Pinned identities of a small bundle. A change here means the canonical
/// bytes of artifacts changed: every deployed def_version would change with
/// it, so update these only on purpose. Cross-checked with Node's
/// `crypto.createHash("sha256")` over the canonical text.
#[test]
fn small_bundle_digests_are_stable() {
    let artifacts = built(&json!({
        "mdl_version": "2.0",
        "catalog": { "params": { "eps.bus_voltage": { "type": "f64", "unit": "V" } } },
        "families": [{ "name": "box", "encoding": "binary", "byte_order": "big",
                       "alias": { "kind": "fixed", "value": "box-1" } }],
        "sets": [{ "name": "box_v1", "family": "box", "packets": [{
            "name": "HK", "length": { "kind": "exact", "bytes": 2 },
            "entries": [{ "kind": "field", "name": "bus_mv", "type": "uint", "size_bits": 16,
                          "conversion": { "kind": "linear", "scale": 0.001, "offset": 0.0 },
                          "unit": "V", "canonical": "eps.bus_voltage" }] }] }]
    }));
    assert_eq!(
        artifacts.catalog.digest.as_str(),
        "c60ec942a7cbe27fb25d7498d65b89b0c48a10d53089fa8e335b4f6d12d9cfec"
    );
    assert_eq!(
        artifacts.families["box"].digest.as_str(),
        "937926978110704a2a5078ba9cb6fac18f75d9a7f0b400f8bde311c59195affd"
    );
    assert_eq!(
        artifacts.sets["box_v1"].digest.as_str(),
        "c5245c92019c88a52c124570a185a1a5606c8c683211e5cb88e0dce436d2a808"
    );
}

// ---- Catalog compatibility ----

fn catalog(params: Value) -> Catalog {
    serde_json::from_value(json!({ "params": params })).unwrap()
}

fn incompatibilities(old: Value, new: Value) -> Vec<(Code, String)> {
    match check_compatible(&catalog(old), &catalog(new)) {
        Ok(()) => Vec::new(),
        Err(diagnostics) => diagnostics
            .iter()
            .map(|d| (d.code, d.path.clone()))
            .collect(),
    }
}

#[test]
fn compatible_changes_are_accepted() {
    let old = json!({ "eps.mode": { "type": "enum", "states": ["SAFE", "NOMINAL"] },
                      "eps.v": { "type": "f64", "unit": "V", "limits": { "red_high": 9.0 } } });
    let new = json!({
        "eps.mode": { "type": "enum", "states": ["SAFE", "NOMINAL", "SCIENCE"],
                      "description": "Power mode" },
        "eps.v": { "type": "f64", "unit": "V", "limits": { "red_high": 8.5 },
                   "deprecated": "use eps.bus_voltage" },
        "eps.bus_voltage": { "type": "f64", "unit": "V" }
    });
    assert_eq!(incompatibilities(old, new), vec![]);
}

#[test]
fn breaking_changes_are_refused() {
    let old = json!({
        "a.gone": { "type": "f64" },
        "a.mode": { "type": "enum", "states": ["SAFE", "NOMINAL"] },
        "a.pos": { "type": "vec3", "unit": "m", "frame": "ECEF" },
        "a.v": { "type": "f64", "unit": "V" }
    });
    let new = json!({
        "a.mode": { "type": "enum", "states": ["NOMINAL", "SAFE"] },
        "a.pos": { "type": "vec3", "unit": "km", "frame": "ECI_J2000" },
        "a.v": { "type": "i64", "unit": "V" }
    });
    assert_eq!(
        incompatibilities(old, new),
        vec![
            (Code::RemovedParam, "/catalog/params/a.gone".to_string()),
            (
                Code::ChangedParam,
                "/catalog/params/a.mode/states".to_string()
            ),
            (
                Code::ChangedParam,
                "/catalog/params/a.pos/frame".to_string()
            ),
            (Code::ChangedParam, "/catalog/params/a.pos/unit".to_string()),
            (Code::ChangedParam, "/catalog/params/a.v/type".to_string()),
        ]
    );
}

#[test]
fn packet_order_doesnt_change_a_set_identity() {
    let packet = |name: &str, apid: i64| {
        json!({ "name": name, "identify": { "apid": apid }, "entries": [
            { "kind": "field", "name": "x", "type": "uint", "size_bits": 8 }] })
    };
    let with_packets = |packets: Value| {
        let mut value = fixture();
        value["sets"][0]["packets"] = packets;
        value["sets"][0].as_object_mut().unwrap().remove("time");
        built(&value).sets["vendor_x_v2"].digest.clone()
    };
    assert_eq!(
        with_packets(json!([packet("A", 1), packet("B", 2)])),
        with_packets(json!([packet("B", 2), packet("A", 1)]))
    );
}

#[test]
fn set_artifacts_list_packets_in_name_order() {
    let artifacts = built(&fixture());
    let mut value: Value = serde_json::from_str(&artifacts.sets["vendor_x_v2"].canonical).unwrap();
    let packet = value["set"]["packets"][0].clone();
    let mut later = packet.clone();
    later["name"] = json!("AA");
    value["set"]["packets"] = json!([packet, later]);
    let bytes = serde_json::to_vec(&value).unwrap();
    let error = read_artifact(&bytes).unwrap_err().to_string();
    assert!(
        error.starts_with("/set/packets/1: packets are listed in name order"),
        "{error}"
    );
}

#[test]
fn a_compatible_catalog_change_keeps_set_identities() {
    let first = built(&fixture());
    let mut added = fixture();
    added["catalog"]["params"]["zzz.unrelated"] = json!({ "type": "f64" });
    let bundle: Bundle = read_bundle(&serde_json::to_vec(&added).unwrap())
        .unwrap()
        .value;
    let next = rebuild(&validate(bundle).unwrap(), &first).unwrap();
    assert_ne!(next.catalog.digest, first.catalog.digest);
    for (name, written) in &next.sets {
        assert_eq!(written.digest, first.sets[name].digest, "{name}");
    }

    // A changed set gets a new artifact, pinned to the new catalog.
    let mut changed = added.clone();
    changed["sets"][1]["packets"][0]["entries"][0]["conversion"]["scale"] = json!(0.01);
    let bundle: Bundle = read_bundle(&serde_json::to_vec(&changed).unwrap())
        .unwrap()
        .value;
    let after = rebuild(&validate(bundle).unwrap(), &first).unwrap();
    assert_ne!(
        after.sets["vendor_j_v1"].digest,
        first.sets["vendor_j_v1"].digest
    );
    let Artifact::Set { catalog, .. } = &after.sets["vendor_j_v1"].artifact else {
        panic!("expected a set");
    };
    assert_eq!(catalog, &after.catalog.digest);
    assert_eq!(
        after.sets["vendor_x_v2"].digest,
        first.sets["vendor_x_v2"].digest
    );

    // An incompatible catalog is refused.
    let mut broken = fixture();
    broken["catalog"]["params"]["eps.bus_voltage"]["unit"] = json!("mV");
    broken["sets"][0]["packets"][0]["entries"][0]["unit"] = json!("mV");
    broken["sets"][1]["packets"][0]["entries"][0]["unit"] = json!("mV");
    let bundle: Bundle = read_bundle(&serde_json::to_vec(&broken).unwrap())
        .unwrap()
        .value;
    assert!(matches!(
        rebuild(&validate(bundle).unwrap(), &first),
        Err(BuildError::IncompatibleCatalog(_))
    ));
}

#[test]
fn consumers_assemble_only_matching_valid_artifacts() {
    let artifacts = built(&fixture());
    let set = load(&artifacts.sets["vendor_x_v2"]);
    let family = load(&artifacts.families["vendor_x"]);
    let catalog = load(&artifacts.catalog);
    let validated = assemble(&set, &family, &catalog).unwrap();
    assert_eq!(validated.bundle().sets[0].name, "vendor_x_v2");

    let other_family = load(&artifacts.families["vendor_j"]);
    assert!(matches!(
        assemble(&set, &other_family, &catalog),
        Err(AssembleError::DigestMismatch { what: "family", .. })
    ));
    assert!(matches!(
        assemble(&catalog, &family, &catalog),
        Err(AssembleError::WrongKind { expected: "set" })
    ));
}

fn rebuilt(value: &Value, previous: &Artifacts) -> Result<Artifacts, BuildError> {
    let bundle: Bundle = read_bundle(&serde_json::to_vec(value).unwrap())
        .unwrap()
        .value;
    rebuild(&validate(bundle).unwrap(), previous)
}

fn load(written: &Written) -> Loaded<Artifact> {
    read_artifact(written.canonical.as_bytes()).unwrap()
}

/// `artifacts` written to storage and read back, with the older catalogs
/// its sets pin.
fn stored(artifacts: &Artifacts, older: &[&Written]) -> Result<Artifacts, AssembleError> {
    let others = artifacts
        .families
        .values()
        .chain(artifacts.sets.values())
        .chain(older.iter().copied())
        .map(load);
    Artifacts::from_loaded(load(&artifacts.catalog), others)
}

fn pinned_catalog(written: &Written) -> &Digest {
    let Artifact::Set { catalog, .. } = &written.artifact else {
        panic!("expected a set");
    };
    catalog
}

#[test]
fn kept_sets_survive_chained_rebuilds_through_storage() {
    let first = built(&fixture());
    let mut second_value = fixture();
    second_value["catalog"]["params"]["zzz.second"] = json!({ "type": "f64" });
    let second = rebuilt(&second_value, &first).unwrap();

    // Kept sets still pin the first catalog, so storage must have it.
    assert!(matches!(
        stored(&second, &[]),
        Err(AssembleError::Missing {
            what: "catalog",
            ..
        })
    ));
    let second_loaded = stored(&second, &[&first.catalog]).unwrap();
    assert_eq!(second_loaded, second);

    let mut third_value = second_value.clone();
    third_value["catalog"]["params"]["zzz.third"] = json!({ "type": "f64" });
    third_value["sets"][1]["packets"][0]["entries"][0]["conversion"]["scale"] = json!(0.01);
    let third = rebuilt(&third_value, &second_loaded).unwrap();
    let kept = &third.sets["vendor_x_v2"];
    assert_eq!(kept.digest, first.sets["vendor_x_v2"].digest);
    assert_eq!(pinned_catalog(kept), &first.catalog.digest);
    let changed = &third.sets["vendor_j_v1"];
    assert_ne!(changed.digest, first.sets["vendor_j_v1"].digest);
    assert_eq!(pinned_catalog(changed), &third.catalog.digest);
    assert_eq!(stored(&third, &[&first.catalog]).unwrap(), third);
}

#[test]
fn stored_artifacts_must_come_from_one_history() {
    let first = built(&fixture());
    let sets = || first.sets.values().map(load);
    let families = || first.families.values().map(load);

    assert!(matches!(
        Artifacts::from_loaded(load(&first.sets["vendor_x_v2"]), families()),
        Err(AssembleError::WrongKind {
            expected: "catalog"
        })
    ));
    assert!(matches!(
        Artifacts::from_loaded(load(&first.catalog), sets()),
        Err(AssembleError::Missing { what: "family", .. })
    ));
    assert!(matches!(
        Artifacts::from_loaded(load(&first.catalog), families().chain(sets()).chain(sets())),
        Err(AssembleError::Duplicate { what: "set", .. })
    ));

    // The same family name with other content is another family.
    let mut other_family = fixture();
    other_family["families"][0]["metadata"] = json!(["apid", "fmt", "zone"]);
    let other = built(&other_family);
    assert!(matches!(
        Artifacts::from_loaded(
            load(&first.catalog),
            other.families.values().map(load).chain(sets())
        ),
        Err(AssembleError::DigestMismatch { what: "family", .. })
    ));

    // Sets pinning a catalog the current one can't replace.
    let mut with_old_param = fixture();
    with_old_param["catalog"]["params"]["zzz.old"] = json!({ "type": "f64" });
    let before = built(&with_old_param);
    let others = before
        .families
        .values()
        .chain(before.sets.values())
        .chain([&before.catalog])
        .map(load);
    match Artifacts::from_loaded(load(&first.catalog), others) {
        Err(AssembleError::IncompatibleCatalog { problems, .. }) => {
            assert_eq!(
                problems.iter().next().map(|d| d.code),
                Some(Code::RemovedParam)
            );
        }
        other => panic!("expected an incompatible catalog, got {other:?}"),
    }
}

#[test]
fn assemble_validates_artifacts_whose_pins_match() {
    let artifacts = built(&fixture());
    let mut value: Value = serde_json::from_str(&artifacts.sets["vendor_x_v2"].canonical).unwrap();
    // flags moved onto bus_counts; the pins still match.
    value["set"]["packets"][0]["entries"][3]["offset_bits"] = json!(64);
    let set = read_artifact(&serde_json::to_vec(&value).unwrap()).unwrap();
    match assemble(
        &set,
        &load(&artifacts.families["vendor_x"]),
        &load(&artifacts.catalog),
    ) {
        Err(AssembleError::Invalid(problems)) => {
            assert!(
                problems.iter().any(|d| d.code == Code::Overlap),
                "{problems}"
            );
        }
        other => panic!("expected invalid artifacts, got {other:?}"),
    }
}

fn command_incompatibilities(old: Value, new: Value) -> Vec<(Code, String)> {
    let catalog = |commands: Value| -> Catalog {
        serde_json::from_value(json!({ "commands": commands })).unwrap()
    };
    match check_compatible(&catalog(old), &catalog(new)) {
        Ok(()) => Vec::new(),
        Err(diagnostics) => diagnostics
            .iter()
            .map(|d| (d.code, d.path.clone()))
            .collect(),
    }
}

#[test]
fn commands_evolve_like_params() {
    let old = json!({ "eps.go": { "arguments": [
        { "name": "mode", "type": "enum", "states": ["A"] },
        { "name": "n", "type": "u64", "default": 1, "range": { "max": 9 } } ] } });
    // Appended states, a new argument with a default, a description and a
    // new command keep callers written for the older catalog valid.
    let compatible = json!({
        "eps.go": { "description": "Go", "arguments": [
            { "name": "mode", "type": "enum", "states": ["A", "B"] },
            { "name": "n", "type": "u64", "default": 1, "range": { "max": 99 } },
            { "name": "fast", "type": "bool", "default": false } ] },
        "eps.stop": {} });
    assert_eq!(command_incompatibilities(old.clone(), compatible), vec![]);

    let go = "/catalog/commands/eps.go";
    let path = |rest: &str| format!("{go}{rest}");
    let breaking = |change: &dyn Fn(&mut Value)| {
        let mut new = old.clone();
        change(&mut new);
        command_incompatibilities(old.clone(), new)
    };
    assert_eq!(
        breaking(&|new| *new = json!({})),
        vec![(Code::RemovedCommand, go.to_string())]
    );
    assert_eq!(
        breaking(&|new| new["eps.go"]["arguments"][1]["type"] = json!("i64")),
        vec![(Code::ChangedCommand, path("/arguments/1/type"))]
    );
    assert_eq!(
        breaking(&|new| new["eps.go"]["arguments"][1]["default"] = json!(2)),
        vec![(Code::ChangedCommand, path("/arguments/1/default"))]
    );
    // A narrower range refuses values callers could send.
    assert_eq!(
        breaking(&|new| new["eps.go"]["arguments"][1]["range"]["max"] = json!(5)),
        vec![(Code::ChangedCommand, path("/arguments/1/range"))]
    );
    assert_eq!(
        breaking(&|new| new["eps.go"]["arguments"][1]["range"]["min"] = json!(1)),
        vec![(Code::ChangedCommand, path("/arguments/1/range"))]
    );
    assert_eq!(
        breaking(&|new| new["eps.go"]["arguments"][0]["states"] = json!(["B", "A"])),
        vec![(Code::ChangedCommand, path("/arguments/0/states"))]
    );
    assert_eq!(
        breaking(&|new| {
            new["eps.go"]["arguments"]
                .as_array_mut()
                .unwrap()
                .push(json!({ "name": "m", "type": "bool" }));
        }),
        vec![(Code::ChangedCommand, path("/arguments/2"))]
    );
    assert_eq!(
        breaking(&|new| {
            new["eps.go"]["arguments"].as_array_mut().unwrap().remove(0);
        }),
        vec![(Code::ChangedCommand, path("/arguments"))]
    );
}

#[test]
fn set_artifacts_list_commands_in_name_order() {
    let artifacts = built(&fixture());
    let value: Value = serde_json::from_str(&artifacts.sets["vendor_x_v2"].canonical).unwrap();
    let names: Vec<&str> = value["set"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|command| command["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["LOAD_TABLE", "SET_MODE"]);

    // Any other order is a second spelling, so it's refused.
    let mut reordered = value.clone();
    reordered["set"]["commands"]
        .as_array_mut()
        .unwrap()
        .reverse();
    let error = read_artifact(&serde_json::to_vec(&reordered).unwrap())
        .unwrap_err()
        .to_string();
    assert!(
        error.starts_with("/set/commands/1: commands are listed in name order"),
        "{error}"
    );
    // A bundle in another order builds the same set artifact.
    let mut shuffled = fixture();
    shuffled["sets"][0]["commands"]
        .as_array_mut()
        .unwrap()
        .reverse();
    assert_eq!(
        built(&shuffled).sets["vendor_x_v2"].digest,
        artifacts.sets["vendor_x_v2"].digest
    );
}
