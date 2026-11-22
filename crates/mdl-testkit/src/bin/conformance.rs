//! Writes the generated conformance cases under `conformance/`, or under
//! the directory given: decoding cases in `decoding/oracle/`, encoding
//! cases in `encoding/oracle/` and command decoding cases in
//! `decoding_commands/oracle/` (`spec/conformance.md` §2.3, §3.2, §4),
//! the last from the encoding cases' layouts. Each is
//! a fixed sample of the oracle's layouts, each from a seed of its own, so
//! running it again with the same testkit and proptest writes the same
//! files. The files are a snapshot: regenerate them on purpose, and review
//! the diff. `just conformance` runs it, as
//! `cargo run -p mdl-testkit --bin conformance [dir]`.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use mdl_testkit::conformance::{case_file, command_decoding_case_file, encoding_case_file};
use mdl_testkit::layout::command::{self, Commands};
use mdl_testkit::layout::{self, Telemetry};
use proptest::strategy::{BoxedStrategy, Strategy, ValueTree};
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use serde_json::Value;

/// A kind of file: its name, the generator, how many layouts, and what its
/// files hold.
struct Kind<T> {
    name: &'static str,
    strategy: BoxedStrategy<T>,
    count: usize,
    holds: &'static str,
}

fn decoding_kinds() -> Vec<Kind<Telemetry>> {
    vec![
        Kind {
            name: "binary",
            strategy: layout::telemetry(),
            count: 24,
            holds: "a binary layout as layout::telemetry() makes it, and its messages",
        },
        Kind {
            name: "binary-damaged",
            strategy: layout::damaged(),
            count: 24,
            holds: "a binary layout as layout::damaged() makes it, and its messages, faults planted in most of them",
        },
        Kind {
            name: "json",
            strategy: layout::json_telemetry(),
            count: 12,
            holds: "a JSON layout as layout::json_telemetry() makes it, and its messages",
        },
        Kind {
            name: "json-damaged",
            strategy: layout::json_damaged(),
            count: 12,
            holds: "a JSON layout as layout::json_damaged() makes it, and its messages, faults planted in most of them",
        },
    ]
}

fn encoding_kinds() -> Vec<Kind<Commands>> {
    vec![
        Kind {
            name: "commands",
            strategy: command::commands(),
            count: 24,
            holds: "a binary command layout as layout::command::commands() makes it, its requests and its refused requests",
        },
        Kind {
            name: "json-commands",
            strategy: command::json_commands(),
            count: 12,
            holds: "a JSON command layout as layout::command::json_commands() makes it, its requests and its refused requests",
        },
    ]
}

fn main() -> Result<(), Box<dyn Error>> {
    let root = std::env::args()
        .nth(1)
        .map_or_else(|| PathBuf::from("conformance"), PathBuf::from);
    let decoding = write_suite(
        &root.join("decoding/oracle"),
        &decoding_kinds(),
        case_file,
        "the result or error the oracle works out forward from decoding.md, in code separate from \
         MDL's decoder; spec/conformance.md section 2.3 says what that does and doesn't check.",
    )?;
    let encoding = write_suite(
        &root.join("encoding/oracle"),
        &encoding_kinds(),
        encoding_case_file,
        "the command or error the oracle works out forward from encoding.md, in code separate from \
         MDL's encoder; spec/conformance.md section 3.2 says what that does and doesn't check.",
    )?;
    // The encoding cases' layouts, from the same seeds: each request's
    // payload decodes back to what it encoded.
    let commands = write_suite(
        &root.join("decoding_commands/oracle"),
        &encoding_kinds(),
        command_decoding_case_file,
        "the command or error the oracle works out forward from encoding.md for each payload, in \
         code separate from MDL's decoder; spec/conformance.md section 4 says what that does and \
         doesn't check.",
    )?;
    println!(
        "wrote {decoding} decoding cases, {encoding} encoding cases and {commands} command \
         decoding cases under {}",
        root.display()
    );
    Ok(())
}

/// Writes one suite's files into `dir`, each layout's from its own seed,
/// and returns how many cases they hold.
fn write_suite<T: std::fmt::Debug>(
    dir: &Path,
    kinds: &[Kind<T>],
    file: fn(&T, &str, &str) -> Value,
    each_with: &str,
) -> Result<usize, Box<dyn Error>> {
    fs::create_dir_all(dir)?;
    remove_written(dir, kinds)?;
    let mut cases = 0;
    for kind in kinds {
        for index in 0..kind.count {
            let mut runner = TestRunner::new_with_rng(
                Config::default(),
                TestRng::from_seed(RngAlgorithm::ChaCha, &seed(kind.name, index)),
            );
            let generated = kind
                .strategy
                .new_tree(&mut runner)
                .map_err(|reason| format!("{} {index}: {reason}", kind.name))?
                .current();
            let origin = format!("{} seed {index}", kind.name);
            let about = format!(
                "Generated by the testkit's oracle (crates/mdl-testkit, {origin}): {}, each with \
                 {each_with}",
                kind.holds
            );
            let written = file(&generated, &about, &origin);
            cases += written["cases"].as_array().map_or(0, Vec::len);
            let path = dir.join(format!("{}-{index:02}.json", kind.name));
            fs::write(&path, serde_json::to_string_pretty(&written)? + "\n")?;
        }
    }
    Ok(cases)
}

/// A layout's seed: its kind's name, then its index.
fn seed(kind: &str, index: usize) -> [u8; 32] {
    let mut seed = [0; 32];
    let name = kind.as_bytes();
    let length = name.len().min(24);
    seed[..length].copy_from_slice(&name[..length]);
    seed[24..].copy_from_slice(&(index as u64).to_le_bytes());
    seed
}

/// Removes the files an earlier run wrote, so a smaller sample leaves none
/// behind: only names of the kinds' own pattern, `<kind>-<nn>.json`.
fn remove_written<T>(dir: &Path, kinds: &[Kind<T>]) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let written = kinds.iter().any(|kind| {
            name.strip_prefix(kind.name)
                .and_then(|rest| rest.strip_prefix('-'))
                .and_then(|rest| rest.strip_suffix(".json"))
                .is_some_and(|number| {
                    number.len() == 2 && number.bytes().all(|b| b.is_ascii_digit())
                })
        });
        if written {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}
