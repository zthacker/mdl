//! The walkthrough in `examples/README.md`, and the worked example in the
//! README, run as written. Each `$ morphc` line of a console block runs in
//! a copy of the examples, from `examples/`, and must show
//! the block's lines: all of them, in order, except where a `...` line
//! skips some. `(exit N)` as the last line is the exit code, 0 when it's
//! left out. Output is stderr, then stdout: morphc writes every
//! diagnostic, note and tally before its results.
//!
//! After an intended change to what morphc prints (or to an identity),
//! `UPDATE_WALKTHROUGH=1 cargo test -p morpheus-morphc --test walkthrough` rewrites
//! each block of both from the real output, keeping a block's leading
//! `...`, for the diff to be reviewed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Step {
    command: Vec<String>,
    shown: Vec<String>,
    exit: i32,
}

/// A console block's command, if `line` starts one; a `$ ` line that
/// isn't morphc's is refused, since it would read as output.
fn command(line: &str) -> Option<Vec<String>> {
    let rest = line.strip_prefix("$ ")?;
    let command = rest
        .strip_prefix("morphc ")
        .unwrap_or_else(|| panic!("a walkthrough step runs morphc: {line}"));
    assert!(
        !command.contains(['"', '\'']),
        "a walkthrough command is plain words: {command}"
    );
    Some(command.split_whitespace().map(str::to_string).collect())
}

fn steps(readme: &str) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut in_block = false;
    for line in readme.lines() {
        match line {
            "```console" => in_block = true,
            "```" if in_block => in_block = false,
            _ if !in_block => {}
            _ => match command(line) {
                Some(command) => steps.push(Step {
                    command,
                    shown: Vec::new(),
                    exit: 0,
                }),
                None => {
                    let step = steps.last_mut().expect("output follows a command");
                    match line
                        .strip_prefix("(exit ")
                        .and_then(|rest| rest.strip_suffix(')'))
                    {
                        Some(code) => step.exit = code.parse().unwrap(),
                        None => step.shown.push(line.to_string()),
                    }
                }
            },
        }
    }
    steps
}

/// Whether `output` is `shown`: the same lines in order, `...` standing
/// for any number of lines.
fn shows(shown: &[String], output: &[&str]) -> bool {
    let segments: Vec<&[String]> = shown.split(|line| line == "...").collect();
    let mut at = 0;
    for (index, segment) in segments.iter().enumerate() {
        if segment.is_empty() {
            continue;
        }
        let fits = |start: usize| {
            output.len() >= start + segment.len()
                && segment
                    .iter()
                    .zip(&output[start..])
                    .all(|(expected, line)| expected == line)
        };
        let start = if index == 0 {
            fits(0).then_some(0)
        } else {
            (at..=output.len().saturating_sub(segment.len())).find(|&start| fits(start))
        };
        match start {
            Some(start) => at = start + segment.len(),
            None => return false,
        }
    }
    shown.last().is_some_and(|line| line == "...") || at == output.len()
}

/// What a block shows for `output`: all of it, or, when the block skips
/// its leading warnings, `...` and everything from the tally, the first
/// error, a note or the results on.
fn block(command: &[String], skips: bool, output: &str, exit: i32) -> Vec<String> {
    let mut lines: Vec<String> = output.lines().map(str::to_string).collect();
    if skips {
        let from = lines
            .iter()
            .position(|line| {
                ["morphc: ", "error[", "note: ", "catalog "]
                    .iter()
                    .any(|start| line.starts_with(start))
            })
            .unwrap_or(0);
        lines.drain(..from);
        lines.insert(0, "...".to_string());
    }
    let mut block = vec![format!("$ morphc {}", command.join(" "))];
    block.extend(lines);
    if exit != 0 {
        block.push(format!("(exit {exit})"));
    }
    block
}

/// `readme` with each console block's contents replaced, in order.
fn rewritten(readme: &str, blocks: Vec<Vec<String>>) -> String {
    let mut blocks = blocks.into_iter();
    let mut out = Vec::new();
    let mut in_block = false;
    for line in readme.lines() {
        match line {
            "```console" => {
                in_block = true;
                out.push(line.to_string());
                out.extend(blocks.next().expect("one block per step"));
            }
            "```" if in_block => {
                in_block = false;
                out.push(line.to_string());
            }
            _ if in_block => {}
            _ => out.push(line.to_string()),
        }
    }
    let mut text = out.join("\n");
    text.push('\n');
    text
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            fs::copy(&path, &target).unwrap();
        }
    }
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn the_walkthrough_runs_as_written() {
    runs_as_written("examples/README.md", &["toml", "xtce", "fleet"], 10);
}

#[test]
fn the_readmes_worked_example_runs_as_written() {
    runs_as_written("README.md", &["beacon"], 2);
}

/// The README's JSON for the worked example is what morphc prints for its
/// beacon, value for value.
#[test]
fn the_readmes_json_is_what_morphc_prints() {
    let readme = fs::read_to_string(repository().join("README.md")).unwrap();
    let block: String = readme
        .lines()
        .skip_while(|line| *line != "```json")
        .skip(1)
        .take_while(|line| *line != "```")
        .map(|line| format!("{line}\n"))
        .collect();
    let shown: serde_json::Value = serde_json::from_str(&block).unwrap();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("readme-json");
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    copy_dir(&repository().join("examples/beacon"), &dir.join("beacon"));
    let morphc = |args: &str| {
        let out = Command::new(env!("CARGO_BIN_EXE_morphc"))
            .current_dir(&dir)
            .args(args.split_whitespace())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    };
    morphc("build beacon/beacon.toml --out store");
    let printed: serde_json::Value = serde_json::from_slice(&morphc(
        "decode store --family beacon --hex 070b1901 --bindings beacon/bindings.toml \
         --time 2026-06-01T12:00:00Z --raw --format json",
    ))
    .unwrap();
    assert_eq!(shown, printed["decode"]["messages"][0]["telemetry"]);
}

/// Runs each console block of `readme` (a path in the repository) in a
/// copy of the examples' `parts`, as the module docs say; at least
/// `steps`, so a block the parser misses fails the test.
fn runs_as_written(readme: &str, parts: &[&str], at_least: usize) {
    let examples = repository().join("examples");
    let readme_path = repository().join(readme);
    let name = readme;
    let readme = fs::read_to_string(&readme_path).unwrap();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name.replace('/', "-"));
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    for part in parts {
        copy_dir(&examples.join(part), &dir.join(part));
    }
    let update = std::env::var_os("UPDATE_WALKTHROUGH").is_some();
    // Never in CI, where a stray variable would pass the test by rewriting
    // what it checks.
    assert!(
        !(update && std::env::var_os("CI").is_some()),
        "UPDATE_WALKTHROUGH is set in CI"
    );
    let steps = steps(&readme);
    assert!(steps.len() >= at_least, "{name}: {} steps", steps.len());
    let mut blocks = Vec::new();
    for step in steps {
        let out = Command::new(env!("CARGO_BIN_EXE_morphc"))
            .current_dir(&dir)
            .args(&step.command)
            .output()
            .unwrap();
        let mut output = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        );
        // Paths morphc builds while walking a directory use the platform's
        // separator.
        if cfg!(windows) {
            output = output.replace('\\', "/");
        }
        let exit = out.status.code().unwrap_or(-1);
        if update {
            let skips = step.shown.first().is_some_and(|line| line == "...");
            blocks.push(block(&step.command, skips, &output, exit));
            continue;
        }
        let command = step.command.join(" ");
        let lines: Vec<&str> = output.lines().collect();
        assert!(
            shows(&step.shown, &lines),
            "`morphc {command}` doesn't show what {name} says (UPDATE_WALKTHROUGH=1 rewrites \
             it).\nShown there:\n{}\n\nOutput:\n{output}",
            step.shown.join("\n")
        );
        assert_eq!(exit, step.exit, "`morphc {command}`");
    }
    if update {
        fs::write(&readme_path, rewritten(&readme, blocks)).unwrap();
    }
}

#[test]
fn shown_output_matches_whole_blocks_or_skips_where_it_says() {
    let shown = |lines: &[&str]| lines.iter().map(|l| l.to_string()).collect::<Vec<_>>();
    let output = ["a", "b", "c", "d"];
    assert!(shows(&shown(&["a", "b", "c", "d"]), &output));
    assert!(shows(&shown(&["...", "c", "d"]), &output));
    assert!(shows(&shown(&["a", "...", "d"]), &output));
    assert!(shows(&shown(&["a", "..."]), &output));
    assert!(
        !shows(&shown(&["a", "b", "c"]), &output),
        "extra lines at the end"
    );
    assert!(
        !shows(&shown(&["b", "c", "d"]), &output),
        "lines before the first"
    );
    assert!(!shows(&shown(&["a", "c", "d"]), &output), "a line left out");
    assert!(!shows(&shown(&["...", "d", "c"]), &output), "out of order");
}

#[test]
fn a_rewritten_block_keeps_its_skip_and_exit() {
    let command = vec!["build".to_string(), "x".to_string()];
    let output = "warning[a]: w\n\nmorphc: 1 warning (1 a)\ncatalog  abc\n";
    assert_eq!(
        block(&command, true, output, 1),
        [
            "$ morphc build x",
            "...",
            "morphc: 1 warning (1 a)",
            "catalog  abc",
            "(exit 1)"
        ]
    );
    let readme = "intro\n```console\n$ morphc build x\nold\n```\nend\n";
    assert_eq!(
        rewritten(
            readme,
            vec![vec!["$ morphc build x".to_string(), "new".to_string()]]
        ),
        "intro\n```console\n$ morphc build x\nnew\n```\nend\n"
    );
}
