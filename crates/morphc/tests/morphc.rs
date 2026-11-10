//! morphc end to end: the real binary run the way a user, a CI job or a
//! frontend in another language (piping a bundle) would, checking exit
//! codes, output and the store it writes.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use mdl::ir::Digest;
use serde_json::Value;

const FULL_BUNDLE: &str = include_str!("../../mdl/tests/fixtures/full_bundle.json");

/// The paired fixtures: a TOML catalog and an XTCE satellite whose bus
/// voltage maps to the catalog's name.
fn paired() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/paired")
}

/// An empty directory for one test, under Cargo's scratch directory.
fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// The paired fixtures copied into a test's scratch directory, to edit.
fn paired_copy(test: &str) -> PathBuf {
    let dir = scratch(test);
    let defs = dir.join("defs");
    fs::create_dir(&defs).unwrap();
    for file in ["catalog.toml", "sat.xml"] {
        fs::copy(paired().join(file), defs.join(file)).unwrap();
    }
    dir
}

fn edit(path: &Path, from: &str, to: &str) {
    let text = fs::read_to_string(path).unwrap();
    assert!(text.contains(from), "{from:?} isn't in {}", path.display());
    fs::write(path, text.replace(from, to)).unwrap();
}

fn morphc_in(dir: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_morphc"))
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("morphc runs");
    let mut input = child.stdin.take().expect("stdin is piped");
    if let Some(text) = stdin {
        input.write_all(text.as_bytes()).expect("write stdin");
    }
    drop(input);
    child.wait_with_output().expect("morphc finishes")
}

fn morphc(args: &[&str]) -> Output {
    morphc_in(&paired(), args, None)
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The artifact lines of a build's stdout, without digests: `set sat
/// unchanged (written)`.
fn artifacts(out: &Output) -> Vec<String> {
    stdout(out)
        .lines()
        .filter(|line| !line.starts_with("ok: "))
        .map(|line| {
            line.split_whitespace()
                .filter(|word| word.len() != 64)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

fn manifest(store: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(store.join("manifest.json")).unwrap()).unwrap()
}

#[test]
fn a_catalog_and_an_xtce_import_build_together() {
    let out = morphc(&["build", "catalog.toml", "sat.xml"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert_eq!(
        artifacts(&out),
        ["catalog", "family sat", "set sat"],
        "{}",
        stdout(&out)
    );
    assert!(
        stdout(&out).ends_with("ok: 4 params, 0 commands, 1 family, 1 set (MDL IR 2.0)\n"),
        "{}",
        stdout(&out)
    );
    assert!(
        err.contains(
            "warning[overridden]: param eps.bus_voltage has description \"Bus voltage\" here, \
             but the earlier definition's \"Main power bus voltage\" takes precedence\n  \
             --> sat.xml:31:79"
        ),
        "{err}"
    );
    assert!(
        err.contains("= the first is at catalog.toml:10:15"),
        "{err}"
    );
    assert!(
        err.ends_with("morphc: 4 warnings (3 xtce.ignored, 1 overridden)\n"),
        "{err}"
    );
}

#[test]
fn a_warning_code_is_shown_once_unless_all_are_asked_for() {
    let grouped = stderr(&morphc(&["build", "catalog.toml", "sat.xml"]));
    assert_eq!(grouped.matches("warning[xtce.ignored]").count(), 1);
    assert!(
        grouped.contains("= and 2 more xtce.ignored warnings (--all-warnings shows them)"),
        "{grouped}"
    );
    let all = stderr(&morphc(&[
        "build",
        "catalog.toml",
        "sat.xml",
        "--all-warnings",
    ]));
    assert_eq!(all.matches("warning[xtce.ignored]").count(), 3, "{all}");
    assert!(!all.contains("more xtce.ignored"), "{all}");
}

#[test]
fn input_order_is_precedence() {
    let dir = morphc_in(&paired().join(".."), &["build", "paired"], None);
    assert_eq!(dir.status.code(), Some(0), "{}", stderr(&dir));
    let files = morphc(&["build", "catalog.toml", "sat.xml"]);
    assert_eq!(stdout(&dir), stdout(&files));

    let reversed = morphc(&["build", "sat.xml", "catalog.toml"]);
    assert_eq!(reversed.status.code(), Some(0), "{}", stderr(&reversed));
    assert!(
        stderr(&reversed).contains(
            "param eps.bus_voltage has description \"Main power bus voltage\" here, but the \
             earlier definition's \"Bus voltage\" takes precedence"
        ),
        "{}",
        stderr(&reversed)
    );
    // The catalog's text is in its own artifact, so the identity differs.
    assert_ne!(stdout(&reversed), stdout(&files));
}

#[test]
fn a_disagreement_between_frontends_is_an_error_in_both_files() {
    let dir = paired_copy("disagreement");
    edit(
        &dir.join("defs/catalog.toml"),
        "unit        = \"V\"",
        "unit        = \"mV\"",
    );
    let out = morphc_in(&dir, &["build", "defs"], None);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(
        err.contains(
            "error[duplicate_name]: param eps.bus_voltage is defined again with unit V; the \
             earlier definition has mV"
        ),
        "{err}"
    );
    assert!(err.contains("--> defs/sat.xml:"), "{err}");
    assert!(
        err.contains("= the first is at defs/catalog.toml:8:15"),
        "{err}"
    );
    assert!(
        err.ends_with("morphc: 1 error, 3 warnings (3 xtce.ignored)\n"),
        "{err}"
    );
    assert_eq!(stdout(&out), "");
}

#[test]
fn json_output_has_every_diagnostic_and_the_artifacts() {
    let out = morphc(&["build", "catalog.toml", "sat.xml", "--format", "json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stderr(&out), "");
    let report: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(report["ok"], true);
    let diagnostics = report["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 4);
    let overridden = &diagnostics[3];
    assert_eq!(overridden["code"], "overridden");
    assert_eq!(
        overridden["path"],
        "/catalog/params/eps.bus_voltage/description"
    );
    assert_eq!(overridden["location"]["file"], "sat.xml");
    assert_eq!(overridden["location"]["line"], 31);
    assert_eq!(overridden["first"]["file"], "catalog.toml");
    let build = &report["build"];
    assert_eq!(build["sets"], 1);
    let catalog = &build["artifacts"][0];
    assert_eq!(catalog["kind"], "catalog");
    assert!(
        catalog["digest"]
            .as_str()
            .unwrap()
            .parse::<Digest>()
            .is_ok()
    );
    assert_eq!(catalog["status"], Value::Null);
}

#[test]
fn a_store_holds_each_artifact_under_its_digest_and_a_manifest() {
    let dir = scratch("store");
    let store = dir.join("store");
    let store_arg = store.to_str().unwrap();
    let out = morphc_in(&paired(), &["build", ".", "--out", store_arg], None);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        artifacts(&out),
        [
            "catalog (written)",
            "family sat (written)",
            "set sat (written)"
        ]
    );
    let manifest = manifest(&store);
    assert_eq!(manifest["manifest"], 1);
    let mut names = Vec::new();
    for entry in fs::read_dir(&store).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_string();
        if name == "manifest.json" {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(
            format!("{}.json", Digest::of_canonical(&text)),
            name,
            "a file's name is the digest of its bytes"
        );
        names.push(name);
    }
    names.sort();
    let mut named = vec![
        manifest["catalog"].as_str().unwrap().to_string(),
        manifest["families"]["sat"].as_str().unwrap().to_string(),
        manifest["sets"]["sat"].as_str().unwrap().to_string(),
    ];
    named.iter_mut().for_each(|digest| digest.push_str(".json"));
    named.sort();
    assert_eq!(names, named);

    // The same build again writes nothing and changes nothing.
    let again = morphc_in(&paired(), &["build", ".", "--out", store_arg], None);
    assert_eq!(
        artifacts(&again),
        [
            "catalog unchanged",
            "family sat unchanged",
            "set sat unchanged"
        ]
    );
    let summary = format!("; 0 new artifacts written to {store_arg}\n");
    assert!(stdout(&again).ends_with(&summary), "{}", stdout(&again));
}

#[test]
fn a_rebuild_keeps_unchanged_sets_and_refuses_a_breaking_catalog() {
    let dir = paired_copy("rebuild");
    let catalog = dir.join("defs/catalog.toml");
    let build = |extra: &[&str]| {
        let mut args = vec!["build", "defs", "--out", "store"];
        args.extend_from_slice(extra);
        morphc_in(&dir, &args, None)
    };
    let first = build(&[]);
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    let before = manifest(&dir.join("store"));

    edit(
        &catalog,
        "Main power bus voltage",
        "Main power bus voltage, 28 V nominal",
    );
    let compatible = build(&[]);
    assert_eq!(compatible.status.code(), Some(0), "{}", stderr(&compatible));
    assert_eq!(
        artifacts(&compatible),
        [
            "catalog changed (written)",
            "family sat unchanged",
            "set sat unchanged"
        ]
    );
    let after = manifest(&dir.join("store"));
    assert_eq!(after["sets"], before["sets"]);
    assert_ne!(after["catalog"], before["catalog"]);
    // The set still pins the first catalog, which the store keeps.
    let first_catalog = format!("{}.json", before["catalog"].as_str().unwrap());
    assert!(dir.join("store").join(first_catalog).is_file());

    // Removing a param breaks the catalog: sets can't keep their identity.
    edit(
        &catalog,
        "[catalog.params.\"eps.batt_temp\"]\ntype        = \"f64\"\nunit        = \"Cel\"\n\
         description = \"Battery temperature\"\n",
        "",
    );
    let breaking = build(&[]);
    let err = stderr(&breaking);
    assert_eq!(breaking.status.code(), Some(1), "{err}");
    assert!(err.contains("error[removed_param]"), "{err}");
    assert!(
        err.contains("note: the catalog isn't compatible with the one in store"),
        "{err}"
    );
    assert_eq!(
        manifest(&dir.join("store")),
        after,
        "a failed build leaves the store"
    );

    let fresh = build(&["--fresh"]);
    assert_eq!(fresh.status.code(), Some(0), "{}", stderr(&fresh));
    assert_eq!(
        artifacts(&fresh),
        [
            "catalog changed (written)",
            "family sat unchanged",
            "set sat changed (written)"
        ]
    );
}

#[test]
fn a_rebuild_into_a_new_store_brings_the_catalogs_kept_sets_pin() {
    let dir = paired_copy("previous");
    let first = morphc_in(&dir, &["build", "defs", "--out", "old"], None);
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    edit(
        &dir.join("defs/catalog.toml"),
        "Battery temperature",
        "Battery pack temperature",
    );
    let out = morphc_in(
        &dir,
        &["build", "defs", "--previous", "old", "--out", "new"],
        None,
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        artifacts(&out),
        [
            "catalog changed (written)",
            "family sat unchanged (written)",
            "set sat unchanged (written)"
        ]
    );
    let old = manifest(&dir.join("old"));
    let pinned = format!("{}.json", old["catalog"].as_str().unwrap());
    assert!(dir.join("new").join(pinned).is_file());
    assert!(
        stdout(&out).ends_with("; 4 new artifacts written to new\n"),
        "the older catalog is counted: {}",
        stdout(&out)
    );
    // The new store reads back on its own.
    let again = morphc_in(&dir, &["build", "defs", "--out", "new"], None);
    assert_eq!(again.status.code(), Some(0), "{}", stderr(&again));
    assert!(
        artifacts(&again)
            .iter()
            .all(|row| row.ends_with("unchanged"))
    );
}

#[test]
fn a_fresh_build_shows_what_the_store_no_longer_names() {
    let dir = paired_copy("removed");
    let first = morphc_in(&dir, &["build", "defs", "--out", "store"], None);
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    let out = morphc_in(
        &dir,
        &["build", "defs/catalog.toml", "--out", "store", "--fresh"],
        None,
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        artifacts(&out),
        [
            "catalog changed (written)",
            "family sat removed",
            "set sat removed"
        ]
    );
}

#[test]
fn a_damaged_store_stops_the_build() {
    let dir = paired_copy("damaged");
    let first = morphc_in(&dir, &["build", "defs", "--out", "store"], None);
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    let manifest = manifest(&dir.join("store"));
    let set = dir.join("store").join(format!(
        "{}.json",
        manifest["sets"]["sat"].as_str().unwrap()
    ));
    let catalog = dir
        .join("store")
        .join(format!("{}.json", manifest["catalog"].as_str().unwrap()));
    fs::copy(&set, &catalog).unwrap();
    let out = morphc_in(&dir, &["build", "defs", "--out", "store"], None);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("not the one its name says"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_store_beside_the_definitions_isnt_read_as_definitions() {
    let dir = paired_copy("store_inside");
    for _ in 0..2 {
        let out = morphc_in(&dir, &["build", "defs", "--out", "defs/store"], None);
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    }
    let out = morphc_in(&dir, &["build", "defs/store"], None);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("defs/store is an artifact store, not definitions"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_bundle_on_stdin_builds() {
    let out = morphc_in(&paired(), &["build", "-"], Some(FULL_BUNDLE));
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stdout(&out).contains("ok: "), "{}", stdout(&out));
}

#[test]
fn what_morphc_cant_run_exits_2() {
    let cases: [(&[&str], &str); 6] = [
        (&["build", "README.md"], "isn't a definition file"),
        (&["build", "-", "-"], "`-` (stdin) is given twice"),
        (
            &["build", "sat.xml", "--import-alias", "bits:x:8"],
            "isn't a locator",
        ),
        (&["build", "nowhere"], "can't read nowhere"),
        (
            &["build", "catalog.toml", "--import-family", "x"],
            "the XTCE options apply to .xml inputs",
        ),
        (
            &["build", "sat.xml", "--import-alias", "serial"],
            "isn't a locator",
        ),
    ];
    let dir = scratch("cant_run");
    fs::write(dir.join("README.md"), "# not definitions\n").unwrap();
    fs::copy(paired().join("catalog.toml"), dir.join("catalog.toml")).unwrap();
    fs::copy(paired().join("sat.xml"), dir.join("sat.xml")).unwrap();
    for (args, message) in cases {
        let out = morphc_in(&dir, args, None);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(stderr(&out).contains(message), "{args:?}: {}", stderr(&out));
    }
    let usage = morphc_in(&dir, &["build", "catalog.toml", "--fresh"], None);
    assert_eq!(usage.status.code(), Some(2), "--fresh needs --out");
}

#[test]
fn a_directory_puts_hand_written_definitions_before_imports() {
    let dir = scratch("directory_order");
    let defs = dir.join("defs");
    fs::create_dir(&defs).unwrap();
    fs::copy(paired().join("sat.xml"), defs.join("a_sat.xml")).unwrap();
    fs::copy(paired().join("catalog.toml"), defs.join("z_catalog.toml")).unwrap();
    let out = morphc_in(&dir, &["build", "defs"], None);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stderr(&out)
            .contains("the earlier definition's \"Main power bus voltage\" takes precedence"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn files_are_found_by_extension_in_any_case_and_read_once() {
    let dir = scratch("extensions");
    let defs = dir.join("defs");
    fs::create_dir(&defs).unwrap();
    fs::copy(paired().join("catalog.toml"), defs.join("catalog.toml")).unwrap();
    fs::copy(paired().join("sat.xml"), defs.join("SAT.XML")).unwrap();
    let out = morphc_in(
        &dir,
        &["build", "defs", "defs/catalog.toml", "defs/SAT.XML"],
        None,
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(artifacts(&out), ["catalog", "family sat", "set sat"]);
}

#[test]
fn stores_inside_the_definitions_are_left_out_and_other_manifests_arent_stores() {
    let dir = paired_copy("store_detection");
    let first = morphc_in(&dir, &["build", "defs", "--out", "store"], None);
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    // A store whose first build stopped before its manifest.
    let unfinished = dir.join("defs/unfinished");
    fs::create_dir(&unfinished).unwrap();
    let catalog = format!(
        "{}.json",
        manifest(&dir.join("store"))["catalog"].as_str().unwrap()
    );
    fs::copy(dir.join("store").join(&catalog), unfinished.join(&catalog)).unwrap();
    let out = morphc_in(&dir, &["build", "defs"], None);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));

    // Another tool's manifest doesn't make a store: its directory is read,
    // and the manifest with it, which isn't a bundle.
    let web = dir.join("defs/web");
    fs::create_dir(&web).unwrap();
    fs::write(web.join("manifest.json"), "{ \"name\": \"console\" }\n").unwrap();
    let out = morphc_in(&dir, &["build", "defs"], None);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("--> defs/web/manifest.json:1:1"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn an_out_that_is_an_input_directory_is_refused() {
    let dir = paired_copy("out_is_input");
    let out = morphc_in(&dir, &["build", "defs", "--out", "defs"], None);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("is an input directory"),
        "{}",
        stderr(&out)
    );
    assert!(!dir.join("defs/manifest.json").exists());
}

#[test]
fn fresh_replaces_a_damaged_manifest_but_not_one_of_another_version() {
    let dir = paired_copy("fresh_manifest");
    let first = morphc_in(&dir, &["build", "defs", "--out", "store"], None);
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    let path = dir.join("store/manifest.json");
    let good = fs::read_to_string(&path).unwrap();

    fs::write(&path, "{").unwrap();
    let rebuild = morphc_in(&dir, &["build", "defs", "--out", "store"], None);
    assert_eq!(rebuild.status.code(), Some(2));
    assert!(
        stderr(&rebuild).contains("(--fresh builds without it)"),
        "{}",
        stderr(&rebuild)
    );
    let fresh = morphc_in(&dir, &["build", "defs", "--out", "store", "--fresh"], None);
    assert_eq!(fresh.status.code(), Some(0), "{}", stderr(&fresh));
    assert!(stderr(&fresh).contains("isn't JSON"), "{}", stderr(&fresh));
    assert!(
        stderr(&fresh).contains("this build replaces it"),
        "{}",
        stderr(&fresh)
    );
    assert_eq!(artifacts(&fresh), ["catalog", "family sat", "set sat"]);
    assert_eq!(fs::read_to_string(&path).unwrap(), good);

    let newer = good.replace("\"manifest\": 1", "\"manifest\": 2");
    fs::write(&path, &newer).unwrap();
    let refused = morphc_in(&dir, &["build", "defs", "--out", "store", "--fresh"], None);
    assert_eq!(refused.status.code(), Some(2));
    assert!(
        stderr(&refused).contains("manifest version 2"),
        "{}",
        stderr(&refused)
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), newer);

    let unknown = good.replace("\"manifest\": 1", "\"manifest\": 1,\n  \"owner\": \"ops\"");
    fs::write(&path, unknown).unwrap();
    let rebuild = morphc_in(&dir, &["build", "defs", "--out", "store"], None);
    assert_eq!(rebuild.status.code(), Some(2));
    assert!(
        stderr(&rebuild).contains("unknown field `owner`"),
        "{}",
        stderr(&rebuild)
    );
}

#[test]
fn a_build_continuing_another_store_is_shown_against_the_manifest_it_replaces() {
    let dir = paired_copy("lineage");
    let catalog = dir.join("defs/catalog.toml");
    let build = |args: &[&str]| morphc_in(&dir, args, None);
    for store in ["p", "o"] {
        let out = build(&["build", "defs", "--out", store]);
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    }
    edit(&catalog, "Battery temperature", "Battery pack temperature");
    let later = build(&["build", "defs", "--out", "o"]);
    assert_eq!(later.status.code(), Some(0), "{}", stderr(&later));
    edit(&catalog, "Battery pack temperature", "Battery temperature");

    let out = build(&["build", "defs", "--previous", "p", "--out", "o"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        artifacts(&out),
        [
            "catalog changed",
            "family sat unchanged",
            "set sat unchanged"
        ]
    );
    assert!(
        stderr(&out).contains(
            "the manifest in o names another build; this one, continuing from p, replaces it"
        ),
        "{}",
        stderr(&out)
    );
}

#[test]
fn an_artifact_in_another_formatting_with_its_digest_is_kept() {
    let dir = paired_copy("formatting");
    let first = morphc_in(&dir, &["build", "defs", "--out", "store"], None);
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    let set = dir.join("store").join(format!(
        "{}.json",
        manifest(&dir.join("store"))["sets"]["sat"]
            .as_str()
            .unwrap()
    ));
    let value: Value = serde_json::from_str(&fs::read_to_string(&set).unwrap()).unwrap();
    let pretty = serde_json::to_string_pretty(&value).unwrap();
    fs::write(&set, &pretty).unwrap();
    let out = morphc_in(&dir, &["build", "defs", "--out", "store", "--fresh"], None);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(fs::read_to_string(&set).unwrap(), pretty);
}

#[test]
fn json_output_reports_problems_and_failures_too() {
    let dir = paired_copy("json_failures");
    edit(
        &dir.join("defs/catalog.toml"),
        "unit        = \"V\"",
        "unit        = \"mV\"",
    );
    let problems = morphc_in(&dir, &["build", "defs", "--format", "json"], None);
    assert_eq!(problems.status.code(), Some(1));
    let report: Value = serde_json::from_str(&stdout(&problems)).unwrap();
    assert_eq!(report["report"], 1);
    assert_eq!(report["ok"], false);
    assert_eq!(report["build"], Value::Null);
    let error = report["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["severity"] == "error")
        .unwrap();
    assert_eq!(error["path"], "/catalog/params/eps.bus_voltage/unit");
    assert_eq!(error["context"], Value::Null);
    let location = &error["location"];
    assert_eq!(location["file"], "defs/sat.xml");
    assert!(location["start"].as_u64().unwrap() < location["end"].as_u64().unwrap());

    let failed = morphc_in(&dir, &["build", "nowhere", "--format", "json"], None);
    assert_eq!(failed.status.code(), Some(2));
    let report: Value = serde_json::from_str(&stdout(&failed)).unwrap();
    assert_eq!(report["ok"], false);
    assert!(
        report["error"]
            .as_str()
            .unwrap()
            .contains("can't read nowhere")
    );
}

#[test]
fn help_groups_the_import_options_apart() {
    let help = stdout(&morphc(&["build", "--help"]));
    let (options, import) = help.split_once("XTCE import:").unwrap();
    for flag in [
        "--out",
        "--previous",
        "--fresh",
        "--all-warnings",
        "--format",
    ] {
        assert!(options.contains(flag), "{flag}: {help}");
        assert!(!import.contains(flag), "{flag}: {help}");
    }
    for flag in [
        "--import-family",
        "--import-set",
        "--import-alias",
        "--xtce-naming",
    ] {
        assert!(import.contains(flag), "{flag}: {help}");
    }
}

#[test]
fn a_reader_that_goes_away_doesnt_change_the_exit_code() {
    let dir = scratch("broken_pipe");
    let many = "<LongDescription>Left out</LongDescription>".repeat(3000);
    let xml = fs::read_to_string(paired().join("sat.xml"))
        .unwrap()
        .replace(
            "<IntegerParameterType name=\"Id\" signed=\"false\" shortDescription=\"Packet id\">",
            &format!("<IntegerParameterType name=\"Id\" signed=\"false\">{many}"),
        );
    fs::write(dir.join("sat.xml"), xml).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_morphc"))
        .current_dir(&dir)
        .args(["build", "sat.xml", "--format", "json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("morphc runs");
    // Far more than a pipe holds, so morphc is still writing when the
    // reader goes.
    drop(child.stdout.take());
    let out = child.wait_with_output().expect("morphc finishes");
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
}

#[cfg(unix)]
#[test]
fn links_to_files_are_followed_and_links_to_directories_only_when_given() {
    use std::os::unix::fs::symlink;
    let dir = scratch("links");
    let vendor = dir.join("vendor");
    fs::create_dir(&vendor).unwrap();
    fs::copy(paired().join("sat.xml"), vendor.join("sat.xml")).unwrap();
    let defs = dir.join("defs");
    fs::create_dir(&defs).unwrap();
    fs::copy(paired().join("catalog.toml"), defs.join("catalog.toml")).unwrap();
    symlink(vendor.join("sat.xml"), defs.join("sat.xml")).unwrap();
    symlink(&vendor, defs.join("vendor_dir")).unwrap();

    let out = morphc_in(&dir, &["build", "defs"], None);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(artifacts(&out), ["catalog", "family sat", "set sat"]);
    // The linked file and its target are one file, read once.
    let twice = morphc_in(&dir, &["build", "defs", "vendor/sat.xml"], None);
    assert_eq!(stdout(&twice), stdout(&out));
    let given = morphc_in(
        &dir,
        &["build", "defs/catalog.toml", "defs/vendor_dir"],
        None,
    );
    assert_eq!(given.status.code(), Some(0), "{}", stderr(&given));
    assert_eq!(stdout(&given), stdout(&out));

    symlink(dir.join("gone.toml"), defs.join("gone.toml")).unwrap();
    let dangling = morphc_in(&dir, &["build", "defs"], None);
    assert_eq!(dangling.status.code(), Some(2));
    assert!(
        stderr(&dangling).contains("defs/gone.toml is a link to nothing"),
        "{}",
        stderr(&dangling)
    );
}

// ---- inspect --------------------------------------------------------------------

#[test]
fn inspect_names_every_family_and_set_with_the_digests_build_gives() {
    let built = morphc(&["build", "."]);
    let overview = morphc(&["inspect", "."]);
    assert_eq!(overview.status.code(), Some(0), "{}", stderr(&overview));
    let text = stdout(&overview);
    let digest = |kind: &str| {
        stdout(&built)
            .lines()
            .find(|line| line.starts_with(kind))
            .and_then(|line| line.split_whitespace().find(|w| w.len() == 64))
            .unwrap()
            .to_string()
    };
    assert!(
        text.starts_with(&format!(
            "catalog  {}  4 params, 0 commands\n",
            digest("catalog ")
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!("family sat  {}\n", digest("family sat"))),
        "{text}"
    );
    assert!(
        text.contains("  binary, big-endian; asset id: fixed Sat; set chosen by binding\n"),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "  set sat  {}  1 packet, 0 commands\n",
            digest("set sat")
        )),
        "{text}"
    );
}

#[test]
fn inspect_set_shows_where_each_entry_sits() {
    let out = morphc(&["inspect", "set", "sat", "."]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("  family sat: binary, big-endian\n"),
        "{text}"
    );
    assert!(text.contains("\npacket HK\n"), "{text}");
    assert!(
        text.contains(
            "  16   16    BusVoltage  uint16  eps.bus_voltage  unit V; linear scale 0.01 offset 0\n"
        ),
        "{text}"
    );

    let json = morphc(&["inspect", "set", "sat", ".", "--format", "json"]);
    let report: Value = serde_json::from_str(&stdout(&json)).unwrap();
    let inspect = &report["inspect"];
    assert_eq!(inspect["view"], "set");
    assert_eq!(inspect["set"]["packets"][0]["name"], "HK");
    let entries = inspect["layout"]["packets"][0]["entries"]
        .as_array()
        .unwrap();
    let starts: Vec<u64> = entries
        .iter()
        .map(|e| e["static_offset_bits"].as_u64().unwrap())
        .collect();
    assert_eq!(starts, [0, 8, 16]);
    assert_eq!(
        inspect["set"]["packets"][0]["entries"]
            .as_array()
            .unwrap()
            .len(),
        entries.len()
    );
}

#[test]
fn inspect_set_shows_ranges_counts_and_pointers() {
    let binary = morphc_in(
        &paired(),
        &["inspect", "set", "vendor_x_v2", "-"],
        Some(FULL_BUNDLE),
    );
    assert_eq!(binary.status.code(), Some(0), "{}", stderr(&binary));
    let text = stdout(&binary);
    assert!(
        text.contains("command LOAD_TABLE; encodes eps.load_table; metadata apid = 300\n"),
        "{text}"
    );
    assert!(
        text.contains("len     uint16             length of table\n"),
        "{text}"
    );
    assert!(
        text.contains("  32..524312  16         crc     crc16\n"),
        "{text}"
    );
    assert!(text.contains("  32   16    check   iso8473\n"), "{text}");

    let json = morphc_in(
        &paired(),
        &["inspect", "set", "vendor_j_v1", "-"],
        Some(FULL_BUNDLE),
    );
    assert_eq!(json.status.code(), Some(0), "{}", stderr(&json));
    let text = stdout(&json);
    assert!(text.contains("  family vendor_j: json\n"), "{text}");
    assert!(text.contains("  /cells  cells  struct cell[]\n"), "{text}");

    let report: Value = serde_json::from_str(&stdout(&morphc_in(
        &paired(),
        &["inspect", "set", "vendor_j_v1", "-", "--format", "json"],
        Some(FULL_BUNDLE),
    )))
    .unwrap();
    assert_eq!(report["inspect"]["layout"], Value::Null);
    assert_eq!(report["inspect"]["encoding"], "json");
}

#[test]
fn inspect_describes_lengths_conditions_conversions_and_unfixed_starts() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shapes");
    let out = morphc_in(&dir, &["inspect", "set", "s", "shapes.json"], None);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    let table = text.split_once("packet P\n").unwrap().1;
    assert_eq!(
        table,
        "  bit     size  entry  type                  maps to  notes
  0       8     n      uint8
  8       64    label  string(8, ..0x00)
  72      0     none   uint8 x 0                      when n == 0
  72*     16    v      uint16                a.v      polynomial 1 + 0.5*x + 2*x^2
  88*     8     flags  uint8                          when not (n == 0)
  88..96  0..   rest   uint8 x (to the end)  a.rest
  * starts there in every message, without a static position (decoding spec 5.2)
"
    );
    let overview = stdout(&morphc_in(&dir, &["inspect", "shapes.json"], None));
    assert!(overview.contains("asset id: bits 0..8;"), "{overview}");
}

#[test]
fn inspect_param_lists_every_entry_that_sends_it() {
    let out = morphc(&["inspect", "param", "eps.bus_voltage", "."]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "param eps.bus_voltage  f64, V\n  Main power bus voltage\n  limits red low 24, red high \
         34\n\nsent by\n  set sat  packet HK  entry BusVoltage  (unit V; linear scale 0.01 \
         offset 0)\n"
    );
    let unsent = morphc(&["inspect", "param", "eps.batt_temp", "."]);
    assert!(
        stdout(&unsent).ends_with("\nno packet sends it\n"),
        "{}",
        stdout(&unsent)
    );
}

#[test]
fn inspect_command_lists_every_packet_that_encodes_it() {
    let out = morphc_in(
        &paired(),
        &["inspect", "command", "eps.load_table", "-"],
        Some(FULL_BUNDLE),
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("  arguments\n    table  bytes\n    slot  u64, range ..7, default 0\n"),
        "{text}"
    );
    assert!(
        text.ends_with("\nencoded by\n  set vendor_x_v2  command LOAD_TABLE  (slot, table)\n"),
        "{text}"
    );
    let json = morphc_in(
        &paired(),
        &[
            "inspect",
            "command",
            "eps.load_table",
            "-",
            "--format",
            "json",
        ],
        Some(FULL_BUNDLE),
    );
    let report: Value = serde_json::from_str(&stdout(&json)).unwrap();
    let packet = &report["inspect"]["packets"][0];
    assert_eq!(packet["packet"], "LOAD_TABLE");
    assert_eq!(packet["arguments"][1]["argument"], "table");
}

#[test]
fn inspect_reads_a_stores_latest_build() {
    let dir = paired_copy("inspect_store");
    let first = morphc_in(&dir, &["build", "defs", "--out", "store"], None);
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    let before = manifest(&dir.join("store"));
    edit(
        &dir.join("defs/catalog.toml"),
        "Battery temperature",
        "Battery pack temperature",
    );
    let rebuild = morphc_in(&dir, &["build", "defs", "--out", "store"], None);
    assert_eq!(rebuild.status.code(), Some(0), "{}", stderr(&rebuild));

    let out = morphc_in(&dir, &["inspect", "store"], None);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    let after = manifest(&dir.join("store"));
    assert!(text.contains(after["catalog"].as_str().unwrap()), "{text}");
    assert!(
        text.contains(&format!(
            "    pins the older catalog {}\n",
            before["catalog"].as_str().unwrap()
        )),
        "{text}"
    );
    let param = morphc_in(&dir, &["inspect", "param", "eps.batt_temp", "store"], None);
    assert!(
        stdout(&param).contains("  Battery pack temperature\n"),
        "{}",
        stdout(&param)
    );

    let xtce = morphc_in(&dir, &["inspect", "store", "--import-family", "x"], None);
    assert_eq!(xtce.status.code(), Some(2));
    assert!(
        stderr(&xtce).contains("store is an artifact store"),
        "{}",
        stderr(&xtce)
    );
}

#[test]
fn inspect_reports_problems_and_unknown_names() {
    let dir = paired_copy("inspect_problems");
    let unknown = morphc_in(&dir, &["inspect", "set", "nope", "defs"], None);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(
        stderr(&unknown).contains("there's no set nope; the sets are sat"),
        "{}",
        stderr(&unknown)
    );
    let param = morphc_in(&dir, &["inspect", "param", "nope", "defs"], None);
    assert!(
        stderr(&param).contains("the catalog has no param nope"),
        "{}",
        stderr(&param)
    );

    edit(
        &dir.join("defs/catalog.toml"),
        "unit        = \"V\"",
        "unit        = \"mV\"",
    );
    let broken = morphc_in(&dir, &["inspect", "defs", "--format", "json"], None);
    assert_eq!(broken.status.code(), Some(1));
    let report: Value = serde_json::from_str(&stdout(&broken)).unwrap();
    assert_eq!(report["ok"], false);
    assert_eq!(report["inspect"], Value::Null);
}

#[test]
fn inspect_shows_definitions_as_a_store_of_them_would() {
    let dir = paired_copy("inspect_modes");
    let built = morphc_in(&dir, &["build", "defs", "--out", "store"], None);
    assert_eq!(built.status.code(), Some(0), "{}", stderr(&built));
    let full = scratch("inspect_modes_full");
    fs::write(full.join("full.json"), FULL_BUNDLE).unwrap();
    let stored = morphc_in(&full, &["build", "full.json", "--out", "store"], None);
    assert_eq!(stored.status.code(), Some(0), "{}", stderr(&stored));
    let json = |dir: &Path, args: &[&str]| -> Value {
        let mut args = args.to_vec();
        args.extend(["--format", "json"]);
        let out = morphc_in(dir, &args, None);
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        serde_json::from_str::<Value>(&stdout(&out)).unwrap()["inspect"].clone()
    };
    assert_eq!(
        json(&dir, &["inspect", "set", "sat", "defs"]),
        json(&dir, &["inspect", "set", "sat", "store"])
    );
    // Packets come in name order either way, as the artifacts list them.
    for view in [&["inspect"][..], &["inspect", "set", "vendor_j_v1"]] {
        let mut defs = view.to_vec();
        defs.push("full.json");
        let mut store = view.to_vec();
        store.push("store");
        assert_eq!(json(&full, &defs), json(&full, &store), "{view:?}");
    }
}

#[test]
fn inspect_param_shows_a_composites_frame_and_conditions() {
    let out = morphc_in(
        &paired(),
        &["inspect", "param", "nav.pos_ecef", "-", "--format", "json"],
        Some(FULL_BUNDLE),
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let report: Value = serde_json::from_str(&stdout(&out)).unwrap();
    let composite = &report["inspect"]["uses"][0];
    assert_eq!(composite["entry"], Value::Null);
    assert_eq!(
        composite["members"],
        serde_json::json!(["pos.x", "pos.y", "pos.z"])
    );
    assert_eq!(composite["frame"], "ECI_J2000");
    let text = stdout(&morphc_in(
        &paired(),
        &["inspect", "param", "nav.pos_ecef", "-"],
        Some(FULL_BUNDLE),
    ));
    assert!(
        text.contains("composite of pos.x, pos.y, pos.z  (in ECI_J2000)"),
        "{text}"
    );
}

#[test]
fn inspect_view_words_come_first() {
    let dir = paired_copy("view_words");
    for args in [
        &["inspect", "defs", "set", "sat"][..],
        &["inspect", "--all-warnings", "set", "sat", "defs"],
    ] {
        let out = morphc_in(&dir, args, None);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(
            stderr(&out)
                .contains("views come before the inputs (`morphc inspect set NAME INPUT...`)"),
            "{args:?}: {}",
            stderr(&out)
        );
    }
    // A directory named like a view.
    fs::rename(dir.join("defs"), dir.join("set")).unwrap();
    let word = morphc_in(&dir, &["inspect", "set"], None);
    assert_eq!(word.status.code(), Some(2));
    let path = morphc_in(&dir, &["inspect", "./set"], None);
    assert_eq!(path.status.code(), Some(0), "{}", stderr(&path));

    let unknown = morphc_in(
        &dir,
        &["inspect", "set", "nope", "./set", "--format", "json"],
        None,
    );
    assert_eq!(unknown.status.code(), Some(2));
    let report: Value = serde_json::from_str(&stdout(&unknown)).unwrap();
    assert!(
        report["error"]
            .as_str()
            .unwrap()
            .contains("there's no set nope")
    );
    assert!(report.get("inspect").is_none());
}

// ---- decode ------------------------------------------------------------------------

fn examples(dir: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .join(dir)
}

/// EPS_HK as the library's example test builds it: CUC time 0,
/// spacecraft 101, 28410 mV, 2000 counts, NOMINAL.
const EPS_HK: &str = "000000000000 0065 6efa 07d0 01";

const BEACON_BINDINGS: &str = "mdl_version = \"2.0\"

[[binding]]
family = \"beacon\"
native_id = 7
asset = \"NODE-7\"
effective_from = \"2026-01-01T00:00:00Z\"
";

#[test]
fn decode_routes_identifies_and_gives_catalog_samples() {
    let mission = examples("toml");
    let args = [
        "decode",
        "mission",
        "--set",
        "ccsds_sat_v1",
        "--hex",
        EPS_HK,
        "--meta",
        "apid=100",
        "--time",
        "2026-06-01T12:00:31Z",
    ];
    let out = morphc_in(&mission, &args, None);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    let (head, table) = text.split_once("  param").unwrap();
    assert!(
        head.starts_with(
            "set ccsds_sat_v1  packet EPS_HK  native id 101 (no bindings)\n  def_version "
        ),
        "{text}"
    );
    assert!(
        head.ends_with("  event time 1999-12-31T23:59:28Z\n"),
        "{text}"
    );
    assert_eq!(
        table,
        "            value    unit  from
  eps.bus_voltage  28.41    V     bus_mv
  eps.batt_temp    30.0     Cel   batt_temp_counts
  eps.mode         NOMINAL        mode
"
    );

    let mut json_args = args.to_vec();
    json_args.extend(["--format", "json"]);
    let report: Value =
        serde_json::from_str(&stdout(&morphc_in(&mission, &json_args, None))).unwrap();
    let message = &report["decode"]["messages"][0];
    assert_eq!(message["ok"], true);
    let telemetry = &message["telemetry"];
    assert_eq!(telemetry["asset"], "101");
    assert_eq!(telemetry["native_id"], 101);
    assert_eq!(telemetry["stand_in"], true);
    assert_eq!(telemetry["event_time"]["kind"], "at");
    assert!(
        telemetry["def_version"]
            .as_str()
            .unwrap()
            .parse::<Digest>()
            .is_ok()
    );
    assert_eq!(telemetry["samples"][0]["param"], "eps.bus_voltage");
    assert_eq!(telemetry["samples"][0]["value"], 28.41);
    assert_eq!(telemetry["samples"][0]["unit"], "V");
    assert_eq!(telemetry["samples"][2]["value"], "NOMINAL");
}

#[test]
fn decode_reads_json_payloads_and_shows_raw_entries() {
    let dir = scratch("decode_json");
    fs::write(
        dir.join("line.json"),
        r#"{"sat":"PX-7","ch":"BATT.T","v":212,"t":"2026-06-01T12:00:30.25Z"}"#,
    )
    .unwrap();
    let mission = examples("toml/mission");
    let mission_arg = mission.to_str().unwrap();
    let args = [
        "decode",
        mission_arg,
        "--family",
        "json_sat",
        "--payload",
        "line.json",
        "--raw",
        "--format",
        "json",
    ];
    let out = morphc_in(&dir, &args, None);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let report: Value = serde_json::from_str(&stdout(&out)).unwrap();
    let telemetry = &report["decode"]["messages"][0]["telemetry"];
    assert_eq!(telemetry["asset"], "PX-7");
    assert_eq!(telemetry["event_time"]["time"], "2026-06-01T12:00:30.25Z");
    let celsius = telemetry["samples"][0]["value"].as_f64().unwrap();
    assert!((celsius - 100.0).abs() < 1e-9, "{celsius}");
    assert_eq!(telemetry["record"]["v"].as_f64(), Some(212.0));
    assert_eq!(telemetry["engineering"][0]["path"], "v");
}

#[test]
fn decode_lines_reports_every_message_and_exits_1_when_one_fails() {
    let dir = scratch("decode_lines");
    fs::write(
        dir.join("capture.txt"),
        "# beacons\n0007016efa\n00 07 02 0b 19 00 fa\n\n0007\nzz\n",
    )
    .unwrap();
    fs::write(dir.join("bindings.toml"), BEACON_BINDINGS).unwrap();
    let defs = examples("toml/mixed_versions");
    let defs = defs.to_str().unwrap();
    let args = [
        "decode",
        defs,
        "--lines",
        "capture.txt",
        "--bindings",
        "bindings.toml",
        "--time",
        "2026-06-01T12:00:00Z",
    ];
    let out = morphc_in(&dir, &args, None);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.starts_with("line 2: set beacon_v1  packet BEACON  asset NODE-7 (native id 7)\n"),
        "{text}"
    );
    assert!(
        text.contains("\nline 3: set beacon_v2  packet BEACON  asset NODE-7 (native id 7)\n"),
        "{text}"
    );
    assert!(
        text.contains("\nline 5: error[unreadable_locator]: "),
        "{text}"
    );
    assert!(
        text.contains("\nline 6: error[morphc.invalid_hex]: \"zz\" isn't a hex byte\n"),
        "{text}"
    );
    assert!(text.ends_with("\ndecoded 2 of 4 messages\n"), "{text}");

    let mut json = args.to_vec();
    json.extend(["--format", "json"]);
    let report: Value = serde_json::from_str(&stdout(&morphc_in(&dir, &json, None))).unwrap();
    assert_eq!(report["ok"], false);
    let lines: Vec<(u64, bool)> = report["decode"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["line"].as_u64().unwrap(), m["ok"].as_bool().unwrap()))
        .collect();
    assert_eq!(lines, [(2, true), (3, true), (5, false), (6, false)]);

    // Before the binding takes effect, the id is no asset.
    let early = morphc_in(
        &dir,
        &[
            "decode",
            defs,
            "--hex",
            "0007016efa",
            "--bindings",
            "bindings.toml",
            "--time",
            "2025-06-01T00:00:00Z",
        ],
        None,
    );
    assert_eq!(early.status.code(), Some(1));
    assert!(
        stdout(&early).starts_with("error[unknown_asset]: "),
        "{}",
        stdout(&early)
    );
}

#[test]
fn decode_refuses_bindings_that_dont_fit_the_build() {
    let dir = scratch("decode_bad_bindings");
    fs::write(
        dir.join("bindings.toml"),
        BEACON_BINDINGS.replace(
            "asset = \"NODE-7\"",
            "asset = \"NODE-7\"\nset = \"beacon_v1\"",
        ),
    )
    .unwrap();
    let defs = examples("toml/mixed_versions");
    let out = morphc_in(
        &dir,
        &[
            "decode",
            defs.to_str().unwrap(),
            "--hex",
            "0007016efa",
            "--bindings",
            "bindings.toml",
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(
        err.contains("error[binding_set]: family beacon reads a layout version"),
        "{err}"
    );
    assert!(err.contains("= bindings.toml at /binding/0/set\n"), "{err}");
    let json = morphc_in(
        &dir,
        &[
            "decode",
            defs.to_str().unwrap(),
            "--hex",
            "0007016efa",
            "--bindings",
            "bindings.toml",
            "--format",
            "json",
        ],
        None,
    );
    let report: Value = serde_json::from_str(&stdout(&json)).unwrap();
    let problem = &report["diagnostics"][0];
    assert_eq!(problem["path"], "/binding/0/set");
    assert_eq!(problem["context"], "bindings.toml at /binding/0/set");
}

#[test]
fn decode_command_gives_the_command_and_its_arguments() {
    let dir = scratch("decode_command");
    fs::write(dir.join("full.json"), FULL_BUNDLE).unwrap();
    fs::write(
        dir.join("cmd.json"),
        r#"{"op":"set_mode","args":{"mode":"NOMINAL","delay":2.5}}"#,
    )
    .unwrap();
    let args = [
        "decode",
        "full.json",
        "--command",
        "--set",
        "vendor_j_v1",
        "--payload",
        "cmd.json",
    ];
    let out = morphc_in(&dir, &args, None);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.starts_with("set vendor_j_v1  command SET_MODE  encodes eps.set_mode\n"),
        "{text}"
    );
    assert!(
        text.ends_with("  mode     NOMINAL\n  delay_s  2.5\n"),
        "{text}"
    );
    let mut json = args.to_vec();
    json.extend(["--format", "json"]);
    let report: Value = serde_json::from_str(&stdout(&morphc_in(&dir, &json, None))).unwrap();
    let command = &report["decode"]["messages"][0]["command"];
    assert_eq!(command["command"], "eps.set_mode");
    assert_eq!(
        command["arguments"],
        serde_json::json!([
            { "name": "mode", "value": "NOMINAL", "defaulted": false },
            { "name": "delay_s", "value": 2.5, "defaulted": false }
        ])
    );
}

#[test]
fn decode_against_a_store_uses_its_identities() {
    let dir = scratch("decode_store");
    let mission = examples("toml/mission");
    let built = morphc_in(
        &dir,
        &["build", mission.to_str().unwrap(), "--out", "store"],
        None,
    );
    assert_eq!(built.status.code(), Some(0), "{}", stderr(&built));
    let out = morphc_in(
        &dir,
        &[
            "decode",
            "store",
            "--set",
            "ccsds_sat_v1",
            "--hex",
            EPS_HK,
            "--meta",
            "apid=100",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let report: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(
        report["decode"]["messages"][0]["telemetry"]["def_version"],
        manifest(&dir.join("store"))["sets"]["ccsds_sat_v1"]
    );
}

#[test]
fn decode_says_what_it_needs() {
    let dir = scratch("decode_needs");
    fs::write(dir.join("full.json"), FULL_BUNDLE).unwrap();
    fs::write(dir.join("bindings.toml"), BEACON_BINDINGS).unwrap();
    let mixed = examples("toml/mixed_versions");
    let mixed = mixed.to_str().unwrap();
    let cases: [(&[&str], &str); 8] = [
        (
            &["decode", "full.json", "--hex", "00"],
            "say which with --family or --set",
        ),
        (
            &["decode", "full.json", "--family", "vendor_j", "--hex", "00"],
            "the family is JSON",
        ),
        (
            &["decode", "full.json", "--set", "nope", "--hex", "00"],
            "there's no set nope",
        ),
        (
            &["decode", mixed, "--set", "beacon_v1", "--hex", "00"],
            "picks each message's set by its version, so --set beacon_v1 isn't used",
        ),
        (
            &[
                "decode",
                mixed,
                "--set",
                "beacon_v1",
                "--bindings",
                "bindings.toml",
                "--hex",
                "00",
            ],
            "with --bindings, each asset's binding names its set",
        ),
        (
            &["decode", "-", "--lines", "-"],
            "can't both come from stdin",
        ),
        (
            &["decode", mixed, "--hex", "0", "--meta", "apid"],
            "isn't NAME=VALUE",
        ),
        (
            &["decode", mixed, "--hex", "00", "--time", "noon"],
            "--time",
        ),
    ];
    for (args, message) in cases {
        let out = morphc_in(&dir, args, None);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
        assert!(stderr(&out).contains(message), "{args:?}: {}", stderr(&out));
    }
}

// ---- encode ------------------------------------------------------------------------

#[test]
fn encode_writes_the_bytes_the_library_pins() {
    let xtce = examples("xtce");
    let encode = |args: &[&str]| {
        let mut all = vec!["encode", "demosat.xml"];
        all.extend_from_slice(args);
        morphc_in(&xtce, &all, None)
    };
    let set_mode = encode(&[
        "--command",
        "eps.set_mode",
        "--arg",
        "mode=NOMINAL",
        "--arg",
        "delay=30",
    ]);
    assert_eq!(set_mode.status.code(), Some(0), "{}", stderr(&set_mode));
    let text = stdout(&set_mode);
    assert!(
        text.starts_with("set demosat  command SetMode  encodes eps.set_mode\n"),
        "{text}"
    );
    assert!(text.ends_with("  payload 1064c00000000102001e\n"), "{text}");

    let track = encode(&[
        "--command",
        "demosat.adcs.track",
        "--arg",
        "waypoints_azimuth=[100, 200]",
        "--arg",
        "waypoints_elevation=[10, 20]",
        "--format",
        "json",
    ]);
    let report: Value = serde_json::from_str(&stdout(&track)).unwrap();
    let packet = &report["encode"]["packet"];
    assert_eq!(packet["payload"], "1065c000000002020064000a00c80014");
    assert_eq!(packet["encoding"], "binary");
    assert_eq!(packet["record"]["N"], 2);

    let defaulted = encode(&["--command", "eps.set_mode", "--arg", "mode=SAFE"]);
    assert!(
        stdout(&defaulted).ends_with("  payload 1064c000000001010000\n  defaulted delay\n"),
        "{}",
        stdout(&defaulted)
    );

    let late = encode(&[
        "--command",
        "demosat.eps.setcharge",
        "--arg",
        "current=5.01",
    ]);
    assert_eq!(late.status.code(), Some(1));
    assert!(
        stdout(&late)
            .starts_with("error[out_of_range]: command demosat.eps.setcharge, argument current: "),
        "{}",
        stdout(&late)
    );
    let uneven = encode(&[
        "--command",
        "demosat.adcs.track",
        "--arg",
        "waypoints_azimuth=[100, 200]",
        "--arg",
        "waypoints_elevation=[10]",
        "--format",
        "json",
    ]);
    assert_eq!(uneven.status.code(), Some(1));
    let report: Value = serde_json::from_str(&stdout(&uneven)).unwrap();
    assert_eq!(report["ok"], false);
    assert_eq!(report["encode"]["error"]["code"], "bad_count");
}

#[test]
fn encode_gives_the_metadata_and_decodes_back() {
    let dir = scratch("encode_round_trip");
    fs::write(dir.join("full.json"), FULL_BUNDLE).unwrap();
    let out = morphc_in(
        &dir,
        &[
            "encode",
            "full.json",
            "--set",
            "vendor_x_v2",
            "--command",
            "eps.load_table",
            "--arg",
            "table=c0ffee",
            "--arg",
            "slot=3",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let report: Value = serde_json::from_str(&stdout(&out)).unwrap();
    let packet = &report["encode"]["packet"];
    assert_eq!(packet["metadata"], serde_json::json!({ "apid": 300 }));
    assert_eq!(packet["deprecated"], Value::Null);
    let payload = packet["payload"].as_str().unwrap();
    assert!(payload.starts_with("0303"), "{payload}");

    let back = morphc_in(
        &dir,
        &[
            "decode",
            "full.json",
            "--command",
            "--set",
            "vendor_x_v2",
            "--hex",
            payload,
            "--meta",
            "apid=300",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(back.status.code(), Some(0), "{}", stderr(&back));
    let report: Value = serde_json::from_str(&stdout(&back)).unwrap();
    assert_eq!(
        report["decode"]["messages"][0]["command"]["arguments"],
        serde_json::json!([
            { "name": "table", "value": "c0ffee", "defaulted": false },
            { "name": "slot", "value": 3, "defaulted": false }
        ])
    );

    let json = morphc_in(
        &dir,
        &[
            "encode",
            "full.json",
            "--set",
            "vendor_j_v1",
            "--command",
            "eps.set_mode",
            "--arg",
            "mode=SAFE",
            "--arg",
            "delay_s=2.5",
        ],
        None,
    );
    assert_eq!(json.status.code(), Some(0), "{}", stderr(&json));
    assert!(
        stdout(&json).contains(
            "  payload {\"args\":{\"delay\":2.5,\"mode\":\"SAFE\"},\"op\":\"set_mode\"}\n"
        ),
        "{}",
        stdout(&json)
    );
}

#[test]
fn encode_says_what_it_needs() {
    let dir = scratch("encode_needs");
    fs::write(dir.join("full.json"), FULL_BUNDLE).unwrap();
    let cases: [(&[&str], &str); 5] = [
        (
            &["encode", "full.json", "--command", "nope"],
            "the catalog has no command nope",
        ),
        (
            &["encode", "full.json", "--command", "eps.set_mode"],
            "encode command eps.set_mode; say which with --set",
        ),
        (
            &[
                "encode",
                "full.json",
                "--set",
                "vendor_x_v2",
                "--command",
                "eps.load_table",
                "--arg",
                "slot=x",
            ],
            "--arg slot: \"x\" isn't an unsigned integer",
        ),
        (
            &[
                "encode",
                "full.json",
                "--set",
                "vendor_x_v2",
                "--command",
                "eps.load_table",
                "--arg",
                "slot",
            ],
            "isn't NAME=VALUE",
        ),
        (
            &[
                "encode",
                "full.json",
                "--set",
                "nope",
                "--command",
                "eps.set_mode",
            ],
            "there's no set nope",
        ),
    ];
    for (args, message) in cases {
        let out = morphc_in(&dir, args, None);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
        assert!(stderr(&out).contains(message), "{args:?}: {}", stderr(&out));
    }
    let unknown = morphc_in(
        &dir,
        &[
            "encode",
            "full.json",
            "--set",
            "vendor_x_v2",
            "--command",
            "eps.load_table",
            "--arg",
            "table=00",
            "--arg",
            "volume=11",
        ],
        None,
    );
    assert_eq!(unknown.status.code(), Some(1), "{}", stderr(&unknown));
    assert!(
        stdout(&unknown).starts_with("error[unknown_argument]: "),
        "{}",
        stdout(&unknown)
    );
}

/// vendor_x_v2's HK: time 0, native id 42, 3000 counts (30 V), NOMINAL,
/// flags 1 (the position is there, the label isn't), two current samples,
/// position (1, 2, 3), an empty label length, and no blobs.
const VENDOR_X_HK: &str = "000000000000 002a 0bb8 01 01 02 03e8fc18 00000001 00000002 00000003 00";

#[test]
fn decode_records_carry_each_messages_own_metadata_and_time() {
    let dir = scratch("decode_records");
    fs::write(dir.join("full.json"), FULL_BUNDLE).unwrap();
    let hk = VENDOR_X_HK.replace(' ', "");
    fs::write(
        dir.join("capture.jsonl"),
        format!(
            "# two formats on one link\n\
             {{\"hex\": \"{hk}\", \"meta\": {{\"apid\": 100}}, \"time\": \"2026-06-01T12:00:00Z\"}}\n\
             {{\"hex\": \"{hk}\", \"meta\": {{\"apid\": 100, \"fmt\": 9}}}}\n\
             {{\"json\": {{}}}}\n\
             not json\n"
        ),
    )
    .unwrap();
    let args = [
        "decode",
        "full.json",
        "--family",
        "vendor_x",
        "--records",
        "capture.jsonl",
        "--meta",
        "fmt=2",
        "--raw",
        "--format",
        "json",
    ];
    let out = morphc_in(&dir, &args, None);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let report: Value = serde_json::from_str(&stdout(&out)).unwrap();
    let messages = report["decode"]["messages"].as_array().unwrap();
    let outcomes: Vec<(u64, Value)> = messages
        .iter()
        .map(|m| (m["line"].as_u64().unwrap(), m["error"]["code"].clone()))
        .collect();
    assert_eq!(
        outcomes,
        [
            (2, Value::Null),
            (3, Value::from("unknown_version")),
            (4, Value::from("morphc.invalid_record")),
            (5, Value::from("morphc.invalid_record")),
        ]
    );
    let telemetry = &messages[0]["telemetry"];
    assert_eq!(telemetry["set"], "vendor_x_v2");
    assert_eq!(telemetry["native_id"], 42);
    let samples = telemetry["samples"].as_array().unwrap();
    let value =
        |param: &str| samples.iter().find(|s| s["param"] == param).unwrap()["value"].clone();
    assert_eq!(value("eps.bus_voltage"), 30.0);
    assert_eq!(value("eps.mode"), "NOMINAL");
    assert_eq!(
        value("nav.pos_ecef"),
        serde_json::json!({ "frame": "ECI_J2000", "x": 1.0, "y": 2.0, "z": 3.0 })
    );
    let currents: Vec<(Value, Value)> = samples
        .iter()
        .filter(|s| s["param"] == "eps.bus_current")
        .map(|s| (s["index"].clone(), s["value"].clone()))
        .collect();
    assert_eq!(
        currents,
        [
            (Value::from(0), Value::from(1.0)),
            (Value::from(1), Value::from(-1.0))
        ]
    );
    assert_eq!(telemetry["record"]["blobs"], serde_json::json!([]));

    // For people, entries keep the packet's order and an empty array shows.
    let mut human = args.to_vec();
    human.truncate(human.len() - 2);
    let text = stdout(&morphc_in(&dir, &human, None));
    let entries = text.split_once("  entries\n").unwrap().1;
    let names: Vec<&str> = entries
        .lines()
        .skip(1)
        .take_while(|line| line.starts_with("  ") && !line.starts_with("  line"))
        .map(|line| line.split_whitespace().next().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "bus_counts",
            "mode",
            "flags",
            "n",
            "samples[0]",
            "samples[1]",
            "pos.x",
            "pos.y",
            "pos.z",
            "label_len",
            "blobs"
        ]
    );
    assert!(entries.contains("  blobs       []"), "{entries}");
}

#[test]
fn decode_lines_read_json_documents_for_a_json_family() {
    let dir = scratch("decode_json_lines");
    let mission = examples("toml/mission");
    fs::write(
        dir.join("capture.jsonl"),
        "{\"sat\":\"PX-7\",\"ch\":\"BATT.T\",\"v\":212,\"t\":\"2026-06-01T12:00:30.25Z\"}\n\
         {\"sat\":\"PX-7\",\"ch\":\"PWR.BUS.V\",\"v\":28400,\"t\":\"2026-06-01T12:00:31Z\"}\n",
    )
    .unwrap();
    let out = morphc_in(
        &dir,
        &[
            "decode",
            mission.to_str().unwrap(),
            "--family",
            "json_sat",
            "--lines",
            "capture.jsonl",
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("line 2: set json_sat_v1  packet PWR_BUS_V  native id PX-7 (no bindings)\n"),
        "{text}"
    );
    assert!(
        text.contains("  eps.bus_voltage  28.4   V     v\n"),
        "{text}"
    );
    assert!(text.ends_with("decoded 2 of 2 messages\n"), "{text}");
}

#[test]
fn decode_command_refuses_bindings() {
    let dir = scratch("decode_command_bindings");
    fs::write(dir.join("full.json"), FULL_BUNDLE).unwrap();
    fs::write(dir.join("bindings.toml"), BEACON_BINDINGS).unwrap();
    let out = morphc_in(
        &dir,
        &[
            "decode",
            "full.json",
            "--command",
            "--set",
            "vendor_j_v1",
            "--bindings",
            "bindings.toml",
            "--hex",
            "00",
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("command payloads are decoded with --set, not --bindings"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn encode_takes_arguments_as_decode_writes_them_and_reports_the_librarys_codes() {
    let xtce = examples("xtce");
    let encode = |args: &[&str]| {
        let mut all = vec!["encode", "demosat.xml"];
        all.extend_from_slice(args);
        morphc_in(&xtce, &all, None)
    };
    // Integers in lists as decimal text, as decode writes large ones.
    let track = encode(&[
        "--command",
        "demosat.adcs.track",
        "--arg",
        "waypoints_azimuth=[\"100\", 200]",
        "--arg",
        "waypoints_elevation=[10, \"20\"]",
    ]);
    assert!(
        stdout(&track).contains("  payload 1065c000000002020064000a00c80014\n"),
        "{}",
        stdout(&track)
    );
    let cases: [(&[&str], &str); 4] = [
        (
            &[
                "--command",
                "demosat.adcs.point",
                "--arg",
                "target=SUN",
                "--arg",
                "rate=NaN",
            ],
            "non_finite",
        ),
        (
            &[
                "--command",
                "eps.set_mode",
                "--arg",
                "mode=SAFE",
                "--arg",
                "mode=NOMINAL",
            ],
            "duplicate_argument",
        ),
        (
            &["--command", "eps.set_mode", "--arg", "mode=1"],
            "unknown_state",
        ),
        (
            &["--command", "eps.set_mode", "--arg", "mode=OFF"],
            "unknown_state",
        ),
    ];
    for (args, code) in cases {
        let out = encode(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {}", stderr(&out));
        assert!(
            stdout(&out).starts_with(&format!("error[{code}]: ")),
            "{args:?}: {}",
            stdout(&out)
        );
    }
}

#[test]
fn encode_writes_the_target_only_where_a_packet_carries_it() {
    let dir = scratch("encode_target");
    fs::write(dir.join("full.json"), FULL_BUNDLE).unwrap();
    let carried = morphc_in(
        &dir,
        &[
            "encode",
            "full.json",
            "--set",
            "legacy_v1",
            "--command",
            "eps.set_mode",
            "--arg",
            "mode=NOMINAL",
            "--target",
            "SAT-9",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(carried.status.code(), Some(0), "{}", stderr(&carried));
    let report: Value = serde_json::from_str(&stdout(&carried)).unwrap();
    assert_eq!(report["encode"]["packet"]["record"]["unit"], "SAT-9");

    let nowhere = morphc_in(
        &dir,
        &[
            "encode",
            "full.json",
            "--set",
            "vendor_x_v2",
            "--command",
            "eps.set_mode",
            "--arg",
            "mode=SAFE",
            "--target",
            "42",
        ],
        None,
    );
    assert_eq!(nowhere.status.code(), Some(2));
    assert!(
        stderr(&nowhere).contains("carries no asset id, so --target goes nowhere"),
        "{}",
        stderr(&nowhere)
    );

    let elsewhere = morphc_in(
        &dir,
        &[
            "encode",
            "full.json",
            "--set",
            "vendor_j_v2",
            "--command",
            "eps.set_mode",
            "--arg",
            "mode=SAFE",
        ],
        None,
    );
    assert_eq!(elsewhere.status.code(), Some(1));
    assert!(
        stdout(&elsewhere).starts_with("error[unknown_command]: "),
        "{}",
        stdout(&elsewhere)
    );
}

#[test]
fn encode_warns_about_a_deprecated_command_and_reads_bytes_with_separators() {
    let dir = scratch("encode_deprecated");
    let mut bundle: Value = serde_json::from_str(FULL_BUNDLE).unwrap();
    bundle["catalog"]["commands"]["eps.load_table"]["deprecated"] = Value::from("use eps.load");
    fs::write(dir.join("full.json"), bundle.to_string()).unwrap();
    let out = morphc_in(
        &dir,
        &[
            "encode",
            "full.json",
            "--set",
            "vendor_x_v2",
            "--command",
            "eps.load_table",
            "--arg",
            "table=0xc0 0xff ee",
            "--arg",
            "slot=1",
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("  deprecated: use eps.load\n"),
        "{}",
        stdout(&out)
    );
    assert!(
        stderr(&out).contains(
            "warning[morphc.deprecated_command]: command eps.load_table is deprecated: use eps.load"
        ),
        "{}",
        stderr(&out)
    );
    let payload = stdout(&out)
        .lines()
        .find_map(|line| line.strip_prefix("  payload "))
        .unwrap()
        .to_string();
    assert!(payload.contains("c0ffee"), "{payload}");
}

#[test]
fn decode_records_take_a_json_payload_byte_for_byte_and_a_time_each() {
    let dir = scratch("decode_record_text");
    fs::write(dir.join("full.json"), FULL_BUNDLE).unwrap();
    // The payload as sent, with a duplicate key the decoder refuses; the
    // `json` convenience can't hold one, so it would hide it.
    let text = r#"{"ch":"THM","heater":1,"heater":2,"t":"2026-06-01T12:00:00Z"}"#;
    let record = serde_json::json!({ "text": text }).to_string();
    fs::write(dir.join("capture.jsonl"), format!("{record}\n")).unwrap();
    let out = morphc_in(
        &dir,
        &[
            "decode",
            "full.json",
            "--set",
            "vendor_j_v1",
            "--records",
            "capture.jsonl",
            "--meta",
            "x=1",
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("line 1: error[invalid_json]: "),
        "{}",
        stdout(&out)
    );

    // Each record's time decides its binding.
    fs::write(dir.join("bindings.toml"), BEACON_BINDINGS).unwrap();
    fs::write(
        dir.join("beacons.jsonl"),
        "{\"hex\": \"0007016efa\", \"time\": \"2025-12-31T23:59:59Z\"}\n\
         {\"hex\": \"0007016efa\", \"time\": \"2026-01-01T00:00:00Z\"}\n",
    )
    .unwrap();
    let mixed = examples("toml/mixed_versions");
    let out = morphc_in(
        &dir,
        &[
            "decode",
            mixed.to_str().unwrap(),
            "--records",
            "beacons.jsonl",
            "--bindings",
            "bindings.toml",
        ],
        None,
    );
    let text = stdout(&out);
    assert!(text.starts_with("line 1: error[unknown_asset]: "), "{text}");
    assert!(
        text.contains("\nline 2: set beacon_v1  packet BEACON  asset NODE-7 (native id 7)\n"),
        "{text}"
    );
}
