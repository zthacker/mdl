//! morphc: the MDL compiler.
//!
//! ```text
//! morphc build catalog.toml vendor.xml           read, merge, validate
//! morphc build definitions/ --out store/         ... and write artifacts
//! frontend | morphc build -                      an IR bundle on stdin
//! ```
//!
//! Exit codes: 0 the definitions built, 1 they have problems (every one is
//! reported), 2 morphc couldn't run (unreadable input, a damaged store).

mod inputs;
mod report;
mod store;

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use mdl::v2::artifact::{self, Artifacts, BuildError, Written};
use mdl::v2::ir::{Digest, Locator};
use mdl::v2::source::{Parsed, merge};
use mdl::v2::validate::validate;

use crate::inputs::Frontend;
use crate::report::{Built, Format, Report, Row};
use crate::store::{Found, Manifest, Stored};

#[derive(Debug, Parser)]
#[command(
    name = "morphc",
    version,
    about = "The MDL (Morpheus Definition Language) compiler"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Read definitions with any frontend, merge and validate them, and
    /// with --out write their artifacts to a store.
    Build(BuildArgs),
}

#[derive(Debug, Args)]
struct BuildArgs {
    /// Definition files and directories: .toml (MDL TOML), .xml (XTCE)
    /// and .json (an MDL IR bundle); `-` reads a bundle from stdin.
    /// Bundles merge in the order of their first file, earlier ones taking
    /// precedence. A directory gives its TOML files, then its JSON, then
    /// its XTCE, each kind in name order.
    #[arg(required = true, value_name = "INPUT")]
    inputs: Vec<PathBuf>,

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

    /// Show every warning, not just the first of each code.
    #[arg(long)]
    all_warnings: bool,

    /// How to report: for people on stderr and stdout, or as one JSON
    /// document on stdout.
    #[arg(long, value_enum, default_value_t = Format::Human)]
    format: Format,

    // Last, since its heading applies to every argument after it.
    #[command(flatten)]
    xtce: XtceArgs,
}

#[derive(Debug, Args)]
#[command(next_help_heading = "XTCE import")]
struct XtceArgs {
    /// The family's name [default: the root SpaceSystem's, lowercased]
    #[arg(long, value_name = "NAME")]
    family: Option<String>,

    /// The set's name [default: the family's]
    #[arg(long, value_name = "NAME")]
    set: Option<String>,

    /// Where an asset's own id is: `fixed:<id>`, `metadata:<name>`,
    /// `path:<pointer>` or `bits:<offset>:<size>` [default: fixed, the
    /// root SpaceSystem's name]
    #[arg(long, value_name = "LOCATOR", value_parser = parse_alias)]
    alias: Option<Locator>,

    /// How packet, field and type names are made from XTCE names
    #[arg(long = "xtce-naming", value_enum)]
    naming: Option<Naming>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Naming {
    /// As XTCE has them, qualified by SpaceSystem only where two share one
    Short,
    /// Always qualified by SpaceSystem, so names don't change as
    /// documents grow
    Qualified,
}

impl XtceArgs {
    fn given(&self) -> bool {
        self.family.is_some() || self.set.is_some() || self.alias.is_some() || self.naming.is_some()
    }

    fn options(&self) -> mdl_xtce::Options {
        let mut options = mdl_xtce::Options::new();
        if let Some(family) = &self.family {
            options = options.with_family(family);
        }
        if let Some(set) = &self.set {
            options = options.with_set(set);
        }
        if let Some(alias) = &self.alias {
            options = options.with_alias(alias.clone());
        }
        if let Some(naming) = self.naming {
            options = options.with_naming(match naming {
                Naming::Short => mdl_xtce::Naming::Short,
                Naming::Qualified => mdl_xtce::Naming::Qualified,
            });
        }
        options
    }
}

fn parse_alias(text: &str) -> Result<Locator, String> {
    let usage = || {
        format!(
            "{text:?} isn't a locator: give fixed:<id>, metadata:<name>, path:<pointer> or \
             bits:<offset>:<size>"
        )
    };
    let (kind, rest) = text.split_once(':').ok_or_else(usage)?;
    match kind {
        "fixed" => Ok(Locator::fixed(rest)),
        "metadata" => Ok(Locator::metadata(rest)),
        "path" => Ok(Locator::path(rest)),
        "bits" => {
            let (offset, size) = rest.split_once(':').ok_or_else(usage)?;
            let offset = offset.parse().map_err(|_| usage())?;
            let size = size.parse().map_err(|_| usage())?;
            Ok(Locator::bits(offset, size))
        }
        _ => Err(usage()),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let (format, result) = match &cli.command {
        Command::Build(args) => (args.format, build(args)),
    };
    match result {
        Ok(code) => code,
        Err(err) => report::failed(format, &err),
    }
}

fn build(args: &BuildArgs) -> anyhow::Result<ExitCode> {
    if let Some(out) = &args.out
        && let Ok(out) = fs::canonicalize(out)
        && args
            .inputs
            .iter()
            .any(|input| fs::canonicalize(input).is_ok_and(|input| input == out))
    {
        bail!(
            "--out {} is an input directory; put the store somewhere else, a store beside \
             the definitions is left out when they're read",
            out.display()
        );
    }
    let inputs = inputs::find(&args.inputs)?;
    if args.xtce.given() && !inputs.iter().any(|i| i.frontend == Frontend::Xtce) {
        bail!("the XTCE options apply to .xml inputs, and none were given");
    }
    let sources = inputs::read(&inputs)?;
    let mut report = Report::new(&sources, args.format, args.all_warnings);

    let parsed = inputs::parse(&sources, &inputs, &args.xtce.options()).and_then(merge);
    let Parsed {
        bundle,
        map,
        warnings,
        ..
    } = match parsed {
        Ok(parsed) => parsed,
        Err(problems) => {
            report.add(problems);
            return Ok(report.finish(None));
        }
    };
    report.add(warnings);
    let validated = match validate(bundle) {
        Ok(validated) => validated,
        Err(diagnostics) => {
            report.add(map.locate_all(&diagnostics));
            return Ok(report.finish(None));
        }
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
            return Ok(report.finish(None));
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
