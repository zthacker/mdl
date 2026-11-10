//! An artifact store (spec/store.md): a directory holding every artifact
//! as `<digest>.json`, its canonical bytes, and `manifest.json`, naming
//! the latest build's catalog, families and sets by digest.
//!
//! Artifacts are never rewritten or removed: a set kept across rebuilds
//! still pins the catalog it was first built against, and consumers
//! holding an older digest can still read it. Each file is written whole
//! (a temporary file, synced, then renamed), artifacts before the
//! manifest, so a store always names artifacts it has. morphc takes no
//! lock: a store has one writer at a time.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, anyhow, bail};
use mdl::artifact::{Artifacts, Written};
use mdl::ir::{Artifact, Digest};
use mdl::read::{Loaded, read_artifact};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MANIFEST: &str = "manifest.json";

/// What `manifest.json` holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The manifest format's version.
    pub manifest: u64,
    pub catalog: Digest,
    pub families: BTreeMap<String, Digest>,
    pub sets: BTreeMap<String, Digest>,
}

impl Manifest {
    pub const VERSION: u64 = 1;

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

/// What a store's `manifest.json` turned out to be.
pub enum Found {
    Manifest(Manifest),
    /// A manifest of a format version this morphc doesn't read, which it
    /// mustn't replace either.
    OtherVersion(String),
    /// Not a manifest this morphc can make sense of: why.
    Unreadable(String),
}

pub fn has_manifest(dir: &Path) -> bool {
    dir.join(MANIFEST).is_file()
}

/// What `dir`'s manifest holds. The version is read first, so a newer
/// format's manifest is told apart from a damaged one.
pub fn find_manifest(dir: &Path) -> Found {
    let path = dir.join(MANIFEST);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => return Found::Unreadable(format!("can't read {}: {err}", path.display())),
    };
    let value: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(err) => return Found::Unreadable(format!("{} isn't JSON: {err}", path.display())),
    };
    match value.get("manifest").and_then(Value::as_u64) {
        Some(Manifest::VERSION) => {}
        Some(version) => {
            return Found::OtherVersion(format!(
                "{} is manifest version {version}, but this morphc reads version {}",
                path.display(),
                Manifest::VERSION
            ));
        }
        None => {
            return Found::Unreadable(format!(
                "{} has no manifest version; it isn't a store manifest",
                path.display()
            ));
        }
    }
    match serde_json::from_value(value) {
        Ok(manifest) => Found::Manifest(manifest),
        Err(err) => Found::Unreadable(format!("{} isn't a store manifest: {err}", path.display())),
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
pub struct Stored {
    pub manifest: Manifest,
    pub artifacts: Artifacts,
    /// The canonical text of every catalog read: the latest and each
    /// older one a set pins.
    catalogs: BTreeMap<Digest, String>,
}

impl Stored {
    /// The canonical text of every catalog read: the latest, and each
    /// older one a set pins.
    pub fn catalogs(&self) -> impl Iterator<Item = &str> {
        self.catalogs.values().map(String::as_str)
    }
}

/// The build `dir`'s manifest names, each artifact checked against the
/// digest it's stored under, with the older catalogs its sets pin.
pub fn load(dir: &Path) -> anyhow::Result<Stored> {
    let path = dir.join(MANIFEST);
    let manifest = match find_manifest(dir) {
        Found::Manifest(manifest) => manifest,
        Found::OtherVersion(why) | Found::Unreadable(why) => bail!(why),
    };
    let catalog = artifact(dir, &manifest.catalog)?;
    let mut catalogs = BTreeMap::new();
    catalogs.insert(catalog.digest.clone(), catalog.canonical.clone());
    let mut others = Vec::new();
    let mut pinned = BTreeSet::new();
    for (name, digest) in &manifest.families {
        let loaded = artifact(dir, digest)?;
        match &loaded.value {
            Artifact::Family { family, .. } if family.name == *name => {}
            _ => bail!("{}: artifact {digest} isn't family {name}", path.display()),
        }
        others.push(loaded);
    }
    for (name, digest) in &manifest.sets {
        let loaded = artifact(dir, digest)?;
        match &loaded.value {
            Artifact::Set { set, catalog, .. } if set.name == *name => {
                if *catalog != manifest.catalog {
                    pinned.insert(catalog.clone());
                }
            }
            _ => bail!("{}: artifact {digest} isn't set {name}", path.display()),
        }
        others.push(loaded);
    }
    for digest in pinned {
        let loaded = artifact(dir, &digest)?;
        catalogs.insert(digest, loaded.canonical.clone());
        others.push(loaded);
    }
    let artifacts = Artifacts::from_loaded(catalog, others)
        .with_context(|| format!("the store at {} doesn't fit together", dir.display()))?;
    Ok(Stored {
        manifest,
        artifacts,
        catalogs,
    })
}

/// The artifact stored under `digest`, which must be its digest.
fn artifact(dir: &Path, digest: &Digest) -> anyhow::Result<Loaded<Artifact>> {
    let path = dir.join(format!("{digest}.json"));
    let bytes = fs::read(&path).with_context(|| format!("can't read {}", path.display()))?;
    let loaded = read_artifact(&bytes)
        .with_context(|| format!("{} isn't an MDL artifact; {}", path.display(), RECOVER))?;
    if loaded.digest != *digest {
        bail!(
            "{} holds artifact {}, not the one its name says; {}",
            path.display(),
            loaded.digest,
            RECOVER
        );
    }
    Ok(loaded)
}

/// What to do about a damaged artifact: its content comes from the
/// definitions, so building again rewrites it.
const RECOVER: &str = "the store is damaged: delete the file and build again (with --fresh if \
                       the manifest names it)";

/// Write `artifacts` to the store at `dir`, with every older catalog a
/// kept set pins (from `previous`), then the manifest naming them. The
/// digests of the artifacts the store didn't have yet, in order.
pub fn write(
    dir: &Path,
    artifacts: &Artifacts,
    previous: Option<&Stored>,
) -> anyhow::Result<Vec<Digest>> {
    fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
    let mut files: BTreeMap<&Digest, &str> = BTreeMap::new();
    files.insert(&artifacts.catalog.digest, &artifacts.catalog.canonical);
    for written in artifacts.families.values().chain(artifacts.sets.values()) {
        files.insert(&written.digest, &written.canonical);
        if let Artifact::Set { catalog, .. } = &written.artifact
            && *catalog != artifacts.catalog.digest
        {
            let text = previous
                .and_then(|stored| stored.catalogs.get(catalog))
                .ok_or_else(|| anyhow!("a set pins catalog {catalog}, which wasn't read"))?;
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
    let mut manifest = serde_json::to_string_pretty(&Manifest::of(artifacts))
        .context("can't write the manifest as JSON")?;
    manifest.push('\n');
    replace(dir, MANIFEST, &manifest)?;
    sync_dir(dir)?;
    Ok(added)
}

/// Store one artifact unless the store has it; true if it was written.
fn put(dir: &Path, digest: &Digest, canonical: &str) -> anyhow::Result<bool> {
    let name = format!("{digest}.json");
    let path = dir.join(&name);
    match fs::read(&path) {
        Ok(bytes) => {
            let holds = bytes == canonical.as_bytes()
                || read_artifact(&bytes).is_ok_and(|loaded| loaded.digest == *digest);
            if holds {
                Ok(false)
            } else {
                bail!(
                    "{} doesn't hold the artifact its name says; {}",
                    path.display(),
                    RECOVER
                )
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            replace(dir, &name, canonical)?;
            Ok(true)
        }
        Err(err) => Err(err).with_context(|| format!("can't read {}", path.display())),
    }
}

/// Write `name` in `dir` whole or not at all: a temporary file of its
/// own, synced, then renamed over `name`.
fn replace(dir: &Path, name: &str, text: &str) -> anyhow::Result<()> {
    let target = dir.join(name);
    let (temporary, file) = temporary(dir, name)
        .with_context(|| format!("can't create a temporary file in {}", dir.display()))?;
    let written = write_synced(file, text).and_then(|()| fs::rename(&temporary, &target));
    if let Err(err) = written {
        fs::remove_file(&temporary).ok();
        return Err(err).with_context(|| format!("can't write {}", target.display()));
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
fn sync_dir(dir: &Path) -> anyhow::Result<()> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(|| format!("can't sync {}", dir.display()))
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> anyhow::Result<()> {
    Ok(())
}
