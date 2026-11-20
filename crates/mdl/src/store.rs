//! Artifact stores (`spec/store.md`): a directory holding every artifact
//! as `<digest>.json`, its canonical bytes, and `manifest.json`, naming
//! the latest build's catalog, families and sets by digest.
//!
//! Artifacts are never rewritten or removed: a set kept across rebuilds
//! still pins the catalog it was first built against, and consumers
//! holding an older digest can still read it. Each file is written whole
//! (a temporary file, synced, then renamed), artifacts before the
//! manifest, so a store always names artifacts it has. Writers take no
//! lock: a store has one writer at a time.
//!
//! A store is read from its directory ([`load`]) or from its files as
//! bytes ([`load_bytes`]), with the same result either way.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::artifact::{Artifacts, AssembleError, Written};
use crate::ir::{Artifact, Digest};
use crate::read::{Loaded, ReadError, identity, read_artifact};

pub const MANIFEST: &str = "manifest.json";

/// What `manifest.json` holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Manifest {
    /// The manifest format's version.
    pub manifest: u64,
    pub catalog: Digest,
    pub families: BTreeMap<String, Digest>,
    pub sets: BTreeMap<String, Digest>,
}

impl Manifest {
    pub const VERSION: u64 = 1;

    /// The manifest naming `artifacts`.
    pub fn of(artifacts: &Artifacts) -> Manifest {
        let digests = |written: &BTreeMap<String, Written>| {
            written
                .iter()
                .map(|(name, w)| (name.clone(), w.digest.clone()))
                .collect()
        };
        Manifest {
            manifest: Manifest::VERSION,
            catalog: artifacts.catalog.digest.clone(),
            families: digests(&artifacts.families),
            sets: digests(&artifacts.sets),
        }
    }
}

/// Why a store can't be read or written. [`StoreError::code`] gives the
/// stable code `spec/api.md` §4 lists.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum StoreError {
    /// A file can't be read, or isn't there.
    #[error("can't read {file}: {detail}")]
    Unreadable { file: String, detail: String },
    #[error("{file} is manifest version {version}, but this MDL reads version {reads}")]
    ManifestVersion {
        file: String,
        version: u64,
        reads: u64,
    },
    #[error("{file} isn't a store manifest: {detail}")]
    BadManifest { file: String, detail: String },
    /// An artifact that doesn't read: damaged, or of a version this MDL
    /// doesn't read.
    #[error("{}", artifact_message(file, error))]
    Artifact { file: String, error: ReadError },
    #[error("{file} holds artifact {holds}, not the one its name says; {RECOVER}")]
    WrongDigest { file: String, holds: Digest },
    /// An artifact the manifest names as a family or set that isn't the
    /// one of that name.
    #[error("{file}: artifact {digest} isn't {what} {name}")]
    WrongArtifact {
        file: String,
        digest: Digest,
        what: &'static str,
        name: String,
    },
    /// The artifacts don't fit together as one build.
    #[error("the store at {store} doesn't fit together: {error}")]
    Assemble { store: String, error: AssembleError },
    #[error("can't write {file}: {detail}")]
    Unwritable { file: String, detail: String },
}

impl StoreError {
    pub fn code(&self) -> &'static str {
        match self {
            StoreError::Unreadable { .. } => "store_unreadable",
            StoreError::ManifestVersion { .. } => "manifest_version",
            StoreError::BadManifest { .. } => "bad_manifest",
            StoreError::Artifact { error, .. } => error.code(),
            StoreError::WrongDigest { .. } => "wrong_digest",
            StoreError::WrongArtifact { .. } => "wrong_artifact",
            StoreError::Assemble { error, .. } => error.code(),
            StoreError::Unwritable { .. } => "store_unwritable",
        }
    }
}

/// What to do about a damaged artifact: its content comes from the
/// definitions, so building again rewrites it.
const RECOVER: &str = "the store is damaged: delete the file and build again";

/// An artifact that doesn't read. Only damage gets [`RECOVER`]'s advice:
/// an artifact of a version this MDL doesn't read is sound, and deleting
/// it from a shared store would lose it.
fn artifact_message(file: &str, error: &ReadError) -> String {
    match error.code() {
        "unsupported_version" | "feature_version" => {
            format!("{file} is an artifact this MDL can't read: {error}")
        }
        _ => format!("{file} isn't an MDL artifact; {RECOVER}: {error}"),
    }
}

/// What a store's `manifest.json` turned out to be.
pub enum Found {
    Manifest(Manifest),
    /// A manifest of a format version this MDL doesn't read, which a writer
    /// mustn't replace either.
    OtherVersion(StoreError),
    /// Not a manifest this MDL can make sense of.
    Unreadable(StoreError),
}

pub fn has_manifest(dir: &Path) -> bool {
    dir.join(MANIFEST).is_file()
}

/// What `dir`'s manifest holds. The version is read first, so a newer
/// format's manifest is told apart from a damaged one.
pub fn find_manifest(dir: &Path) -> Found {
    let path = dir.join(MANIFEST);
    let file = path.display().to_string();
    match fs::read(&path) {
        Ok(bytes) => read_manifest(&bytes, &file),
        Err(err) => Found::Unreadable(StoreError::Unreadable {
            file,
            detail: err.to_string(),
        }),
    }
}

/// A manifest's bytes, named `file` in messages.
fn read_manifest(bytes: &[u8], file: &str) -> Found {
    let bad = |detail: String| {
        Found::Unreadable(StoreError::BadManifest {
            file: file.to_string(),
            detail,
        })
    };
    let value: Value = match serde_json::from_slice(bytes) {
        Ok(value) => value,
        Err(err) => return bad(format!("it isn't JSON: {err}")),
    };
    match value.get("manifest").and_then(Value::as_u64) {
        Some(Manifest::VERSION) => {}
        Some(version) => {
            return Found::OtherVersion(StoreError::ManifestVersion {
                file: file.to_string(),
                version,
                reads: Manifest::VERSION,
            });
        }
        None => return bad("it has no manifest version".to_string()),
    }
    match serde_json::from_value(value) {
        Ok(manifest) => Found::Manifest(manifest),
        Err(err) => bad(err.to_string()),
    }
}

/// Is `dir` an artifact store: a manifest that says it's one, or only
/// artifacts named by their digests (a store whose first build stopped
/// before its manifest)?
pub fn is_store(dir: &Path) -> bool {
    match find_manifest(dir) {
        Found::Manifest(_) | Found::OtherVersion(_) => return true,
        Found::Unreadable(_) => {}
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    let mut artifacts = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == MANIFEST {
            continue;
        }
        let Some(stem) = name.strip_suffix(".json") else {
            continue;
        };
        if stem.parse::<Digest>().is_err() {
            return false;
        }
        artifacts += 1;
    }
    artifacts > 0
}

/// A store's latest build, read back.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Stored {
    pub manifest: Manifest,
    pub artifacts: Artifacts,
    /// The canonical text of every catalog read: the latest and each
    /// older one a set pins.
    catalogs: BTreeMap<Digest, String>,
}

impl Stored {
    /// A build made in memory, as a store holding only it would read: its
    /// one catalog, and the manifest naming it.
    pub fn of(artifacts: Artifacts) -> Stored {
        let catalogs = BTreeMap::from([(
            artifacts.catalog.digest.clone(),
            artifacts.catalog.canonical.clone(),
        )]);
        Stored {
            manifest: Manifest::of(&artifacts),
            artifacts,
            catalogs,
        }
    }

    /// The canonical text of every catalog read: the latest, and each
    /// older one a set pins.
    pub fn catalogs(&self) -> impl Iterator<Item = &str> {
        self.catalogs.values().map(String::as_str)
    }
}

/// The build `dir`'s manifest names, each artifact checked against the
/// digest it's stored under, with the older catalogs its sets pin.
pub fn load(dir: &Path) -> Result<Stored, StoreError> {
    let manifest = match find_manifest(dir) {
        Found::Manifest(manifest) => manifest,
        Found::OtherVersion(error) | Found::Unreadable(error) => return Err(error),
    };
    assemble(
        manifest,
        &dir.join(MANIFEST).display().to_string(),
        |digest| {
            let path = dir.join(format!("{digest}.json"));
            let file = path.display().to_string();
            match fs::read(&path) {
                Ok(bytes) => Ok((bytes, file)),
                Err(err) => Err(StoreError::Unreadable {
                    file,
                    detail: err.to_string(),
                }),
            }
        },
    )
}

/// The build a manifest names, from its bytes and the artifacts' bytes,
/// each artifact found by its own identity, as if they were the files of
/// a store: [`load`] of a directory holding them reads the same build. As
/// a directory's other files are, artifacts the manifest doesn't need are
/// never read (one of a newer version, say), and bytes with no identity
/// aren't one of the build's.
pub fn load_bytes(manifest: &[u8], artifacts: &[&[u8]]) -> Result<Stored, StoreError> {
    let manifest = match read_manifest(manifest, MANIFEST) {
        Found::Manifest(manifest) => manifest,
        Found::OtherVersion(error) | Found::Unreadable(error) => return Err(error),
    };
    let given: BTreeMap<Digest, &[u8]> = artifacts
        .iter()
        .filter_map(|bytes| Some((identity(bytes)?, *bytes)))
        .collect();
    assemble(manifest, MANIFEST, |digest| match given.get(digest) {
        Some(bytes) => Ok((bytes.to_vec(), format!("artifact {digest}"))),
        None => Err(StoreError::Unreadable {
            file: format!("artifact {digest}"),
            detail: "it isn't among the artifacts given".to_string(),
        }),
    })
}

/// The build `manifest` names, each artifact fetched by digest as bytes
/// and the name messages give it.
fn assemble(
    manifest: Manifest,
    manifest_file: &str,
    fetch: impl Fn(&Digest) -> Result<(Vec<u8>, String), StoreError>,
) -> Result<Stored, StoreError> {
    let artifact = |digest: &Digest| -> Result<Loaded<Artifact>, StoreError> {
        let (bytes, file) = fetch(digest)?;
        let loaded = read_artifact(&bytes).map_err(|error| StoreError::Artifact {
            file: file.clone(),
            error,
        })?;
        if loaded.digest != *digest {
            return Err(StoreError::WrongDigest {
                file,
                holds: loaded.digest,
            });
        }
        Ok(loaded)
    };
    let wrong = |digest: &Digest, what: &'static str, name: &str| StoreError::WrongArtifact {
        file: manifest_file.to_string(),
        digest: digest.clone(),
        what,
        name: name.to_string(),
    };
    let catalog = artifact(&manifest.catalog)?;
    let mut catalogs = BTreeMap::new();
    catalogs.insert(catalog.digest.clone(), catalog.canonical.clone());
    let mut others = Vec::new();
    let mut pinned = BTreeSet::new();
    for (name, digest) in &manifest.families {
        let loaded = artifact(digest)?;
        match &loaded.value {
            Artifact::Family { family, .. } if family.name == *name => {}
            _ => return Err(wrong(digest, "family", name)),
        }
        others.push(loaded);
    }
    for (name, digest) in &manifest.sets {
        let loaded = artifact(digest)?;
        match &loaded.value {
            Artifact::Set { set, catalog, .. } if set.name == *name => {
                if *catalog != manifest.catalog {
                    pinned.insert(catalog.clone());
                }
            }
            _ => return Err(wrong(digest, "set", name)),
        }
        others.push(loaded);
    }
    for digest in pinned {
        let loaded = artifact(&digest)?;
        catalogs.insert(digest, loaded.canonical.clone());
        others.push(loaded);
    }
    let artifacts =
        Artifacts::from_loaded(catalog, others).map_err(|error| StoreError::Assemble {
            store: manifest_file.to_string(),
            error,
        })?;
    Ok(Stored {
        manifest,
        artifacts,
        catalogs,
    })
}

/// Write `artifacts` to the store at `dir`, with every older catalog a
/// kept set pins (from `previous`), then the manifest naming them. The
/// digests of the artifacts the store didn't have yet, in order.
pub fn write(
    dir: &Path,
    artifacts: &Artifacts,
    previous: Option<&Stored>,
) -> Result<Vec<Digest>, StoreError> {
    let unwritable = |path: &Path, err: io::Error| StoreError::Unwritable {
        file: path.display().to_string(),
        detail: err.to_string(),
    };
    fs::create_dir_all(dir).map_err(|err| unwritable(dir, err))?;
    let mut files: BTreeMap<&Digest, &str> = BTreeMap::new();
    files.insert(&artifacts.catalog.digest, &artifacts.catalog.canonical);
    for written in artifacts.families.values().chain(artifacts.sets.values()) {
        files.insert(&written.digest, &written.canonical);
        if let Artifact::Set { catalog, .. } = &written.artifact
            && *catalog != artifacts.catalog.digest
        {
            let text = previous
                .and_then(|stored| stored.catalogs.get(catalog))
                .ok_or_else(|| StoreError::Unreadable {
                    file: format!("{catalog}.json"),
                    detail: "a set pins this catalog, which the previous build didn't read"
                        .to_string(),
                })?;
            files.insert(catalog, text);
        }
    }
    let mut added = Vec::new();
    for (digest, canonical) in files {
        if put(dir, digest, canonical)? {
            added.push(digest.clone());
        }
    }
    sync_dir(dir)?;
    let mut manifest = serde_json::to_string_pretty(&Manifest::of(artifacts)).map_err(|err| {
        StoreError::Unwritable {
            file: dir.join(MANIFEST).display().to_string(),
            detail: err.to_string(),
        }
    })?;
    manifest.push('\n');
    replace(dir, MANIFEST, &manifest)?;
    sync_dir(dir)?;
    Ok(added)
}

/// Store one artifact unless the store has it; true if it was written.
fn put(dir: &Path, digest: &Digest, canonical: &str) -> Result<bool, StoreError> {
    let name = format!("{digest}.json");
    let path = dir.join(&name);
    match fs::read(&path) {
        Ok(bytes) => {
            if bytes == canonical.as_bytes() {
                return Ok(false);
            }
            match read_artifact(&bytes) {
                Ok(loaded) if loaded.digest == *digest => Ok(false),
                Ok(loaded) => Err(StoreError::WrongDigest {
                    file: path.display().to_string(),
                    holds: loaded.digest,
                }),
                Err(error) => Err(StoreError::Artifact {
                    file: path.display().to_string(),
                    error,
                }),
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            replace(dir, &name, canonical)?;
            Ok(true)
        }
        Err(err) => Err(StoreError::Unreadable {
            file: path.display().to_string(),
            detail: err.to_string(),
        }),
    }
}

/// Write `name` in `dir` whole or not at all: a temporary file of its
/// own, synced, then renamed over `name`.
fn replace(dir: &Path, name: &str, text: &str) -> Result<(), StoreError> {
    let target = dir.join(name);
    let (temporary, file) = temporary(dir, name).map_err(|err| StoreError::Unwritable {
        file: dir.display().to_string(),
        detail: format!("can't create a temporary file: {err}"),
    })?;
    let written = write_synced(file, text).and_then(|()| fs::rename(&temporary, &target));
    if let Err(err) = written {
        fs::remove_file(&temporary).ok();
        return Err(StoreError::Unwritable {
            file: target.display().to_string(),
            detail: err.to_string(),
        });
    }
    Ok(())
}

/// A new hidden file beside `name`, created only if no file has its name,
/// so two writers (even on two hosts sharing the store) never share one.
fn temporary(dir: &Path, name: &str) -> io::Result<(PathBuf, File)> {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    loop {
        let count = COUNT.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!(
            ".{name}.{}-{nanos}-{count}.tmp",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
}

fn write_synced(mut file: File, text: &str) -> io::Result<()> {
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

/// Make renames in `dir` durable, so the manifest is never on disk
/// before the artifacts it names. Windows has no way to sync a
/// directory; there the order rests on the file system.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<(), StoreError> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|err| StoreError::Unwritable {
            file: dir.display().to_string(),
            detail: format!("can't sync it: {err}"),
        })
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<(), StoreError> {
    Ok(())
}
