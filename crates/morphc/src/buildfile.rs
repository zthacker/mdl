//! Build files (`spec/build-files.md`): a project's definition inputs in
//! one file, each with its own import options, so one build can import
//! several XTCE systems as families of their own.
//!
//! ```toml
//! [[input]]
//! path    = "catalog"
//! catalog = "owner"
//!
//! [[input]]
//! path   = "vendors/demosat.xml"
//! family = "demosat"
//! alias  = { kind = "fixed", value = "DemoSat" }
//! ```

use std::fmt;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::Context;
use mdl::ir::{Locator, Scalar};
use serde::Deserialize;

/// The name morphc looks for when given no inputs, and leaves out when it
/// walks a directory of definitions.
pub const NAME: &str = "morphc.toml";

/// The format version this morphc reads.
const VERSION: i64 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    /// The format's version, 1 when left out: checked before the file is
    /// read as this struct, so only its place among the keys matters here.
    #[serde(rename = "version")]
    _version: Option<i64>,
    input: Vec<Entry>,
}

/// One `[[input]]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: PathBuf,
    family: Option<String>,
    join: Option<String>,
    set: Option<String>,
    set_version: Option<Scalar>,
    alias: Option<Locator>,
    naming: Option<crate::Naming>,
    catalog: Option<Catalog>,
}

/// What an input is to the build's catalog.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Catalog {
    /// Its definitions are the names every other input maps onto.
    Owner,
}

/// One input of a build file.
#[derive(Debug)]
pub struct Input {
    /// Its path, joined to the directory the build file really lives in.
    pub path: PathBuf,
    /// Its XTCE documents' import options.
    pub options: mdl_xtce::Options,
    /// Whether it gives any, which only XTCE documents can use.
    pub has_options: bool,
    /// Whether it owns the build's catalog (`catalog = "owner"`).
    pub owner: bool,
}

/// A build file that can't be read as one, with where in its text, for
/// morphc to report as it reports definitions.
#[derive(Debug)]
pub struct Invalid {
    pub file: PathBuf,
    pub text: String,
    pub message: String,
    pub span: Option<Range<usize>>,
}

impl fmt::Display for Invalid {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{} isn't a build file: {}",
            self.file.display(),
            self.message
        )
    }
}

impl std::error::Error for Invalid {}

/// The inputs `path` lists, in order. Relative paths are taken from the
/// directory the file really lives in, so a link to a build file reads the
/// same inputs as the file.
pub fn read(path: &Path) -> anyhow::Result<Vec<Input>> {
    if path.is_dir() {
        anyhow::bail!(
            "{} is a directory; -c takes a build file ({} in it, say)",
            path.display(),
            path.join(NAME).display()
        );
    }
    let text =
        fs::read_to_string(path).with_context(|| format!("can't read {}", path.display()))?;
    let invalid = |message: String, span: Option<Range<usize>>| Invalid {
        file: path.to_path_buf(),
        text: text.clone(),
        message,
        span,
    };
    // The version first, so a newer format is named as one rather than
    // refused key by key.
    let table: toml::Table =
        toml::from_str(&text).map_err(|err| invalid(err.message().to_string(), err.span()))?;
    match table.get("version") {
        None => {}
        Some(toml::Value::Integer(VERSION)) => {}
        Some(toml::Value::Integer(version)) => {
            return Err(invalid(
                format!("build file format {version}; this morphc reads format {VERSION}"),
                None,
            )
            .into());
        }
        Some(_) => {
            return Err(invalid("version is an integer, 1".to_string(), None).into());
        }
    }
    let file: File =
        toml::from_str(&text).map_err(|err| invalid(err.message().to_string(), err.span()))?;
    if file.input.is_empty() {
        return Err(invalid("it lists no inputs".to_string(), None).into());
    }
    let real = fs::canonicalize(path).with_context(|| format!("can't read {}", path.display()))?;
    let base = real.parent().unwrap_or(Path::new(""));
    let mut inputs = file
        .input
        .into_iter()
        .map(|entry| {
            if entry.path.as_os_str() == "-" {
                return Err(invalid(
                    "its inputs are files and directories, not stdin".to_string(),
                    None,
                )
                .into());
            }
            let shown = entry.path.display().to_string();
            if let Some(problem) = join_problem(&entry) {
                return Err(invalid(format!("input {shown} {problem}"), None).into());
            }
            let has_options = entry.family.is_some()
                || entry.join.is_some()
                || entry.set.is_some()
                || entry.set_version.is_some()
                || entry.alias.is_some()
                || entry.naming.is_some();
            let mut options = mdl_xtce::Options::new();
            if let Some(family) = entry.family {
                options = options.with_family(family);
            }
            if let Some(family) = entry.join {
                options = options.with_join(family);
            }
            if let Some(set) = entry.set {
                options = options.with_set(set);
            }
            if let Some(version) = entry.set_version {
                options = options.with_set_version(version);
            }
            if let Some(alias) = entry.alias {
                options = options.with_alias(alias);
            }
            if let Some(naming) = entry.naming {
                options = options.with_naming(naming.xtce());
            }
            Ok(Input {
                path: base.join(entry.path),
                options,
                has_options,
                owner: entry.catalog.is_some(),
            })
        })
        .collect::<anyhow::Result<Vec<Input>>>()?;
    // Owners close the catalog: the other inputs' XTCE documents map onto
    // its names rather than define their own.
    if inputs.iter().any(|input| input.owner) {
        for input in inputs.iter_mut().filter(|input| !input.owner) {
            input.options.catalog = mdl_xtce::CatalogRole::Refer;
        }
    }
    Ok(inputs)
}

/// Why an input's `join` can't be followed, or `set_version` given, as
/// the rest of the sentence that names the input.
fn join_problem(entry: &Entry) -> Option<String> {
    let Some(family) = &entry.join else {
        return entry.set_version.as_ref().map(|_| {
            "gives set_version, which is for a set that joins a family (join)".to_string()
        });
    };
    if let Some(own) = &entry.family {
        return Some(format!(
            "names family {own} and joins family {family}; give one"
        ));
    }
    if entry.catalog.is_some() {
        return Some(format!(
            "owns the catalog and joins family {family}: the inputs that refer to an owner's \
             catalog are read with it, before any input that joins a family, so an owner \
             can't join one"
        ));
    }
    if entry.set.is_none() {
        return Some(format!(
            "joins family {family}, so it names the set it adds to it (set)"
        ));
    }
    None
}
