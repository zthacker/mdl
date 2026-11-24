//! morphc end to end: the real binary run the way a user, a CI job or a
//! frontend in another language (piping a bundle) would, checking exit
//! codes, output and the store it writes.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use mdl::ir::{Digest, IrVersion};
use mdl::{runtime, store};
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

/// A release states the versions it reads and writes
/// (`spec/compatibility.md` §6): `--version` gives them, from the library
/// morphc is built with; `-V`, the release alone.
#[test]
fn the_version_states_what_morphc_reads_and_writes() {
    let release = env!("CARGO_PKG_VERSION");
    let out = morphc(&["--version"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!(
            "morphc {release} (MDL IR {}, store format {}, result format {})\n",
            IrVersion::CURRENT,
            store::Manifest::VERSION,
            runtime::FORMAT
        )
    );
    let out = morphc(&["-V"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), format!("morphc {release}\n"));
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
    assert_eq!(report["report"], 3);
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

/// A signed form other than two's complement is part of an entry's type.
#[test]
fn inspect_set_shows_signed_forms() {
    let out = morphc_in(
        &paired(),
        &["inspect", "set", "sink_v1", "-"],
        Some(FULL_BUNDLE),
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("  32   16    trim     int16 sign-magnitude "),
        "{text}"
    );
    assert!(
        text.contains("  48   24    offsets  int8 ones' complement x 3"),
        "{text}"
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

/// A range end past ±(2^53 - 1), which the IR writes as a decimal string,
/// is shown as the integer it is.
#[test]
fn inspect_command_shows_a_wide_range_end_as_its_integer() {
    let out = morphc_in(
        &paired(),
        &["inspect", "command", "sink.mark", "-"],
        Some(FULL_BUNDLE),
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("    stamp  u64, range 1..1152921504606846975\n"),
        "{text}"
    );
}

/// The two-vendor fixture: one catalog command, sys.set_mode, whose
/// DETUMBLE mode vendor B's packet doesn't accept.
fn vendors() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vendors")
}

/// Vendor A's FIRE requires pulse_ms (IR 2.3), so leaving it out is
/// refused there, while vendor B's, which doesn't encode it, sends the
/// catalog's default.
#[test]
fn a_packet_requires_its_own_arguments() {
    let fire = |set: &str, arguments: &[&str]| {
        let mut args = vec![
            "encode",
            "vendors.toml",
            "--set",
            set,
            "--command",
            "effector.fire",
            "--arg",
            "target_id=7",
        ];
        for argument in arguments {
            args.extend(["--arg", argument]);
        }
        morphc_in(&vendors(), &args, None)
    };
    let refused = fire("vendor_a", &[]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        stdout(&refused).contains(
            "error[missing_argument]: command effector.fire, argument pulse_ms: the set requires \
             it, so the catalog's default, 200, isn't sent"
        ),
        "{}",
        stdout(&refused)
    );
    let sent = fire("vendor_a", &["pulse_ms=500"]);
    assert!(
        stdout(&sent).contains("  payload 02000701f4\n"),
        "{}",
        stdout(&sent)
    );
    let defaulted = fire("vendor_b", &[]);
    assert!(
        stdout(&defaulted).contains("  payload 040700\n  defaulted pulse_ms\n"),
        "{}",
        stdout(&defaulted)
    );
    let inspected = morphc_in(
        &vendors(),
        &["inspect", "command", "effector.fire", "vendors.toml"],
        None,
    );
    assert!(
        stdout(&inspected).ends_with(
            "\nencoded by\n  set vendor_a  command FIRE  (target_id in target, pulse_ms in \
             pulse)\n    limits  pulse_ms  required\n  set vendor_b  command FIRE  (target_id \
             in tgt)\n"
        ),
        "{}",
        stdout(&inspected)
    );
}

#[test]
fn a_packet_accepts_only_its_own_states() {
    let set_mode = |set: &str, mode: &str| {
        morphc_in(
            &vendors(),
            &[
                "encode",
                "vendors.toml",
                "--set",
                set,
                "--command",
                "sys.set_mode",
                "--arg",
                &format!("mode={mode}"),
            ],
            None,
        )
    };
    // An encoding refused is the report, on stdout.
    let refused = set_mode("vendor_b", "DETUMBLE");
    assert!(!refused.status.success());
    assert!(
        stdout(&refused).contains(
            "error[unsupported_state]: command sys.set_mode, argument mode: the set doesn't \
             accept state \"DETUMBLE\"; it accepts \"SAFE\" and \"NOMINAL\""
        ),
        "{}",
        stdout(&refused)
    );
    let unknown = set_mode("vendor_b", "BOGUS");
    assert!(
        stdout(&unknown).contains("error[unknown_state]"),
        "{}",
        stdout(&unknown)
    );
    let sent = set_mode("vendor_b", "NOMINAL");
    assert!(
        stdout(&sent).contains("  payload 0301\n"),
        "{}",
        stdout(&sent)
    );
    let sent = set_mode("vendor_a", "DETUMBLE");
    assert!(
        stdout(&sent).contains("  payload 0102\n"),
        "{}",
        stdout(&sent)
    );
    // Inspect shows what each packet accepts before anyone sends it.
    let inspected = morphc_in(
        &vendors(),
        &["inspect", "command", "sys.set_mode", "vendors.toml"],
        None,
    );
    assert!(
        stdout(&inspected).ends_with(
            "\nencoded by\n  set vendor_a  command SET_MODE  (mode)\n  set vendor_b  command MODE  \
             (mode in md)\n    limits  mode  states SAFE NOMINAL\n"
        ),
        "{}",
        stdout(&inspected)
    );
    let json = morphc_in(
        &vendors(),
        &[
            "inspect",
            "command",
            "sys.set_mode",
            "vendors.toml",
            "--format",
            "json",
        ],
        None,
    );
    let json: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(
        json["inspect"]["packets"][1]["limits"],
        serde_json::json!({ "mode": { "states": ["SAFE", "NOMINAL"] } })
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
    // A packet's own limits (IR 2.2) follow it.
    let set_mode = morphc_in(
        &paired(),
        &["inspect", "command", "eps.set_mode", "-"],
        Some(FULL_BUNDLE),
    );
    assert!(
        stdout(&set_mode).contains(
            "  set vendor_j_v1  command SET_MODE  (mode, delay_s in delay)\n    limits  delay_s  \
             min, range ..10, default 0\n"
        ),
        "{}",
        stdout(&set_mode)
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
    let composite = report["inspect"]["uses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|use_| use_["set"] == "vendor_x_v2")
        .unwrap();
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
    // The runtime API's decoded command (spec/api.md §3.5).
    let command = &report["decode"]["messages"][0]["command"];
    assert_eq!(command["family"], "vendor_j");
    assert_eq!(command["set"], "vendor_j_v1");
    assert_eq!(command["packet"], "SET_MODE");
    assert_eq!(command["command"], "eps.set_mode");
    assert_eq!(
        command["arguments"],
        serde_json::json!({ "mode": "NOMINAL", "delay_s": 2.5 })
    );
    assert_eq!(command["defaulted"], serde_json::json!([]));
    assert_eq!(command["target"], Value::Null);
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
    assert_eq!(packet["family"], "demosat");
    assert_eq!(packet["command"], "demosat.adcs.track");
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
    let command = &report["decode"]["messages"][0]["command"];
    assert_eq!(
        command["arguments"],
        serde_json::json!({ "table": "c0ffee", "slot": 3 })
    );
    assert_eq!(command["defaulted"], serde_json::json!([]));

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

/// A telemetry packet encodes from its params' values, with its identify
/// key and its family's alias framed as metadata: the drone's SYS_STATUS
/// is the first message of the walkthrough's capture, byte for byte
/// (`examples/fleet/drone.jsonl`).
#[test]
fn encode_packet_writes_what_the_capture_holds() {
    let dir = examples("");
    let base = [
        "encode",
        "toml/mission",
        "xtce/demosat.xml",
        "--packet",
        "SYS_STATUS",
        "--from",
        "1",
        "--value",
        "eps.bus_voltage=15.2",
    ];
    let mut args = base.to_vec();
    args.extend(["--value", "eps.batt_remaining=76"]);
    let out = morphc_in(&dir, &args, None);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stdout(&out).ends_with(
            "set drone_v1  packet SYS_STATUS  from native id 1\n  def_version \
             9cdb54d367681f74df4f0223c2b3cbbb8cbdd5909ca64839dc2dcfc8d7458617\n  payload \
             0000000000000000000000000000603b00000000000000000000000000004c\n  metadata msgid = \
             1, sysid = 1\n"
        ),
        "{}",
        stdout(&out)
    );
    args.extend(["--format", "json"]);
    let out = morphc_in(&dir, &args, None);
    let report: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(report["report"], 3);
    let telemetry = &report["encode"]["telemetry"];
    assert_eq!(telemetry["native_id"], 1);
    assert_eq!(
        telemetry["metadata"],
        serde_json::json!({ "msgid": 1, "sysid": 1 })
    );
    assert_eq!(
        telemetry["record"],
        serde_json::json!({ "voltage_battery": 15200, "battery_remaining": 76 })
    );

    // A value left out is the library's error, naming the entry.
    let out = morphc_in(&dir, &base, None);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stdout(&out).contains(
            "error[missing_value]: packet SYS_STATUS, battery_remaining: the message holds it, \
             and no value is given for it, raw or by param\n"
        ),
        "{}",
        stdout(&out)
    );
    // A raw value takes the place of a param's.
    let mut raw = base.to_vec();
    raw.extend(["--raw", "battery_remaining=76"]);
    let out = morphc_in(&dir, &raw, None);
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    // --value is for --packet.
    let out = morphc_in(
        &dir,
        &[
            "encode",
            "toml/mission",
            "--command",
            "eps.set_mode",
            "--value",
            "eps.mode=SAFE",
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
}

/// The CUC time code is written from --at: 2026-06-01T12:00:00Z is
/// 833,630,437 s (0x31b030e5) on TAI from 2000-01-01, TAI being 37 s ahead
/// of UTC then, worked out apart from MDL. 28.5 V is 28,500 mV (0x6f54).
#[test]
fn encode_packet_writes_its_clock_from_the_event_time() {
    let dir = examples("");
    let base = [
        "encode",
        "toml/mission",
        "xtce/demosat.xml",
        "--set",
        "ccsds_sat_v1",
        "--packet",
        "EPS_HK",
        "--from",
        "7",
        "--value",
        "eps.bus_voltage=28.5",
        "--value",
        "eps.mode=NOMINAL",
        "--raw",
        "batt_temp_counts=1000",
    ];
    let mut args = base.to_vec();
    args.extend(["--at", "2026-06-01T12:00:00Z"]);
    let out = morphc_in(&dir, &args, None);
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    let text = stdout(&out);
    assert!(
        text.contains(
            "  payload 31b030e5000000076f5403e801\n  metadata apid = 100\n  event time \
             2026-06-01T12:00:00Z\n"
        ),
        "{text}"
    );
    args.extend(["--format", "json"]);
    let out = morphc_in(&dir, &args, None);
    let report: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(
        report["encode"]["telemetry"]["event_time"],
        serde_json::json!({ "kind": "at", "time": "2026-06-01T12:00:00Z" })
    );
    // Without it, nothing gives the time code.
    let out = morphc_in(&dir, &base, None);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stdout(&out).contains("error[missing_value]: packet EPS_HK, time:"),
        "{}",
        stdout(&out)
    );
    // --at is for --packet.
    let out = morphc_in(
        &dir,
        &[
            "encode",
            "toml/mission",
            "--command",
            "eps.set_mode",
            "--at",
            "2026-06-01T12:00:00Z",
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
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

    // Decoded back, the target is where it's sent, and delay_s, which
    // legacy_v1 doesn't encode, has the catalog's default.
    let payload = report["encode"]["packet"]["payload"].as_str().unwrap();
    let decode = |format: &str| {
        let out = morphc_in(
            &dir,
            &[
                "decode",
                "full.json",
                "--command",
                "--set",
                "legacy_v1",
                "--hex",
                payload,
                "--format",
                format,
            ],
            None,
        );
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        stdout(&out)
    };
    let text = decode("human");
    assert!(
        text.contains("  delay_s  0.0      (default; the set doesn't encode it)\n")
            && text.ends_with("  sent to SAT-9\n"),
        "{text}"
    );
    let report: Value = serde_json::from_str(&decode("json")).unwrap();
    let command = &report["decode"]["messages"][0]["command"];
    assert_eq!(
        command["arguments"],
        serde_json::json!({ "mode": "NOMINAL", "delay_s": 0.0 })
    );
    assert_eq!(command["defaulted"], serde_json::json!(["delay_s"]));
    assert_eq!(command["target"], "SAT-9");

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
            "--arg",
            "delay_s=0",
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

    // A native id is never empty (spec/api.md §1): empty text names no
    // target.
    let empty = morphc_in(
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
            "",
        ],
        None,
    );
    assert_eq!(empty.status.code(), Some(1), "{}", stderr(&empty));
    assert!(
        stdout(&empty).starts_with("error[missing_target]: ")
            && stdout(&empty).contains("empty text, which is no native id"),
        "{}",
        stdout(&empty)
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

// ---- build files ---------------------------------------------------------------------

/// A project with a TOML catalog and two XTCE systems, each its own
/// import in the build file.
fn project(test: &str) -> PathBuf {
    let dir = scratch(test);
    let defs = dir.join("defs");
    fs::create_dir_all(defs.join("vendors")).unwrap();
    copy_file(
        &examples("toml/mission/catalog.toml"),
        &defs.join("catalog.toml"),
    );
    copy_file(
        &examples("xtce/demosat.xml"),
        &defs.join("vendors/demosat.xml"),
    );
    copy_file(
        &examples("xtce/imager.xml"),
        &defs.join("vendors/imager.xml"),
    );
    fs::write(
        defs.join("morphc.toml"),
        "[[input]]\npath = \"catalog.toml\"\n\n\
         [[input]]\npath = \"vendors/demosat.xml\"\n\n\
         [[input]]\npath = \"vendors/imager.xml\"\nfamily = \"camera\"\nnaming = \"qualified\"\n",
    )
    .unwrap();
    dir
}

fn copy_file(from: &Path, to: &Path) {
    fs::copy(from, to).unwrap();
}

#[test]
fn a_build_file_imports_each_xtce_input_as_a_family() {
    let dir = project("buildfile_imports");
    let out = morphc_in(&dir, &["build", "-c", "defs/morphc.toml"], None);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let rows = artifacts(&out);
    assert!(rows.contains(&"family camera".to_string()), "{rows:?}");
    assert!(rows.contains(&"family demosat".to_string()), "{rows:?}");
    // Paths are relative to the build file, and the file names it.
    assert!(
        stderr(&out).contains("--> defs/vendors/demosat.xml:"),
        "{}",
        stderr(&out)
    );

    // On the command line, every XTCE document is one import: one family,
    // whose single set holds both vendors' packets, which collide.
    let flat = morphc_in(
        &dir,
        &[
            "build",
            "defs/catalog.toml",
            "defs/vendors/demosat.xml",
            "defs/vendors/imager.xml",
        ],
        None,
    );
    assert_eq!(flat.status.code(), Some(1), "{}", stderr(&flat));
    assert!(
        stderr(&flat).contains("error[ambiguous_identify]"),
        "{}",
        stderr(&flat)
    );

    // inspect, decode and encode read it too.
    let inspect = morphc_in(&dir, &["inspect", "-c", "defs/morphc.toml"], None);
    assert_eq!(inspect.status.code(), Some(0), "{}", stderr(&inspect));
    assert!(
        stdout(&inspect).contains("\nfamily camera  "),
        "{}",
        stdout(&inspect)
    );
}

#[test]
fn a_catalog_owner_closes_the_catalog_to_the_other_inputs() {
    let dir = project("buildfile_owner");
    let defs = dir.join("defs");
    edit(
        &defs.join("morphc.toml"),
        "path = \"catalog.toml\"\n",
        "path = \"catalog.toml\"\ncatalog = \"owner\"\n",
    );
    let build = || {
        morphc_in(
            &dir,
            &["build", "-c", "defs/morphc.toml", "--all-warnings"],
            None,
        )
    };

    // The vendors' aliases map onto the owner's names, and nothing else
    // joins the catalog: a meta-command without an alias is left out, named.
    let out = build();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("ok: 4 params, 1 command, 2 families, 2 sets"),
        "{}",
        stdout(&out)
    );
    assert!(
        stderr(&out).contains(
            "warning[xtce.ignored]: meta-command /DemoSat/EPS/SetHeater isn't imported: the \
             catalog is another input's, and without an alias in the MDL namespace it names no \
             command there"
        ),
        "{}",
        stderr(&out)
    );

    // A misspelt alias is a name the owner doesn't define.
    let demosat = defs.join("vendors/demosat.xml");
    edit(
        &demosat,
        "alias=\"eps.batt_temp\"",
        "alias=\"eps.bat_temp\"",
    );
    let typo = build();
    assert_eq!(typo.status.code(), Some(1), "{}", stderr(&typo));
    assert!(
        stderr(&typo).contains(
            "error[unknown_canonical]: param eps.bat_temp isn't in the catalog; did you mean \
             eps.batt_temp?\n   --> defs/vendors/demosat.xml:"
        ),
        "{}",
        stderr(&typo)
    );
    edit(
        &demosat,
        "alias=\"eps.bat_temp\"",
        "alias=\"eps.batt_temp\"",
    );

    // Another input may restate an owner's name, but not add one, whatever
    // its frontend. TOML files are one bundle, so it's each file's
    // definitions that count.
    fs::write(
        defs.join("extra.toml"),
        "mdl_version = \"2.0\"\n\n[catalog.params.\"eps.heater\"]\ntype = \"bool\"\n",
    )
    .unwrap();
    fs::write(
        defs.join("extra.json"),
        r#"{
  "mdl_version": "2.0",
  "catalog": {
    "params": { "eps.batt_temp": { "type": "f64", "unit": "Cel" } },
    "commands": { "eps.reboot": {} }
  }
}
"#,
    )
    .unwrap();
    edit(
        &defs.join("morphc.toml"),
        "catalog = \"owner\"\n",
        "catalog = \"owner\"\n\n[[input]]\npath = \"extra.toml\"\n\n[[input]]\npath = \"extra.json\"\n",
    );
    let added = build();
    assert_eq!(added.status.code(), Some(1), "{}", stderr(&added));
    let err = stderr(&added);
    let owned = |what: &str, at: &str| {
        format!(
            "error[catalog_owner]: this build's catalog is owned by other inputs, which don't \
             define {what}; define it there, or map onto a name they define\n --> {at}"
        )
    };
    assert!(
        err.contains(&owned("param eps.heater", "defs/extra.toml:3:")),
        "{err}"
    );
    assert!(
        err.contains(&owned("command eps.reboot", "defs/extra.json:5:")),
        "{err}"
    );
    assert_eq!(err.matches("error[").count(), 2, "{err}");
    // The imports' warnings are reported with the errors.
    assert!(
        err.contains("warning[xtce.ignored]: meta-command /DemoSat/EPS/SetHeater"),
        "{err}"
    );

    // An owner listed after a directory holding it would be read as that
    // directory's, and so own nothing.
    fs::write(
        defs.join("ordered.toml"),
        "[[input]]\npath = \".\"\n\n[[input]]\npath = \"catalog.toml\"\ncatalog = \"owner\"\n",
    )
    .unwrap();
    let ordered = morphc_in(&dir, &["build", "-c", "defs/ordered.toml"], None);
    assert_eq!(ordered.status.code(), Some(2), "{}", stderr(&ordered));
    assert!(
        stderr(&ordered).contains(
            "defs/catalog.toml is read as part of input defs, listed before the catalog owner \
             that includes it, so it wouldn't be owned; list the owner first"
        ),
        "{}",
        stderr(&ordered)
    );

    // An input is the owner or says nothing.
    edit(
        &defs.join("morphc.toml"),
        "catalog = \"owner\"",
        "catalog = \"own\"",
    );
    let bad = build();
    assert!(
        stderr(&bad).contains("error[morphc.build_file]: unknown variant `own`, expected `owner`"),
        "{}",
        stderr(&bad)
    );
}

#[test]
fn a_build_file_in_the_working_directory_is_found_and_left_out_of_walks() {
    let dir = project("buildfile_found");
    let defs = dir.join("defs");
    let found = morphc_in(&defs, &["build"], None);
    assert_eq!(found.status.code(), Some(0), "{}", stderr(&found));
    assert!(artifacts(&found).contains(&"family camera".to_string()));

    // Walking a directory reads its definitions, not its build file.
    let walk = dir.join("walk");
    fs::create_dir(&walk).unwrap();
    copy_file(
        &examples("toml/mission/catalog.toml"),
        &walk.join("catalog.toml"),
    );
    fs::write(
        walk.join("morphc.toml"),
        "[[input]]\npath = \"catalog.toml\"\n",
    )
    .unwrap();
    let walked = morphc_in(&dir, &["build", "walk"], None);
    assert_eq!(walked.status.code(), Some(0), "{}", stderr(&walked));
    let named = morphc_in(&dir, &["build", "defs/morphc.toml"], None);
    assert_eq!(named.status.code(), Some(2));
    assert!(
        stderr(&named).contains("is a build file, not definitions; give it with -c"),
        "{}",
        stderr(&named)
    );

    let empty = scratch("buildfile_none");
    let none = morphc_in(&empty, &["build"], None);
    assert_eq!(none.status.code(), Some(2));
    assert!(
        stderr(&none).contains("or run where a morphc.toml is"),
        "{}",
        stderr(&none)
    );
}

#[test]
fn a_build_file_says_what_it_needs() {
    let dir = project("buildfile_needs");
    let write = |text: &str| fs::write(dir.join("bad.toml"), text).unwrap();
    let build = |args: &[&str]| morphc_in(&dir, args, None);

    let both = build(&["build", "defs/catalog.toml", "-c", "defs/morphc.toml"]);
    assert!(
        stderr(&both).contains("give inputs or a build file (-c), not both"),
        "{}",
        stderr(&both)
    );
    let flags = build(&["build", "-c", "defs/morphc.toml", "--import-family", "x"]);
    assert!(
        stderr(&flags).contains("each input's import options are written in it"),
        "{}",
        stderr(&flags)
    );

    write("[[input]]\npath = \"defs/catalog.toml\"\nfamilly = \"x\"\n");
    let unknown = build(&["build", "-c", "bad.toml"]);
    let err = stderr(&unknown);
    assert!(
        err.contains("error[morphc.build_file]: unknown field `familly`"),
        "{err}"
    );
    assert!(err.contains(" --> bad.toml:3:1\n"), "{err}");

    write("input = []\n");
    assert!(stderr(&build(&["build", "-c", "bad.toml"])).contains("it lists no inputs"));
    write("[[input]]\npath = \"-\"\n");
    assert!(stderr(&build(&["build", "-c", "bad.toml"])).contains("not stdin"));
    write("[[input]]\npath = \"defs/catalog.toml\"\nfamily = \"x\"\n");
    let options = build(&["build", "-c", "bad.toml"]);
    assert!(
        stderr(&options).contains("input defs/catalog.toml has import options but no .xml files"),
        "{}",
        stderr(&options)
    );
    write(
        "[[input]]\npath = \"defs/vendors/imager.xml\"\nalias = { kind = \"path\", pointer = \"/id\" }\n",
    );
    let alias = build(&["build", "-c", "bad.toml"]);
    // A path alias on a binary family: a definitions problem, located
    // like any other.
    assert_eq!(alias.status.code(), Some(1), "{}", stderr(&alias));
    let said = stderr(&alias);
    assert!(
        said.contains("error[wrong_encoding]: path locators only apply"),
        "{said}"
    );
    assert!(said.contains(" --> defs/vendors/imager.xml:"), "{said}");
    for out in [both, flags, unknown] {
        assert_eq!(out.status.code(), Some(2));
    }
}

/// An XTCE import whose alias a build file reads from metadata builds,
/// and a message carrying that metadata decodes as the asset bound to it.
#[test]
fn a_build_files_metadata_alias_builds_and_decodes() {
    let dir = scratch("buildfile_metadata_alias");
    fs::write(
        dir.join("sat.xml"),
        r#"<SpaceSystem name="SmallSat" xmlns="http://www.omg.org/spec/XTCE/20180204"><TelemetryMetaData><ParameterTypeSet><IntegerParameterType name="U8" signed="false"><IntegerDataEncoding sizeInBits="8"/></IntegerParameterType></ParameterTypeSet><ParameterSet><Parameter name="Mode" parameterTypeRef="U8"/></ParameterSet><ContainerSet><SequenceContainer name="Status"><EntryList><ParameterRefEntry parameterRef="Mode"/></EntryList></SequenceContainer></ContainerSet></TelemetryMetaData></SpaceSystem>"#,
    )
    .unwrap();
    fs::write(
        dir.join("morphc.toml"),
        "[[input]]\npath = \"sat.xml\"\nalias = { kind = \"metadata\", name = \"scid\" }\n",
    )
    .unwrap();
    fs::write(
        dir.join("bindings.toml"),
        "mdl_version = \"2.0\"\n\n[[binding]]\nfamily = \"smallsat\"\nnative_id = 42\nasset = \"SAT-1\"\n\
         set = \"smallsat\"\neffective_from = \"2026-01-01T00:00:00Z\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("capture.jsonl"),
        "{\"hex\": \"07\", \"meta\": {\"scid\": 42}, \"time\": \"2026-06-01T12:00:00Z\"}\n",
    )
    .unwrap();
    let built = morphc_in(
        &dir,
        &["build", "-c", "morphc.toml", "--out", "store"],
        None,
    );
    assert_eq!(built.status.code(), Some(0), "{}", stderr(&built));
    let args = [
        "decode",
        "store",
        "--family",
        "smallsat",
        "--records",
        "capture.jsonl",
        "--bindings",
        "bindings.toml",
        "--format",
        "json",
    ];
    let out = morphc_in(&dir, &args, None);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let report: Value = serde_json::from_str(&stdout(&out)).unwrap();
    let telemetry = &report["decode"]["messages"][0]["telemetry"];
    assert_eq!(telemetry["asset"], "SAT-1", "{report:#}");
    assert_eq!(telemetry["native_id"], 42, "{report:#}");
    assert_eq!(telemetry["samples"][0]["value"], 7, "{report:#}");
}

#[test]
fn a_build_file_error_is_located_for_tools_too() {
    let dir = scratch("buildfile_json_error");
    fs::write(
        dir.join("morphc.toml"),
        "[[input]]\npath = \"x\"\nfamilly = \"y\"\n",
    )
    .unwrap();
    let out = morphc_in(&dir, &["build", "--format", "json"], None);
    assert_eq!(out.status.code(), Some(2));
    let report: Value = serde_json::from_str(&stdout(&out)).unwrap();
    let problem = &report["diagnostics"][0];
    assert_eq!(problem["code"], "morphc.build_file");
    assert_eq!(problem["location"]["file"], "morphc.toml");
    assert_eq!(problem["location"]["line"], 3);
    assert!(report["error"].as_str().unwrap().contains("familly"));

    for (text, message) in [
        (
            "version = 2\n[[input]]\npath = \"x\"\nwhatever = 1\n",
            "build file format 2; this morphc reads format 1",
        ),
        (
            "version = \"1\"\n[[input]]\npath = \"x\"\n",
            "version is an integer, 1",
        ),
    ] {
        fs::write(dir.join("morphc.toml"), text).unwrap();
        let out = morphc_in(&dir, &["build"], None);
        assert_eq!(out.status.code(), Some(2));
        assert!(stderr(&out).contains(message), "{}", stderr(&out));
    }
    let dir_given = morphc_in(&dir, &["build", "-c", "."], None);
    assert!(
        stderr(&dir_given).contains("is a directory; -c takes a build file"),
        "{}",
        stderr(&dir_given)
    );
}

#[test]
fn a_build_file_says_when_the_order_it_lists_isnt_the_order_of_precedence() {
    let dir = project("buildfile_order");
    let defs = dir.join("defs");
    copy_file(
        &examples("toml/mission/drone.toml"),
        &defs.join("drone.toml"),
    );
    fs::write(
        defs.join("order.toml"),
        "version = 1\n\n[[input]]\npath = \"drone.toml\"\n\n[[input]]\npath = \"vendors/demosat.xml\"\n\n\
         [[input]]\npath = \"catalog.toml\"\n",
    )
    .unwrap();
    let out = morphc_in(&dir, &["build", "-c", "defs/order.toml"], None);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("catalog.toml comes after "), "{err}");
    assert!(err.contains("so it takes precedence over it"), "{err}");
    // The catalog's text wins, as the note says.
    assert!(
        err.contains("the earlier definition's \"Main power bus voltage\" takes precedence"),
        "{err}"
    );
    // Found in the working directory, it says so.
    let found = morphc_in(&defs, &["build"], None);
    assert!(
        stderr(&found).contains(
            "note: no inputs were given, so the definitions are the ones ./morphc.toml lists"
        ),
        "{}",
        stderr(&found)
    );
}

#[test]
fn a_build_file_keeps_an_xtce_system_whole_and_reads_a_file_once() {
    let dir = scratch("buildfile_split");
    let xtce = "http://www.omg.org/spec/XTCE/20180204";
    fs::write(
        dir.join("a.xml"),
        format!(r#"<SpaceSystem name="Root" xmlns="{xtce}"><TelemetryMetaData><ParameterTypeSet><IntegerParameterType name="U8" signed="false"><IntegerDataEncoding sizeInBits="8"/></IntegerParameterType></ParameterTypeSet></TelemetryMetaData></SpaceSystem>"#),
    )
    .unwrap();
    fs::write(
        dir.join("b.xml"),
        format!(r#"<SpaceSystem name="Other" xmlns="{xtce}"><TelemetryMetaData><ParameterSet><Parameter name="X" parameterTypeRef="/Root/U8"/></ParameterSet><ContainerSet><SequenceContainer name="HK"><EntryList><ParameterRefEntry parameterRef="X"/></EntryList></SequenceContainer></ContainerSet></TelemetryMetaData></SpaceSystem>"#),
    )
    .unwrap();
    let together = morphc_in(&dir, &["build", "a.xml", "b.xml"], None);
    assert_eq!(together.status.code(), Some(0), "{}", stderr(&together));
    fs::write(
        dir.join("morphc.toml"),
        "[[input]]\npath = \"a.xml\"\n\n[[input]]\npath = \"b.xml\"\n",
    )
    .unwrap();
    let split = morphc_in(&dir, &["build"], None);
    assert_eq!(split.status.code(), Some(1), "{}", stderr(&split));
    let err = stderr(&split);
    assert!(err.contains("error[xtce.reference]"), "{err}");
    assert!(
        err.contains("note: references between XTCE documents resolve within one import"),
        "{err}"
    );

    // A file listed twice is read once, with its first input's options.
    let fleet = scratch("buildfile_twice");
    copy_dir_all(&examples("xtce"), &fleet.join("xtce"));
    fs::write(
        fleet.join("morphc.toml"),
        "[[input]]\npath = \"xtce/imager.xml\"\nfamily = \"a\"\n\n[[input]]\npath = \"xtce\"\nfamily = \"b\"\n",
    )
    .unwrap();
    let twice = morphc_in(&fleet, &["build"], None);
    assert_eq!(twice.status.code(), Some(0), "{}", stderr(&twice));
    let rows = artifacts(&twice);
    assert!(
        rows.contains(&"family a".to_string()) && rows.contains(&"family b".to_string()),
        "{rows:?}"
    );
}

#[test]
fn a_build_file_takes_aliases_absolute_paths_and_links() {
    let dir = project("buildfile_forms");
    let defs = dir.join("defs");
    let demosat = defs.join("vendors/demosat.xml");
    fs::write(
        dir.join("forms.toml"),
        format!(
            "[[input]]\npath = {:?}\nalias = {{ kind = \"fixed\", value = \"SAT-9\" }}\n",
            demosat.to_str().unwrap()
        ),
    )
    .unwrap();
    let fixed = morphc_in(&dir, &["inspect", "-c", "forms.toml"], None);
    assert_eq!(fixed.status.code(), Some(0), "{}", stderr(&fixed));
    assert!(
        stdout(&fixed).contains("asset id: fixed SAT-9;"),
        "{}",
        stdout(&fixed)
    );
    fs::write(
        dir.join("forms.toml"),
        "[[input]]\npath = \"defs/vendors/demosat.xml\"\nalias = { kind = \"bits\", offset_bits = 0, size_bits = 8 }\n",
    )
    .unwrap();
    let bits = morphc_in(&dir, &["inspect", "-c", "forms.toml"], None);
    assert_eq!(bits.status.code(), Some(0), "{}", stderr(&bits));
    assert!(
        stdout(&bits).contains("asset id: bits 0..8;"),
        "{}",
        stdout(&bits)
    );

    #[cfg(unix)]
    {
        // A link reads the inputs the file it points at lists.
        std::os::unix::fs::symlink(defs.join("morphc.toml"), dir.join("link.toml")).unwrap();
        let linked = morphc_in(&dir, &["build", "-c", "link.toml"], None);
        assert_eq!(linked.status.code(), Some(0), "{}", stderr(&linked));
        assert!(artifacts(&linked).contains(&"family camera".to_string()));
    }
}

fn copy_dir_all(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_dir_all(&path, &target);
        } else {
            fs::copy(&path, &target).unwrap();
        }
    }
}

// ---- an XTCE import joining a family ----------------------------------------------------

/// Vendor X's two flight software versions, an XTCE document each, as one
/// family under an owner's catalog, with bindings that move X-1 from the
/// first to the second in June; copied into a test's scratch directory.
fn joined(test: &str) -> PathBuf {
    let dir = scratch(test);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/joined");
    for entry in fs::read_dir(&fixture).unwrap() {
        let entry = entry.unwrap();
        copy_file(&entry.path(), &dir.join(entry.file_name()));
    }
    dir
}

#[test]
fn an_xtce_import_joins_a_family_another_input_defines() {
    let dir = joined("joined_family");
    let build = morphc_in(&dir, &["build", "--all-warnings"], None);
    assert_eq!(build.status.code(), Some(0), "{}", stderr(&build));
    assert_eq!(
        artifacts(&build),
        [
            "catalog",
            "family vendor_x",
            "set vendor_x_v1",
            "set vendor_x_v2"
        ]
    );
    assert!(
        stdout(&build).ends_with("ok: 1 param, 2 commands, 1 family, 2 sets (MDL IR 2.3)\n"),
        "{}",
        stdout(&build)
    );

    // The bindings say which set decodes X-1's housekeeping, on either side
    // of the switch: millivolts, then a status byte and centivolts.
    let decode = |hex: &str, time: &str| {
        let out = morphc_in(
            &dir,
            &[
                "decode",
                "--hex",
                hex,
                "--bindings",
                "bindings.toml",
                "--time",
                time,
            ],
            None,
        );
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        stdout(&out)
    };
    let march = decode("6ef4", "2026-03-01T00:00:00Z");
    assert!(
        march.starts_with("set vendor_x_v1  packet HK  asset X-1 (native id VendorX)\n"),
        "{march}"
    );
    assert!(
        march.contains("  eps.bus_voltage  28.404  V     BusVoltage\n"),
        "{march}"
    );
    let july = decode("010b18", "2026-07-01T00:00:00Z");
    assert!(
        july.starts_with("set vendor_x_v2  packet HK  asset X-1 (native id VendorX)\n"),
        "{july}"
    );
    assert!(july.contains("  eps.bus_voltage  28.40"), "{july}");

    // What each version's packets accept is what its XTCE says beyond the
    // owner's catalog: the first has no DETUMBLE mode, and gives the pulse
    // no initial value, so the catalog's default isn't sent with it; both
    // keep the pulse to 1000 ms, in the catalog's unit.
    let encode = |set: &str, command: &str, arguments: &[&str]| {
        let mut args = vec!["encode", "--set", set, "--command", command];
        for argument in arguments {
            args.extend(["--arg", argument]);
        }
        let out = morphc_in(&dir, &args, None);
        (out.status.code(), stdout(&out))
    };
    let (code, text) = encode("vendor_x_v1", "sys.set_mode", &["mode=DETUMBLE"]);
    assert_eq!(code, Some(1), "{text}");
    assert!(
        text.contains(
            "error[unsupported_state]: command sys.set_mode, argument mode: the set doesn't \
             accept state \"DETUMBLE\"; it accepts \"SAFE\" and \"NOMINAL\""
        ),
        "{text}"
    );
    let (code, text) = encode("vendor_x_v2", "sys.set_mode", &["mode=DETUMBLE"]);
    assert_eq!(code, Some(0), "{text}");
    assert!(text.contains("  payload 0102\n"), "{text}");
    let (code, text) = encode("vendor_x_v1", "effector.fire", &["target_id=7"]);
    assert_eq!(code, Some(1), "{text}");
    assert!(
        text.contains(
            "error[missing_argument]: command effector.fire, argument pulse_ms: the set \
             requires it, so the catalog's default, 200, isn't sent"
        ),
        "{text}"
    );
    let (code, text) = encode(
        "vendor_x_v1",
        "effector.fire",
        &["target_id=7", "pulse_ms=500"],
    );
    assert_eq!(code, Some(0), "{text}");
    assert!(text.contains("  payload 02000701f4\n"), "{text}");
    let inspected = morphc_in(&dir, &["inspect", "command", "sys.set_mode"], None);
    assert!(
        stdout(&inspected).ends_with(
            "\nencoded by\n  set vendor_x_v1  command SetMode  (mode)\n    limits  mode  \
             states SAFE NOMINAL\n  set vendor_x_v2  command SetMode  (mode)\n"
        ),
        "{}",
        stdout(&inspected)
    );
    let inspected = morphc_in(&dir, &["inspect", "command", "effector.fire"], None);
    assert!(
        stdout(&inspected)
            .contains("  set vendor_x_v1  command Fire  (target_id, pulse_ms)\n    limits  pulse_ms  range 50..1000, required\n"),
        "{}",
        stdout(&inspected)
    );

    // Two inputs that each define the family are the conflict they were.
    edit(
        &dir.join("morphc.toml"),
        "join = \"vendor_x\"",
        "family = \"vendor_x\"",
    );
    let twice = morphc_in(&dir, &["build"], None);
    assert_eq!(twice.status.code(), Some(1), "{}", stderr(&twice));
    assert!(
        stderr(&twice).contains(
            "error[duplicate_name]: family vendor_x is defined twice\n --> vendor_x_v2.xml:4:1"
        ),
        "{}",
        stderr(&twice)
    );
}

#[test]
fn a_join_the_build_cant_follow_is_refused() {
    let dir = joined("joined_refused");
    let build_file = dir.join("morphc.toml");
    let original = fs::read_to_string(&build_file).unwrap();
    let build_with = |from: &str, to: &str| {
        assert!(original.contains(from), "{from:?}");
        fs::write(&build_file, original.replace(from, to)).unwrap();
        let out = morphc_in(&dir, &["build"], None);
        (out.status.code(), stderr(&out))
    };
    let join = "join = \"vendor_x\"";

    // What the import can't join is reported at it, naming its set.
    let (code, err) = build_with(join, "join = \"vendor_y\"");
    assert_eq!(code, Some(1), "{err}");
    assert!(
        err.contains(
            "error[xtce.options]: set vendor_x_v2 joins family vendor_y, which no other input \
             defines; did you mean vendor_x?\n --> vendor_x_v2.xml:4:1"
        ),
        "{err}"
    );
    let (code, err) = build_with(
        join,
        "join = \"vendor_x\"\nalias = { kind = \"fixed\", value = \"X-2\" }",
    );
    assert_eq!(code, Some(1), "{err}");
    assert!(
        err.contains(
            "error[xtce.options]: set vendor_x_v2 joins family vendor_x, whose alias is \
             {\"kind\":\"fixed\",\"value\":\"VendorX\"}, but the alias option is \
             {\"kind\":\"fixed\",\"value\":\"X-2\"}; leave it out to take the family's"
        ),
        "{err}"
    );
    fs::write(
        dir.join("drone.toml"),
        "mdl_version = \"2.0\"\n\n[[families]]\nname = \"drone\"\nencoding = \"json\"\nalias = { \
         kind = \"path\", pointer = \"/id\" }\n",
    )
    .unwrap();
    let (code, err) = build_with(
        "[[input]]\npath = \"vendor_x_v2.xml\"\njoin = \"vendor_x\"",
        "[[input]]\npath = \"drone.toml\"\n\n[[input]]\npath = \"vendor_x_v2.xml\"\njoin = \"drone\"",
    );
    assert_eq!(code, Some(1), "{err}");
    assert!(
        err.contains(
            "error[xtce.options]: set vendor_x_v2 can't join family drone: the family is JSON, \
             and an XTCE import is binary"
        ),
        "{err}"
    );

    // What no input could follow is the build file's.
    for (from, to, problem) in [
        (
            join,
            "join = \"vendor_x\"\nfamily = \"other\"",
            "input vendor_x_v2.xml names family other and joins family vendor_x; give one",
        ),
        (
            "set  = \"vendor_x_v2\"",
            "",
            "input vendor_x_v2.xml joins family vendor_x, so it names the set it adds to it \
             (set)",
        ),
        (
            "set    = \"vendor_x_v1\"",
            "set    = \"vendor_x_v1\"\nset_version = 1",
            "input vendor_x_v1.xml gives set_version, which is for a set that joins a family \
             (join)",
        ),
        (
            "catalog = \"owner\"",
            "catalog = \"owner\"\njoin = \"vendor_x\"\nset = \"x\"",
            "input catalog.toml owns the catalog and joins family vendor_x: the inputs that \
             refer to an owner's catalog are read with it, before any input that joins a \
             family, so an owner can't join one",
        ),
    ] {
        let (code, err) = build_with(from, to);
        assert_eq!(code, Some(2), "{err}");
        assert!(
            err.contains(&format!("error[morphc.build_file]: {problem}\n")),
            "{err}"
        );
    }

    // An import that joins a family the input with problems would have
    // defined waits for it: it's named, not reported missing a family.
    fs::write(&build_file, &original).unwrap();
    let v1 = dir.join("vendor_x_v1.xml");
    edit(
        &v1,
        "parameterTypeRef=\"Millivolts\"",
        "parameterTypeRef=\"Milivolts\"",
    );
    let out = morphc_in(&dir, &["build"], None);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("error[xtce.reference]: Milivolts"), "{err}");
    assert!(!err.contains("xtce.options"), "{err}");
    assert!(
        err.contains(
            "note: not read yet, since the family each joins is in no input that read, and an \
             input that didn't read may define it: vendor_x_v2.xml\n"
        ),
        "{err}"
    );
    // A referring import reads whatever else has problems, so its own are
    // reported with theirs: the owner's catalog and the first version both.
    let catalog = dir.join("catalog.toml");
    edit(&catalog, "[catalog.params.", "[catalog.params");
    let out = morphc_in(&dir, &["build"], None);
    let err = stderr(&out);
    assert!(err.contains("error[toml.syntax]"), "{err}");
    assert!(err.contains("error[xtce.reference]: Milivolts"), "{err}");
    edit(&catalog, "[catalog.params", "[catalog.params.");
    // A catalog that doesn't merge is reported once, and each import that
    // refers to it waits for it, named.
    fs::write(
        dir.join("conflict.json"),
        "{\"mdl_version\": \"2.0\", \"catalog\": {\"commands\": {\"effector.fire\": \
         {\"arguments\": [{\"name\": \"target_id\", \"type\": \"u64\"}]}}}}",
    )
    .unwrap();
    edit(
        &v1,
        "parameterTypeRef=\"Milivolts\"",
        "parameterTypeRef=\"Millivolts\"",
    );
    fs::write(
        &build_file,
        original.clone() + "\n[[input]]\npath = \"conflict.json\"\ncatalog = \"owner\"\n",
    )
    .unwrap();
    let out = morphc_in(&dir, &["build"], None);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert_eq!(err.matches("error[duplicate_name]").count(), 1, "{err}");
    assert!(
        err.contains(
            "note: not read yet, since the catalog they refer to doesn't merge: \
             vendor_x_v1.xml, vendor_x_v2.xml\n"
        ),
        "{err}"
    );
    fs::write(&build_file, &original).unwrap();
    edit(
        &v1,
        "parameterTypeRef=\"Millivolts\"",
        "parameterTypeRef=\"Milivolts\"",
    );
    // One with problems that defines nothing it needs holds nothing back:
    // the second version still reads, and its own problems are reported
    // in the same run.
    edit(
        &v1,
        "parameterTypeRef=\"Milivolts\"",
        "parameterTypeRef=\"Millivolts\"",
    );
    fs::write(dir.join("vendor_y.xml"), "<SpaceSystem name=\"VendorY\"").unwrap();
    fs::write(
        &build_file,
        original.replace(
            join,
            "join = \"vendor_x\"\nalias = { kind = \"fixed\", value = \"X-2\" }",
        ) + "\n[[input]]\npath = \"vendor_y.xml\"\nfamily = \"vendor_y\"\n",
    )
    .unwrap();
    let out = morphc_in(&dir, &["build"], None);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("error[xtce.syntax]"), "{err}");
    assert!(
        err.contains("error[xtce.options]: set vendor_x_v2 joins family vendor_x, whose alias"),
        "{err}"
    );
    assert!(!err.contains("not read yet"), "{err}");
}

/// A store whose set an earlier release wrote with a spelling this release
/// refuses to build (a packet's limits restating the argument's own unit,
/// `spec/compatibility.md` §7) still decodes and encodes.
#[test]
fn a_store_with_a_spelling_builds_now_refuse_still_reads() {
    let dir = joined("joined_spelling");
    let out = morphc_in(&dir, &["build", "--out", "store"], None);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    // What 0.1.0's importer wrote for vendor X's pulse: its unit, which is
    // the catalog's.
    let store_dir = dir.join("store");
    let manifest_path = store_dir.join(store::MANIFEST);
    let mut manifest: Value =
        serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
    let digest = manifest["sets"]["vendor_x_v1"]
        .as_str()
        .unwrap()
        .to_string();
    let mut set: Value = serde_json::from_str(
        &fs::read_to_string(store_dir.join(format!("{digest}.json"))).unwrap(),
    )
    .unwrap();
    let fire = set["set"]["commands"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|command| command["name"] == "Fire")
        .unwrap();
    fire["arguments"]["pulse_ms"]["unit"] = Value::from("ms");
    let canonical = mdl::canonical::to_canonical_string(&set).unwrap();
    let digest = Digest::of_canonical(&canonical);
    fs::write(store_dir.join(format!("{digest}.json")), &canonical).unwrap();
    manifest["sets"]["vendor_x_v1"] = Value::from(digest.to_string());
    fs::write(&manifest_path, manifest.to_string()).unwrap();
    let encode = |pulse: &str| {
        morphc_in(
            &dir,
            &[
                "encode",
                "store",
                "--set",
                "vendor_x_v1",
                "--command",
                "effector.fire",
                "--arg",
                "target_id=7",
                "--arg",
                pulse,
            ],
            None,
        )
    };
    let sent = encode("pulse_ms=500");
    assert_eq!(sent.status.code(), Some(0), "{}", stderr(&sent));
    assert!(
        stdout(&sent).contains("  payload 02000701f4\n"),
        "{}",
        stdout(&sent)
    );
    let refused = encode("pulse_ms=1500");
    assert_eq!(refused.status.code(), Some(1), "{}", stderr(&refused));
    assert!(
        stdout(&refused).contains("error[out_of_range]"),
        "{}",
        stdout(&refused)
    );
}

/// An XTCE aggregate parameter marked with `MDL.frame` (xtce.md §4.1)
/// builds into one composite and decodes to one vector sample.
#[test]
fn an_xtce_aggregate_marked_with_a_frame_decodes_to_one_vector() {
    let dir = scratch("xtce_composite");
    fs::write(
        dir.join("sat.xml"),
        r#"<SpaceSystem name="Sat" xmlns="http://www.omg.org/spec/XTCE/20180204">
  <TelemetryMetaData>
    <ParameterTypeSet>
      <FloatParameterType name="Rate"><UnitSet><Unit>rad/s</Unit></UnitSet><FloatDataEncoding sizeInBits="32"/></FloatParameterType>
      <AggregateParameterType name="Vec"><MemberList>
        <Member name="x" typeRef="Rate"/><Member name="y" typeRef="Rate"/><Member name="z" typeRef="Rate"/>
      </MemberList></AggregateParameterType>
    </ParameterTypeSet>
    <ParameterSet>
      <Parameter name="Rate" parameterTypeRef="Vec">
        <AncillaryDataSet><AncillaryData name="MDL.frame">BODY</AncillaryData></AncillaryDataSet>
      </Parameter>
    </ParameterSet>
    <ContainerSet>
      <SequenceContainer name="ADCS"><EntryList><ParameterRefEntry parameterRef="Rate"/></EntryList></SequenceContainer>
    </ContainerSet>
  </TelemetryMetaData>
</SpaceSystem>"#,
    )
    .unwrap();
    // x 1.0, y 2.0, z -0.5, big-endian binary32.
    let out = morphc_in(
        &dir,
        &[
            "decode",
            "sat.xml",
            "--hex",
            "3f80000040000000bf000000",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let report: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(
        report["decode"]["messages"][0]["telemetry"]["samples"],
        serde_json::json!([{
            "param": "sat.rate",
            "value": { "frame": "BODY", "x": 1.0, "y": 2.0, "z": -0.5 },
            "unit": "rad/s",
            "path": "Rate_x,Rate_y,Rate_z",
            "index": null,
            "time_offset_ns": null,
            "quality": "good"
        }])
    );
}

/// A marked XTCE array numbered 1 to 4, scalar last, reordered by
/// `MDL.members` (xtce.md §4.1), decodes to one quaternion sample.
#[test]
fn an_xtce_array_marked_with_a_frame_decodes_to_one_quaternion() {
    let dir = scratch("xtce_array_composite");
    fs::write(
        dir.join("sat.xml"),
        r#"<SpaceSystem name="Sat" xmlns="http://www.omg.org/spec/XTCE/20180204">
  <TelemetryMetaData>
    <ParameterTypeSet>
      <FloatParameterType name="Q"><FloatDataEncoding sizeInBits="32"/></FloatParameterType>
      <ArrayParameterType name="Quat" arrayTypeRef="Q">
        <AncillaryDataSet><AncillaryData name="MDL.frame">ECI_J2000</AncillaryData></AncillaryDataSet>
        <DimensionList><Dimension>
          <StartingIndex><FixedValue>1</FixedValue></StartingIndex>
          <EndingIndex><FixedValue>4</FixedValue></EndingIndex>
        </Dimension></DimensionList>
      </ArrayParameterType>
    </ParameterTypeSet>
    <ParameterSet>
      <Parameter name="Att" parameterTypeRef="Quat">
        <AncillaryDataSet><AncillaryData name="MDL.members">4 1 2 3</AncillaryData></AncillaryDataSet>
      </Parameter>
    </ParameterSet>
    <ContainerSet>
      <SequenceContainer name="ADCS"><EntryList><ParameterRefEntry parameterRef="Att"/></EntryList></SequenceContainer>
    </ContainerSet>
  </TelemetryMetaData>
</SpaceSystem>"#,
    )
    .unwrap();
    // q1 0.5, q2 -0.5, q3 0.25, q4 0.75 (the scalar), big-endian binary32.
    let out = morphc_in(
        &dir,
        &[
            "decode",
            "sat.xml",
            "--hex",
            "3f000000bf0000003e8000003f400000",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let report: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(
        report["decode"]["messages"][0]["telemetry"]["samples"],
        serde_json::json!([{
            "param": "sat.att",
            "value": { "frame": "ECI_J2000", "w": 0.75, "x": 0.5, "y": -0.5, "z": 0.25 },
            "unit": null,
            "path": "Att[3],Att[0],Att[1],Att[2]",
            "index": null,
            "time_offset_ns": null,
            "quality": "good"
        }])
    );
}
