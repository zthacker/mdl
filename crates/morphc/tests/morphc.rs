//! morphc end to end: run the real binary the way a user (or a frontend in
//! another language, piping IR JSON) would, and check exit codes and output.

use std::io::Write;
use std::process::{Command, Output, Stdio};

const LEGACY_BOX: &str = include_str!("../../mdl/tests/fixtures/ir_legacy_box.json");

/// Cargo builds the binary for integration tests and tells us where it is.
fn morphc(args: &[&str], stdin: Option<&str>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_morphc"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("morphc runs");
    if let Some(text) = stdin {
        child
            .stdin
            .take()
            .expect("stdin is piped")
            .write_all(text.as_bytes())
            .expect("write stdin");
    }
    child.wait_with_output().expect("morphc finishes")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn valid_ir_from_a_file_builds() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../mdl/tests/fixtures/ir_legacy_box.json"
    );
    let out = morphc(&["build", "--ir", path], None);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout).trim(),
        "ok: 1 sets, 1 packets, 3 fields, 2 canonical params (MDL IR v1)"
    );
}

#[test]
fn ir_on_stdin_with_problems_exits_1_and_lists_them() {
    // As a frontend in another language would pipe it, with two mistakes.
    let broken = LEGACY_BOX
        .replace(
            "\"unit\": \"V\",\n              \"canonical\"",
            "\"unit\": \"mV\",\n              \"canonical\"",
        )
        .replace("\"length_bytes\": 4", "\"length_bytes\": 2");
    let out = morphc(&["build", "--ir", "-"], Some(&broken));
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("legacy_box: packet HK, field bus_mv: unit \"mV\""),
        "{stderr}"
    );
    assert!(
        stderr.contains("run past the 16-bit data field"),
        "{stderr}"
    );
    assert!(stderr.ends_with("morphc: 3 problems\n"), "{stderr}");
}

#[test]
fn malformed_json_exits_2() {
    let out = morphc(&["build", "--ir", "-"], Some("{ not json"));
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("isn't valid MDL IR JSON"));
}

#[test]
fn a_missing_definitions_directory_is_a_problem_report() {
    let out = morphc(&["build", "/definitely/not/here"], None);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("catalog.toml"));
}
