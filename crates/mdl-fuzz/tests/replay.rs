//! Every committed input of every fuzz target, run through it on stable:
//! the seeds (`fuzz/seeds/<target>`), the corpus grown from them
//! (`fuzz/corpus/<target>`) and the crashes found and fixed
//! (`fuzz/regressions/<target>`). A failure here is a regression, seen
//! without libFuzzer.
//!
//! A panic and a hang reproduce here. An input libFuzzer reports for time
//! (`timeout-*`), memory (`oom-*`) or a leak (`leak-*`) may not: this test
//! has no memory limit or leak checker, and gives an input a minute in a
//! debug build against libFuzzer's 10 s in a release one. Such an input
//! is kept with a unit test that holds the bound.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use mdl_fuzz::{TARGETS, Target};

/// The repository: `crates/mdl-fuzz`, two levels up.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

fn fuzz_dir() -> PathBuf {
    root().join("fuzz")
}

/// The directories of inputs, each holding one directory per target.
const INPUTS: [&str; 3] = ["seeds", "corpus", "regressions"];

/// The largest corpus input committed; `just fuzz-cmin` leaves larger ones
/// out.
const CORPUS_INPUT_MAX: u64 = 4096;

/// How long an input may run: a debug build is slower than libFuzzer's
/// release one, and a hang must still end the test.
const BUDGET: Duration = Duration::from_secs(60);

/// The stack an input runs on: libFuzzer runs release frames on an 8 MiB
/// main thread, and debug frames are larger.
const STACK: usize = 64 << 20;

/// The names of the entries of a directory, none if there's no directory.
fn names(dir: &Path) -> BTreeSet<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return BTreeSet::new();
    };
    entries
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

/// Every input, by its path, with its target.
fn inputs() -> Vec<(String, Target)> {
    let mut inputs = Vec::new();
    for (name, target) in TARGETS {
        for kind in INPUTS {
            let dir = fuzz_dir().join(kind).join(name);
            for file in names(&dir) {
                inputs.push((dir.join(&file).display().to_string(), target));
            }
        }
    }
    inputs
}

/// How an input failed.
enum Failure {
    /// It panicked; the hook printed why.
    Panic(String),
    /// It was still running when the budget ran out.
    Timeout(String),
}

/// The input at `path` run on a thread of its own, named for it, within
/// the budget; how it failed, if it did.
fn run(path: &str, target: Target) -> Option<Failure> {
    let data = fs::read(path).unwrap();
    let (done, finished) = mpsc::channel();
    let input = thread::Builder::new()
        .name(path.to_string())
        .stack_size(STACK)
        .spawn(move || {
            target(&data);
            // Nobody listens after a timeout.
            let _ = done.send(());
        })
        .unwrap();
    match finished.recv_timeout(BUDGET) {
        Ok(()) => {
            input.join().unwrap();
            None
        }
        // It ended without saying so: it panicked, and the hook printed why.
        Err(RecvTimeoutError::Disconnected) => {
            assert!(input.join().is_err());
            Some(Failure::Panic(path.to_string()))
        }
        Err(RecvTimeoutError::Timeout) => Some(Failure::Timeout(format!(
            "{path}: still running after {BUDGET:?}"
        ))),
    }
}

/// The inputs are shared among a worker for each core, each running one at
/// a time (see [`run`]); a generated target's input builds a whole case,
/// which makes them slow one after another. After a timeout no worker
/// takes another input: a hang in a shared path would otherwise cost a
/// minute an input, and the hung threads, which nothing can stop, go when
/// the process ends. What ends the process instead of a thread (a stack
/// overflow, an allocation that fails) can't be caught: a stack overflow
/// names the thread, and the path of the input each worker is running is
/// in `target/tmp/replay-input-<worker>`.
#[test]
fn every_committed_input_passes_its_target() {
    let inputs = inputs();
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let failed = Mutex::new(Vec::new());
    let workers = thread::available_parallelism().map_or(4, |n| n.get());
    thread::scope(|scope| {
        for worker in 0..workers {
            let (inputs, next, stop, failed) = (&inputs, &next, &stop, &failed);
            scope.spawn(move || {
                let running =
                    Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("replay-input-{worker}"));
                while !stop.load(Ordering::Relaxed) {
                    let Some((path, target)) = inputs.get(next.fetch_add(1, Ordering::Relaxed))
                    else {
                        break;
                    };
                    fs::write(&running, path).unwrap();
                    match run(path, *target) {
                        None => {}
                        Some(Failure::Panic(why)) => failed.lock().unwrap().push(why),
                        Some(Failure::Timeout(why)) => {
                            stop.store(true, Ordering::Relaxed);
                            failed.lock().unwrap().push(why);
                        }
                    }
                }
            });
        }
    });
    let failed = failed.into_inner().unwrap();
    assert!(
        failed.is_empty(),
        "inputs that fail their target: {failed:#?}"
    );
}

/// Each target has a binary in `fuzz/` and seeds; each binary and each
/// directory of inputs is a target's.
#[test]
fn targets_binaries_and_inputs_match() {
    let targets: BTreeSet<String> = TARGETS.iter().map(|(name, _)| name.to_string()).collect();
    let binaries: BTreeSet<String> = names(&fuzz_dir().join("fuzz_targets"))
        .into_iter()
        .filter_map(|file| file.strip_suffix(".rs").map(str::to_string))
        .collect();
    assert_eq!(binaries, targets, "fuzz/fuzz_targets");
    let manifest = fs::read_to_string(fuzz_dir().join("Cargo.toml")).unwrap();
    for name in &targets {
        assert!(
            manifest.contains(&format!(
                "name = \"{name}\"\npath = \"fuzz_targets/{name}.rs\""
            )),
            "fuzz/Cargo.toml has no binary {name}"
        );
        assert!(
            !names(&fuzz_dir().join("seeds").join(name)).is_empty(),
            "fuzz/seeds/{name} is empty"
        );
    }
    for kind in INPUTS {
        let dirs = names(&fuzz_dir().join(kind));
        let strays: Vec<&String> = dirs.difference(&targets).collect();
        assert!(
            strays.is_empty(),
            "fuzz/{kind} has inputs for no target: {strays:?}"
        );
    }
}

/// The committed corpus holds no input over 4 KB, which keeps each refresh
/// small; seeds and crashes may be any size.
#[test]
fn corpus_inputs_are_small() {
    let mut large = Vec::new();
    for (name, _) in TARGETS {
        let dir = fuzz_dir().join("corpus").join(name);
        for file in names(&dir) {
            let size = fs::metadata(dir.join(&file)).unwrap().len();
            if size > CORPUS_INPUT_MAX {
                large.push(format!("{name}/{file}: {size} bytes"));
            }
        }
    }
    assert!(large.is_empty(), "run `just fuzz-cmin`: {large:#?}");
}

/// The generated targets commit seeds only. A grown corpus would record
/// the testkit's branches, its code being instrumented too, and an input
/// means what the generators make of its seed at one commit, so a crash
/// such a target finds is kept as a unit test of its definition and
/// payload.
#[test]
fn generated_targets_keep_seeds_only() {
    for name in ["layouts", "shapes"] {
        for kind in ["corpus", "regressions"] {
            let kept = names(&fuzz_dir().join(kind).join(name));
            assert!(
                kept.is_empty(),
                "fuzz/{kind}/{name} holds {} inputs; {name} keeps seeds only (a crash \
                 becomes a unit test)",
                kept.len()
            );
        }
    }
}

/// The versions of each package a lockfile holds, by name.
fn locked(path: &Path) -> BTreeMap<String, BTreeSet<String>> {
    let text = fs::read_to_string(path).unwrap();
    let mut versions: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for package in text.split("[[package]]\n").skip(1) {
        let field = |key: &str| {
            package
                .lines()
                .find_map(|line| line.strip_prefix(key))
                .map(|value| value.trim_matches('"').to_string())
        };
        if let (Some(name), Some(version)) = (field("name = "), field("version = ")) {
            versions.entry(name).or_default().insert(version);
        }
    }
    versions
}

/// The fuzz harness and the Elixir binding's native library, each a
/// workspace of its own, build with the workspace's versions of what they
/// share: a crash libFuzzer finds replays here, and the binding reads and
/// decodes as morphc does. `just fuzz-lock` and `just elixir-lock` bring
/// their lockfiles back in line after `Cargo.lock` changes.
#[test]
fn the_other_lockfiles_lock_the_workspace_versions() {
    let workspace = locked(&root().join("Cargo.lock"));
    let others = [
        (fuzz_dir().join("Cargo.lock"), "just fuzz-lock"),
        (
            root().join("bindings/elixir/native/mdl_nif/Cargo.lock"),
            "just elixir-lock",
        ),
    ];
    for (lockfile, recipe) in others {
        let differ: Vec<String> = locked(&lockfile)
            .iter()
            .filter_map(|(name, versions)| {
                let theirs = workspace.get(name)?;
                (!versions.is_subset(theirs))
                    .then(|| format!("{name} {versions:?}, workspace {theirs:?}"))
            })
            .collect();
        assert!(
            differ.is_empty(),
            "{}: run `{recipe}`: {differ:#?}",
            lockfile.display()
        );
    }
}
