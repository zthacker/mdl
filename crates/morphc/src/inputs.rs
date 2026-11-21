//! Definition files: found on disk, read, and put through their frontends.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, bail};
use mdl::ir::{Catalog, Family};
use mdl::source::{Parsed, Severity, SourceDiagnostic, SourceFile, Sources};

use mdl::store;

use crate::buildfile;

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

    /// How diagnostics name it.
    pub fn name(&self) -> String {
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
/// Each path comes with the XTCE import its documents belong to, and
/// `owners` says which imports own the build's catalog. A file a catalog
/// owner names that an earlier input, not an owner, already reads is an
/// error, since it would be read as that input's, unowned.
pub fn find(paths: &[(PathBuf, usize)], owners: &[bool]) -> anyhow::Result<Vec<Input>> {
    let mut found = Found {
        paths,
        owners,
        inputs: Vec::new(),
        seen: BTreeMap::new(),
        stdin: false,
    };
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

struct Found<'a> {
    paths: &'a [(PathBuf, usize)],
    owners: &'a [bool],
    inputs: Vec<Input>,
    /// Each file's canonical path, so one named twice is read once, with
    /// the import that reads it.
    seen: BTreeMap<PathBuf, usize>,
    stdin: bool,
}

impl Found<'_> {
    fn add(&mut self, path: &Path, frontend: Frontend, import: usize) -> anyhow::Result<()> {
        let canonical =
            fs::canonicalize(path).with_context(|| format!("can't read {}", path.display()))?;
        let shown = shown(path, &canonical);
        if let Some(&first) = self.seen.get(&canonical) {
            let owner = |import: usize| self.owners.get(import).copied().unwrap_or(false);
            if owner(import) && !owner(first) {
                let input = self
                    .paths
                    .iter()
                    .find(|(_, index)| *index == first)
                    .map_or_else(String::new, |(path, _)| shown_path(path));
                bail!(
                    "{shown} is read as part of input {input}, listed before the catalog owner \
                     that includes it, so it wouldn't be owned; list the owner first"
                );
            }
            return Ok(());
        }
        self.seen.insert(canonical, import);
        self.inputs.push(Input {
            path: path.to_path_buf(),
            frontend,
            import,
            shown,
        });
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

/// Why the definitions couldn't be read: every problem and warning, in
/// file order, and the XTCE imports left unread, with why.
pub struct Unread {
    pub problems: Vec<SourceDiagnostic>,
    pub skipped: Vec<(usize, Skip)>,
}

/// Why an XTCE import wasn't read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// It refers to the catalog, and what read doesn't merge into one.
    Catalog,
    /// It joins a family no input that read defines, and an input that
    /// didn't read might.
    Family,
}

/// One bundle per frontend in the order of its first file: the TOML files
/// as one, each XTCE import's documents as one (with `imports[i]` as
/// import `i`'s options), and each JSON bundle alone.
///
/// An XTCE import that refers to the catalog or joins a family is read
/// after what it's read with: one that refers to the catalog once the
/// others are read, with the catalog of those that read (only an owner
/// defines names, and an owner joins no family); one that joins a family
/// after that, with every family read so far. One that joins a family no
/// input that read defines waits while an input has problems, rather than
/// report a family that input may define; one that refers to a catalog
/// that doesn't merge waits too. Each that waits is in
/// [`Unread::skipped`].
pub fn parse(
    sources: &Sources,
    inputs: &[Input],
    imports: &[mdl_xtce::Options],
) -> Result<Vec<Parsed>, Unread> {
    let files: Vec<(&Input, SourceFile<'_>)> = inputs.iter().zip(sources.iter()).collect();
    let toml = || {
        files
            .iter()
            .filter(|(input, _)| input.frontend == Frontend::Toml)
            .map(|(_, file)| *file)
    };
    let import_files = |index: usize| {
        files
            .iter()
            .filter(move |(input, _)| input.frontend == Frontend::Xtce && input.import == index)
            .map(|(_, file)| *file)
    };
    let options = |index: usize| imports.get(index).cloned().unwrap_or_default();
    let mut read = Read::default();
    let mut toml_read = false;
    let mut xtce = Vec::new();
    let mut imports_seen = BTreeSet::new();
    for (place, (input, file)) in files.iter().enumerate() {
        match input.frontend {
            Frontend::Toml if !toml_read => {
                toml_read = true;
                read.add(place, mdl_toml::read(toml()));
            }
            Frontend::Json => read.add(place, mdl::json::read(*file)),
            Frontend::Xtce if imports_seen.insert(input.import) => xtce.push((place, input.import)),
            Frontend::Toml | Frontend::Xtce => {}
        }
    }
    // What an import waits for: nothing, the catalog, or a family.
    let stage = |import: usize| {
        let options = options(import);
        match (options.join.is_some(), refers(&options)) {
            (true, _) => 2,
            (false, true) => 1,
            (false, false) => 0,
        }
    };
    let mut skipped = Vec::new();
    let mut merged: Option<(usize, Option<Catalog>)> = None;
    for waits in 0..=2 {
        let due: Vec<(usize, usize)> = xtce
            .iter()
            .copied()
            .filter(|(_, import)| stage(*import) == waits)
            .collect();
        if due.is_empty() {
            continue;
        }
        // Whether an input read so far has problems: one may define the
        // family a joining import looks for.
        let failed_before = read.failed;
        read.parts.sort_by_key(|(place, _)| *place);
        // The catalog of what read, which a referring import's own
        // problems don't depend on: what it writes with it is used only
        // when everything reads. It's merged once for each set of parts
        // (parts are only added), so a merge that fails is reported once.
        let catalog = if due.iter().any(|(_, import)| refers(&options(*import))) {
            match &merged {
                Some((parts, catalog)) if *parts == read.parts.len() => catalog.clone(),
                _ => {
                    let catalog = read.catalog();
                    merged = Some((read.parts.len(), catalog.clone()));
                    catalog
                }
            }
        } else {
            None
        };
        let families: Vec<Family> = read
            .parts
            .iter()
            .flat_map(|(_, parsed)| parsed.bundle.families.iter().cloned())
            .collect();
        let mut context = mdl_xtce::Context::new().with_families(&families);
        if let Some(catalog) = &catalog {
            context = context.with_catalog(catalog);
        }
        for (place, import) in due {
            let options = options(import);
            if refers(&options) && catalog.is_none() {
                skipped.push((import, Skip::Catalog));
                continue;
            }
            let missing = options
                .join
                .as_ref()
                .is_some_and(|family| !families.iter().any(|known| known.name == *family));
            if missing && failed_before {
                skipped.push((import, Skip::Family));
                continue;
            }
            read.add(
                place,
                mdl_xtce::read_with(import_files(import), &options, context),
            );
        }
    }
    let Read {
        mut parts,
        mut problems,
        failed,
    } = read;
    parts.sort_by_key(|(place, _)| *place);
    if !failed {
        return Ok(parts.into_iter().map(|(_, parsed)| parsed).collect());
    }
    for (_, part) in &parts {
        problems.extend(part.warnings.iter().cloned());
    }
    problems.sort_by_key(|d| d.span.map(|span| (span.source, span.start)));
    Err(Unread { problems, skipped })
}

/// The bundles read so far, each with the place of its first file, which
/// orders the merge; and the problems of those that didn't read.
#[derive(Default)]
struct Read {
    parts: Vec<(usize, Parsed)>,
    problems: Vec<SourceDiagnostic>,
    failed: bool,
}

impl Read {
    /// The catalog of the parts read, merged in file order: an empty one
    /// when none did, and none when they don't merge, whose errors are
    /// then the problems' (the parts' warnings are reported with them).
    fn catalog(&mut self) -> Option<Catalog> {
        if self.parts.is_empty() {
            return Some(Catalog::default());
        }
        self.parts.sort_by_key(|(place, _)| *place);
        match mdl::source::merge(self.parts.iter().map(|(_, parsed)| parsed.clone())) {
            Ok(merged) => Some(merged.bundle.catalog),
            Err(diagnostics) => {
                self.failed = true;
                self.problems.extend(
                    diagnostics
                        .into_iter()
                        .filter(|d| d.severity == Severity::Error),
                );
                None
            }
        }
    }

    fn add(&mut self, place: usize, result: Result<Parsed, Vec<SourceDiagnostic>>) {
        match result {
            Ok(parsed) => self.parts.push((place, parsed)),
            Err(diagnostics) => {
                self.failed = true;
                self.problems.extend(diagnostics);
            }
        }
    }
}

fn refers(options: &mdl_xtce::Options) -> bool {
    options.catalog == mdl_xtce::CatalogRole::Refer
}
