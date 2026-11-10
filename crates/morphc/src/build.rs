//! `morphc build`: definitions to artifacts, written to a store.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::Args;
use mdl::artifact::{self, Artifacts, BuildError, Written};
use mdl::ir::Digest;

use crate::report::{Built, Report, Row};
use crate::store::{self, Found, Manifest, Stored};
use crate::{Given, ReadArgs, definitions, inputs};

#[derive(Debug, Args)]
pub struct BuildArgs {
    /// Write the artifacts to this store: `<digest>.json` for each, never
    /// rewritten, and `manifest.json` naming this build's. When the store
    /// has a manifest, the build is a rebuild of it.
    #[arg(short, long, value_name = "DIR")]
    out: Option<PathBuf>,

    /// Rebuild against this store's manifest instead of --out's: sets
    /// that haven't changed keep their identity (def_version), and the
    /// catalog must be compatible with the store's.
    #[arg(long, value_name = "DIR")]
    previous: Option<PathBuf>,

    /// Build without a previous store even when --out has a manifest, so
    /// every set gets a new identity (after a breaking catalog change).
    #[arg(long, requires = "out", conflicts_with = "previous")]
    fresh: bool,

    // Last, since the XTCE options' heading applies to every argument
    // after them.
    #[command(flatten)]
    pub read: ReadArgs,
}

pub fn run(args: &BuildArgs) -> anyhow::Result<ExitCode> {
    let mut given = Given::of(&args.read)?;
    if let Some(out) = &args.out
        && let Ok(out) = fs::canonicalize(out)
        && given
            .paths
            .iter()
            .any(|(input, _)| fs::canonicalize(input).is_ok_and(|input| input == out))
    {
        bail!(
            "--out {} is an input directory; put the store somewhere else, a store beside \
             the definitions is left out when they're read",
            out.display()
        );
    }
    let inputs = given.find()?;
    let sources = inputs::read(&inputs)?;
    let mut report = Report::new(&sources, args.read.format, args.read.all_warnings);
    for note in &given.notes {
        report.note(note.clone());
    }
    let Some((validated, map)) = definitions(&given.imports, &inputs, &sources, &mut report) else {
        return Ok(report.finish::<Built>(None));
    };

    let previous = previous_store(args)?;
    let built = match &previous {
        Some((_, stored)) => artifact::rebuild(&validated, &stored.artifacts),
        None => artifact::build(&validated),
    };
    let artifacts = match built {
        Ok(artifacts) => artifacts,
        Err(BuildError::IncompatibleCatalog(diagnostics)) => {
            report.add(map.locate_all(&diagnostics));
            if let Some((dir, _)) = &previous {
                report.note(format!(
                    "the catalog isn't compatible with the one in {}, so its sets can't keep \
                     their identities; --fresh builds new ones",
                    dir.display()
                ));
            }
            return Ok(report.finish::<Built>(None));
        }
        Err(err) => return Err(err).context("can't write the artifacts"),
    };
    let stored = previous.as_ref().map(|(_, stored)| stored);
    let replaced = replaced_manifest(args, &mut report)?;
    if let (Some(replaced), Some((dir, stored)), Some(out)) = (&replaced, &previous, &args.out)
        && *replaced != stored.manifest
    {
        report.note(format!(
            "the manifest in {} names another build; this one, continuing from {}, replaces it",
            out.display(),
            dir.display()
        ));
    }
    // Rows show what the store's manifest changes from, or else what the
    // build continues.
    let compared = replaced.or_else(|| stored.map(|stored| stored.manifest.clone()));
    let added = match &args.out {
        Some(out) => store::write(out, &artifacts, stored)?,
        None => Vec::new(),
    };

    let bundle = validated.bundle();
    let built = Built {
        mdl_version: bundle.mdl_version.to_string(),
        params: bundle.catalog.params.len(),
        commands: bundle.catalog.commands.len(),
        families: bundle.families.len(),
        sets: bundle.sets.len(),
        artifacts: rows(&artifacts, compared.as_ref(), &added),
        out: args.out.as_ref().map(|out| out.display().to_string()),
        written: added.iter().map(Digest::to_string).collect(),
    };
    Ok(report.finish(Some(built)))
}

/// The store to rebuild against: --previous's, or --out's when it has a
/// manifest and the build isn't --fresh.
fn previous_store(args: &BuildArgs) -> anyhow::Result<Option<(PathBuf, Stored)>> {
    let dir = match (&args.previous, &args.out) {
        (Some(dir), _) => dir,
        (None, Some(out)) if !args.fresh && store::has_manifest(out) => out,
        _ => return Ok(None),
    };
    let stored = store::load(dir).with_context(|| {
        let fresh = if args.previous.is_none() {
            " (--fresh builds without it)"
        } else {
            ""
        };
        format!(
            "can't read the store at {} to rebuild against it{fresh}",
            dir.display()
        )
    })?;
    Ok(Some((dir.clone(), stored)))
}

/// The manifest a build into --out replaces, read before it's replaced.
/// One of another format version isn't replaced; a damaged one is, with a
/// note.
fn replaced_manifest(args: &BuildArgs, report: &mut Report) -> anyhow::Result<Option<Manifest>> {
    let Some(out) = &args.out else {
        return Ok(None);
    };
    if !store::has_manifest(out) {
        return Ok(None);
    }
    match store::find_manifest(out) {
        Found::Manifest(manifest) => Ok(Some(manifest)),
        Found::OtherVersion(why) => bail!("{why}, so it won't replace that manifest"),
        Found::Unreadable(why) => {
            report.note(format!("{why}; this build replaces it"));
            Ok(None)
        }
    }
}

/// Each artifact, compared with the previous build's when there is one,
/// then what the previous build had and this one doesn't.
fn rows(artifacts: &Artifacts, previous: Option<&Manifest>, added: &[Digest]) -> Vec<Row> {
    let row = |kind, name: Option<&String>, now: &Digest, before: Option<&Digest>| Row {
        kind,
        name: name.cloned(),
        digest: now.to_string(),
        status: previous.map(|_| match before {
            None => "new",
            Some(before) if before == now => "unchanged",
            Some(_) => "changed",
        }),
        written: added.contains(now),
    };
    let mut rows = vec![row(
        "catalog",
        None,
        &artifacts.catalog.digest,
        previous.map(|p| &p.catalog),
    )];
    for (name, written) in &artifacts.families {
        let before = previous.and_then(|p| p.families.get(name));
        rows.push(row("family", Some(name), &written.digest, before));
    }
    for (name, written) in &artifacts.sets {
        let before = previous.and_then(|p| p.sets.get(name));
        rows.push(row("set", Some(name), &written.digest, before));
    }
    if let Some(previous) = previous {
        rows.extend(removed("family", &previous.families, &artifacts.families));
        rows.extend(removed("set", &previous.sets, &artifacts.sets));
    }
    rows
}

/// What the previous build named and this one doesn't.
fn removed(
    kind: &'static str,
    before: &BTreeMap<String, Digest>,
    now: &BTreeMap<String, Written>,
) -> impl Iterator<Item = Row> {
    before
        .iter()
        .filter(|(name, _)| !now.contains_key(*name))
        .map(move |(name, digest)| Row {
            kind,
            name: Some(name.clone()),
            digest: digest.to_string(),
            status: Some("removed"),
            written: false,
        })
}
