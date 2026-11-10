//! Definition files: found on disk, read, and put through their frontends.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use mdl::source::{Parsed, SourceDiagnostic, SourceFile, Sources};

use crate::store;

/// Which frontend reads a file, chosen by its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frontend {
    Toml,
    Xtce,
    Json,
}

impl Frontend {
    /// By extension, in any case (vendor files often come as `.XML`).
    fn of(path: &Path) -> Option<Frontend> {
        let extension = path.extension()?.to_str()?;
        [
            ("toml", Frontend::Toml),
            ("xml", Frontend::Xtce),
            ("json", Frontend::Json),
        ]
        .into_iter()
        .find(|(known, _)| extension.eq_ignore_ascii_case(known))
        .map(|(_, frontend)| frontend)
    }

    /// Where a directory's files of this frontend come, and so their
    /// precedence: hand-written definitions, then bundles other tools
    /// wrote, then vendor imports.
    fn rank(self) -> u8 {
        match self {
            Frontend::Toml => 0,
            Frontend::Json => 1,
            Frontend::Xtce => 2,
        }
    }
}

/// A definition file, or `-` for a JSON bundle on stdin.
#[derive(Debug)]
pub struct Input {
    pub path: PathBuf,
    pub frontend: Frontend,
}

impl Input {
    fn is_stdin(&self) -> bool {
        self.path.as_os_str() == "-"
    }

    fn name(&self) -> String {
        if self.is_stdin() {
            "<stdin>".to_string()
        } else {
            self.path.display().to_string()
        }
    }
}

/// The files `paths` name, in order, each read once, where it's first
/// named.
///
/// A directory gives its TOML files, then its JSON bundles, then its XTCE
/// documents, each kind in name order, so what decides precedence is what
/// a file is, not what it's called. Hidden entries (names starting with
/// `.`) and artifact stores are left out, so a store kept beside the
/// definitions isn't read as bundles. A link given as an input is
/// followed; inside a directory, links to files are and links to
/// directories aren't.
pub fn find(paths: &[PathBuf]) -> anyhow::Result<Vec<Input>> {
    let mut found = Found::default();
    for path in paths {
        if path.as_os_str() == "-" {
            if found.stdin {
                bail!("`-` (stdin) is given twice");
            }
            found.stdin = true;
            found.inputs.push(Input {
                path: path.clone(),
                frontend: Frontend::Json,
            });
            continue;
        }
        let metadata =
            fs::metadata(path).with_context(|| format!("can't read {}", path.display()))?;
        if !metadata.is_dir() {
            let Some(frontend) = Frontend::of(path) else {
                bail!(
                    "{} isn't a definition file: morphc reads .toml (MDL TOML), .xml (XTCE) \
                     and .json (an MDL IR bundle)",
                    path.display()
                );
            };
            found.add(path, frontend)?;
            continue;
        }
        if store::is_store(path) {
            bail!("{} is an artifact store, not definitions", path.display());
        }
        let mut files = Vec::new();
        walk(path, &mut files)?;
        if files.is_empty() {
            bail!(
                "{} has no definition files (.toml, .xml or .json)",
                path.display()
            );
        }
        files.sort_by_key(|(_, frontend)| frontend.rank());
        for (file, frontend) in files {
            found.add(&file, frontend)?;
        }
    }
    Ok(found.inputs)
}

#[derive(Default)]
struct Found {
    inputs: Vec<Input>,
    /// Each file's canonical path, so one named twice is read once.
    seen: BTreeSet<PathBuf>,
    stdin: bool,
}

impl Found {
    fn add(&mut self, path: &Path, frontend: Frontend) -> anyhow::Result<()> {
        let canonical =
            fs::canonicalize(path).with_context(|| format!("can't read {}", path.display()))?;
        if self.seen.insert(canonical) {
            self.inputs.push(Input {
                path: path.to_path_buf(),
                frontend,
            });
        }
        Ok(())
    }
}

/// The definition files below `dir`, in name order.
fn walk(dir: &Path, files: &mut Vec<(PathBuf, Frontend)>) -> anyhow::Result<()> {
    let listing = || format!("can't list {}", dir.display());
    let mut entries = fs::read_dir(dir)
        .with_context(listing)?
        .collect::<Result<Vec<_>, _>>()
        .with_context(listing)?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        let kind = entry
            .file_type()
            .with_context(|| format!("can't read {}", path.display()))?;
        if kind.is_dir() {
            if !store::is_store(&path) {
                walk(&path, files)?;
            }
            continue;
        }
        let Some(frontend) = Frontend::of(&path) else {
            continue;
        };
        if kind.is_symlink() {
            match fs::metadata(&path) {
                Ok(target) if target.is_file() => {}
                Ok(_) => continue,
                Err(err) => {
                    return Err(err)
                        .with_context(|| format!("{} is a link to nothing", path.display()));
                }
            }
        }
        files.push((path, frontend));
    }
    Ok(())
}

/// Every input's text, in order, named as it was given.
pub fn read(inputs: &[Input]) -> anyhow::Result<Sources> {
    let mut sources = Sources::new();
    for input in inputs {
        let text = if input.is_stdin() {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .context("can't read stdin as UTF-8 text")?;
            text
        } else {
            fs::read_to_string(&input.path)
                .with_context(|| format!("can't read {} as UTF-8 text", input.path.display()))?
        };
        sources.add(input.name(), text);
    }
    Ok(sources)
}

/// One bundle per frontend in the order of its first file: the TOML files
/// as one, the XTCE documents as one, and each JSON bundle alone. On
/// failure, every problem and every warning, in file order.
pub fn parse(
    sources: &Sources,
    inputs: &[Input],
    xtce: &mdl_xtce::Options,
) -> Result<Vec<Parsed>, Vec<SourceDiagnostic>> {
    let files: Vec<(Frontend, SourceFile<'_>)> = inputs
        .iter()
        .map(|input| input.frontend)
        .zip(sources.iter())
        .collect();
    let of = |frontend: Frontend| {
        files
            .iter()
            .filter(move |(f, _)| *f == frontend)
            .map(|(_, file)| *file)
    };
    let mut parts = Vec::new();
    let mut problems = Vec::new();
    let mut failed = false;
    let (mut toml_read, mut xtce_read) = (false, false);
    for (frontend, file) in &files {
        let result = match frontend {
            Frontend::Toml if !toml_read => {
                toml_read = true;
                mdl_toml::read(of(Frontend::Toml))
            }
            Frontend::Xtce if !xtce_read => {
                xtce_read = true;
                mdl_xtce::read(of(Frontend::Xtce), xtce)
            }
            Frontend::Json => mdl::json::read(*file),
            Frontend::Toml | Frontend::Xtce => continue,
        };
        match result {
            Ok(parsed) => parts.push(parsed),
            Err(diagnostics) => {
                failed = true;
                problems.extend(diagnostics);
            }
        }
    }
    if !failed {
        return Ok(parts);
    }
    for part in &parts {
        problems.extend(part.warnings.iter().cloned());
    }
    problems.sort_by_key(|d| d.span.map(|span| (span.source, span.start)));
    Err(problems)
}
