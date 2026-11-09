//! morphc end to end: the real binary run the way a user, a CI job or a
//! frontend in another language (piping a bundle) would, checking exit
//! codes, output and the store it writes.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use mdl::v2::ir::Digest;
use serde_json::Value;

const FULL_BUNDLE: &str = include_str!("../../mdl/tests/fixtures/v2/full_bundle.json");

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
            &["build", "sat.xml", "--alias", "bits:x:8"],
            "isn't a locator",
        ),
        (&["build", "nowhere"], "can't read nowhere"),
        (
            &["build", "catalog.toml", "--family", "x"],
            "the XTCE options apply to .xml inputs",
        ),
        (
            &["build", "sat.xml", "--alias", "serial"],
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
    for flag in ["--family", "--set", "--alias", "--xtce-naming"] {
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
