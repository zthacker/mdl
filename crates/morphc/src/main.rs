//! morphc: the MDL compiler.
//!
//! ```text
//! morphc build definitions/        TOML frontend → IR → validate
//! morphc build --ir defs.json      IR from any frontend → validate
//! morphc build --ir -              the same, reading the IR from stdin
//! ```
//!
//! Exit codes: 0 the definitions are valid, 1 they have problems (every one
//! is printed), 2 morphc couldn't run (unreadable input, malformed JSON).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand};
use mdl::diagnostics::Errors;
use mdl::frontend;
use mdl::ir::Definitions;
use mdl::validate::validate;

#[derive(Debug, Parser)]
#[command(
    name = "morphc",
    about = "The MDL (Morpheus Definition Language) compiler"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check definitions and report every problem.
    Build {
        /// A directory of TOML definitions: `catalog.toml` plus `sets/<set>.toml`.
        #[arg(required_unless_present = "ir", conflicts_with = "ir")]
        dir: Option<PathBuf>,

        /// Read MDL IR JSON from this file instead ("-" for stdin), as
        /// produced by any frontend in any language.
        #[arg(long)]
        ir: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("morphc: {err:#}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        Command::Build { dir, ir } => {
            let loaded = match (ir, dir) {
                (Some(path), _) => Ok(read_ir(&path)?),
                (None, Some(dir)) => frontend::toml::load_dir(&dir),
                (None, None) => anyhow::bail!("give a definitions directory or --ir"),
            };
            // Format errors (from the frontend) and meaning errors (from
            // validation) are reported the same way.
            let definitions = match loaded {
                Ok(definitions) => definitions,
                Err(errors) => return Ok(report(&errors)),
            };
            match validate(definitions) {
                Ok(plan) => {
                    println!("{}", summary(plan.definitions()));
                    Ok(ExitCode::SUCCESS)
                }
                Err(errors) => Ok(report(&errors)),
            }
        }
    }
}

/// Read IR JSON from a file, or from stdin when the path is "-".
fn read_ir(path: &Path) -> anyhow::Result<Definitions> {
    let text = if path == Path::new("-") {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .context("reading IR from stdin")?;
        text
    } else {
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?
    };
    serde_json::from_str(&text)
        .with_context(|| format!("{} isn't valid MDL IR JSON", path.display()))
}

/// Print every diagnostic, then a count, to stderr.
fn report(errors: &Errors) -> ExitCode {
    eprint!("{errors}");
    let count = errors.0.len();
    eprintln!(
        "morphc: {count} problem{}",
        if count == 1 { "" } else { "s" }
    );
    ExitCode::from(1)
}

/// "ok: 2 sets, 5 packets, 21 fields, 14 canonical params (MDL IR v1)"
fn summary(definitions: &Definitions) -> String {
    let packets: usize = definitions.sets.values().map(|s| s.packets.len()).sum();
    let fields: usize = definitions
        .sets
        .values()
        .flat_map(|s| &s.packets)
        .map(|p| p.fields.len())
        .sum();
    format!(
        "ok: {} sets, {packets} packets, {fields} fields, {} canonical params (MDL IR v{})",
        definitions.sets.len(),
        definitions.catalog.len(),
        definitions.mdl_version
    )
}
