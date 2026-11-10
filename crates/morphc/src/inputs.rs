//! Definition files: found on disk, read, and put through their frontends.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, bail};
use mdl::source::{Parsed, SourceDiagnostic, SourceFile, Sources};

use crate::{buildfile, store};

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
    /// Which XTCE import an XTCE document belongs to (an index into the
    /// imports' options): one import per build-file input, else one.
    pub import: usize,
    /// How diagnostics name it.
    shown: String,
}

impl Input {
    fn is_stdin(&self) -> bool {
        self.path.as_os_str() == "-"
    }

    fn name(&self) -> String {
        self.shown.clone()
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
/// directories aren't, and a build file in a directory is left out.
///
/// Each path comes with the XTCE import its documents belong to.
pub fn find(paths: &[(PathBuf, usize)]) -> anyhow::Result<Vec<Input>> {
    let mut found = Found::default();
    for (path, import) in paths {
        let import = *import;
        if path.as_os_str() == "-" {
            if found.stdin {
                bail!("`-` (stdin) is given twice");
            }
            found.stdin = true;
            found.inputs.push(Input {
                path: path.clone(),
                frontend: Frontend::Json,
                import,
                shown: "<stdin>".to_string(),
            });
            continue;
        }
        let metadata =
            fs::metadata(path).with_context(|| format!("can't read {}", path.display()))?;
        if !metadata.is_dir() {
            if path.file_name().is_some_and(|name| name == buildfile::NAME) {
                bail!(
                    "{} is a build file, not definitions; give it with -c",
                    path.display()
                );
            }
            let Some(frontend) = Frontend::of(path) else {
                bail!(
                    "{} isn't a definition file: morphc reads .toml (MDL TOML), .xml (XTCE) \
                     and .json (an MDL IR bundle)",
                    path.display()
                );
            };
            found.add(path, frontend, import)?;
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
            found.add(&file, frontend, import)?;
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
    fn add(&mut self, path: &Path, frontend: Frontend, import: usize) -> anyhow::Result<()> {
        let canonical =
            fs::canonicalize(path).with_context(|| format!("can't read {}", path.display()))?;
        let shown = shown(path, &canonical);
        if self.seen.insert(canonical) {
            self.inputs.push(Input {
                path: path.to_path_buf(),
                frontend,
                import,
                shown,
            });
        }
        Ok(())
    }
}

/// A path's name for people, as [`shown`] gives it.
pub fn shown_path(path: &Path) -> String {
    match fs::canonicalize(path) {
        Ok(canonical) => shown(path, &canonical),
        Err(_) => path.display().to_string(),
    }
}

/// A file's name for people: as given, unless that's absolute or climbs
/// with `..` (as a build file's inputs do), when the real path relative to
/// the working directory reads better, if the file is below it.
fn shown(path: &Path, canonical: &Path) -> String {
    let roundabout =
        path.is_absolute() || path.components().any(|part| part == Component::ParentDir);
    let below = std::env::current_dir()
        .and_then(fs::canonicalize)
        .ok()
        .and_then(|dir| canonical.strip_prefix(dir).ok().map(Path::to_path_buf));
    match below {
        Some(relative) if roundabout => relative.display().to_string(),
        _ => path.display().to_string(),
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
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') || name == buildfile::NAME {
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
/// as one, each XTCE import's documents as one (with `imports[i]` as
/// import `i`'s options), and each JSON bundle alone. On failure, every
/// problem and every warning, in file order.
pub fn parse(
    sources: &Sources,
    inputs: &[Input],
    imports: &[mdl_xtce::Options],
) -> Result<Vec<Parsed>, Vec<SourceDiagnostic>> {
    let files: Vec<(&Input, SourceFile<'_>)> = inputs.iter().zip(sources.iter()).collect();
    let toml = || {
        files
            .iter()
            .filter(|(input, _)| input.frontend == Frontend::Toml)
            .map(|(_, file)| *file)
    };
    let import = |index: usize| {
        files
            .iter()
            .filter(move |(input, _)| input.frontend == Frontend::Xtce && input.import == index)
            .map(|(_, file)| *file)
    };
    let mut parts = Vec::new();
    let mut problems = Vec::new();
    let mut failed = false;
    let mut toml_read = false;
    let mut imports_read = BTreeSet::new();
    for (input, file) in &files {
        let result = match input.frontend {
            Frontend::Toml if !toml_read => {
                toml_read = true;
                mdl_toml::read(toml())
            }
            Frontend::Xtce if imports_read.insert(input.import) => {
                let options = imports.get(input.import).cloned().unwrap_or_default();
                mdl_xtce::read(import(input.import), &options)
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
