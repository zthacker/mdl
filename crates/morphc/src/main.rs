//! morphc: the MDL compiler.
//!
//! ```text
//! morphc build catalog.toml vendor.xml           read, merge, validate
//! morphc build definitions/ --out store/         ... and write artifacts
//! frontend | morphc build -                      an IR bundle on stdin
//! morphc inspect set vendor_x_v2 store/         a set's datasheet
//! morphc inspect param eps.v definitions/        who sends a param, and how
//! ```
//!
//! Exit codes: 0 done, 1 the definitions have problems (every one is
//! reported), 2 morphc couldn't run (unreadable input, a damaged store).

mod build;
mod describe;
mod inputs;
mod inspect;
mod report;
mod store;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::bail;
use clap::{Args, Parser, Subcommand, ValueEnum};
use mdl::v2::ir::Locator;
use mdl::v2::source::{Parsed, SourceMap, Sources, merge};
use mdl::v2::validate::{Validated, validate};

use crate::inputs::{Frontend, Input};
use crate::report::{Format, Report};

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
    Build(build::BuildArgs),
    /// Show what definitions or a store hold: families and sets, a set's
    /// packets with where each entry sits, or every packet that sends a
    /// catalog param or encodes a command. The inputs are definitions, as
    /// for build, or one artifact store, whose latest build is shown.
    /// Definitions show a fresh build's identities; a store shows the ones
    /// its rebuilds kept. Views come first (`morphc inspect set NAME
    /// INPUT...`); an input named like a view is written ./set.
    Inspect(inspect::InspectArgs),
}

/// How definitions are read, for every command that reads them.
#[derive(Debug, Args)]
struct ReadArgs {
    /// Definition files and directories: .toml (MDL TOML), .xml (XTCE)
    /// and .json (an MDL IR bundle); `-` reads a bundle from stdin.
    /// Bundles merge in the order of their first file, earlier ones taking
    /// precedence. A directory gives its TOML files, then its JSON, then
    /// its XTCE, each kind in name order.
    #[arg(required = true, value_name = "INPUT")]
    inputs: Vec<PathBuf>,

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
        Command::Build(args) => (args.read.format, build::run(args)),
        Command::Inspect(args) => (args.read().format, inspect::run(args)),
    };
    match result {
        Ok(code) => code,
        Err(err) => report::failed(format, &err),
    }
}

/// The definition files `read` names.
fn find_inputs(read: &ReadArgs) -> anyhow::Result<Vec<Input>> {
    let inputs = inputs::find(&read.inputs)?;
    if read.xtce.given() && !inputs.iter().any(|i| i.frontend == Frontend::Xtce) {
        bail!("the XTCE options apply to .xml inputs, and none were given");
    }
    Ok(inputs)
}

/// The definitions in `sources`, merged and validated, with where each
/// value came from; or none, with every problem in `report`. Warnings go
/// to `report` either way.
fn definitions(
    read: &ReadArgs,
    inputs: &[Input],
    sources: &Sources,
    report: &mut Report,
) -> Option<(Validated, SourceMap)> {
    let parsed = inputs::parse(sources, inputs, &read.xtce.options()).and_then(merge);
    let Parsed {
        bundle,
        map,
        warnings,
        ..
    } = match parsed {
        Ok(parsed) => parsed,
        Err(problems) => {
            report.add(problems);
            return None;
        }
    };
    report.add(warnings);
    match validate(bundle) {
        Ok(validated) => Some((validated, map)),
        Err(diagnostics) => {
            report.add(map.locate_all(&diagnostics));
            None
        }
    }
}
