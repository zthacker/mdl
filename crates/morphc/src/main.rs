//! morphc: the MDL compiler.
//!
//! ```text
//! morphc build catalog.toml vendor.xml           read, merge, validate
//! morphc build definitions/ --out store/         ... and write artifacts
//! frontend | morphc build -                      an IR bundle on stdin
//! morphc inspect set vendor_x_v2 store/         a set's datasheet
//! morphc inspect param eps.v definitions/        who sends a param, and how
//! morphc decode store/ --hex 0a1b... --meta apid=100
//! morphc decode store/ --lines capture.txt       one message per line
//! morphc encode store/ --command eps.set_mode --arg mode=SAFE
//! ```
//!
//! Exit codes: 0 done, 1 the definitions have problems (every one is
//! reported), 2 morphc couldn't run (unreadable input, a damaged store).

mod build;
mod buildfile;
mod decode;
mod describe;
mod encode;
mod inputs;
mod inspect;
mod report;
mod store;
mod values;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, anyhow, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use mdl::artifact::{self, Artifacts};
use mdl::ir::{Artifact, Bundle, Locator};
use mdl::read::{Loaded, read_artifact};
use mdl::route::FamilySets;
use mdl::source::{Parsed, SourceDiagnostic, SourceMap, Sources, Span, merge};
use mdl::validate::{Validated, validate};

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
    /// Decode messages with definitions or a store, as a ground system
    /// does: route each to an asset and set, identify its packet, and
    /// give its catalog samples. With --command, decode command payloads.
    Decode(decode::DecodeArgs),
    /// Encode a catalog command and its arguments as a set's command
    /// packet: the payload, the metadata a link frames it with, and the
    /// def_version to record.
    Encode(encode::EncodeArgs),
}

/// How definitions are read, for every command that reads them.
#[derive(Debug, Args)]
struct ReadArgs {
    /// Definition files and directories: .toml (MDL TOML), .xml (XTCE)
    /// and .json (an MDL IR bundle); `-` reads a bundle from stdin.
    /// Bundles merge in the order of their first file, earlier ones taking
    /// precedence. A directory gives its TOML files, then its JSON, then
    /// its XTCE, each kind in name order. Without inputs, morphc reads the
    /// build file given with -c, or ./morphc.toml.
    #[arg(value_name = "INPUT")]
    inputs: Vec<PathBuf>,

    /// A build file listing the inputs, each with its own import options
    /// (spec/build-files.md), in place of INPUT [default: ./morphc.toml
    /// when no INPUT is given]
    #[arg(short = 'c', long = "build-file", value_name = "FILE")]
    build_file: Option<PathBuf>,

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
    /// The imported family's name [default: the root SpaceSystem's,
    /// lowercased]
    #[arg(id = "import_family", long = "import-family", value_name = "NAME")]
    family: Option<String>,

    /// The imported set's name [default: the family's]
    #[arg(id = "import_set", long = "import-set", value_name = "NAME")]
    set: Option<String>,

    /// Where an imported asset's own id is: `fixed:<id>`,
    /// `metadata:<name>`, `path:<pointer>` or `bits:<offset>:<size>`
    /// [default: fixed, the root SpaceSystem's name]
    #[arg(
        id = "import_alias",
        long = "import-alias",
        value_name = "LOCATOR",
        value_parser = parse_alias
    )]
    alias: Option<Locator>,

    /// How packet, field and type names are made from XTCE names
    #[arg(id = "xtce_naming", long = "xtce-naming", value_enum)]
    naming: Option<Naming>,
}

#[derive(Debug, Clone, Copy, ValueEnum, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum Naming {
    /// As XTCE has them, qualified by SpaceSystem only where two share one
    Short,
    /// Always qualified by SpaceSystem, so names don't change as
    /// documents grow
    Qualified,
}

impl Naming {
    fn xtce(self) -> mdl_xtce::Naming {
        match self {
            Naming::Short => mdl_xtce::Naming::Short,
            Naming::Qualified => mdl_xtce::Naming::Qualified,
        }
    }
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
            options = options.with_naming(naming.xtce());
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
        Command::Decode(args) => (args.read.format, decode::run(args)),
        Command::Encode(args) => (args.read.format, encode::run(args)),
    };
    match result {
        Ok(code) => code,
        Err(err) => match err.downcast_ref::<buildfile::Invalid>() {
            Some(invalid) => {
                let mut sources = Sources::new();
                let file = sources.add(invalid.file.display().to_string(), invalid.text.clone());
                let mut problem =
                    SourceDiagnostic::error("morphc.build_file", invalid.message.clone());
                if let Some(span) = &invalid.span {
                    problem = problem.with_span(Span::new(file, span.start, span.end));
                }
                report::failed_at(format, &sources, problem)
            }
            None => report::failed(format, &err),
        },
    }
}

/// What a run reads, from the command line or a build file: each path
/// with the XTCE import its documents belong to, each import's options,
/// and the build file, if any.
struct Given {
    paths: Vec<(PathBuf, usize)>,
    imports: Vec<mdl_xtce::Options>,
    build_file: Option<PathBuf>,
    /// For each import, whether it was given options (which only XTCE
    /// documents can use).
    optioned: Vec<bool>,
    /// What to tell people about how the inputs were found and ordered.
    notes: Vec<String>,
}

impl Given {
    fn of(read: &ReadArgs) -> anyhow::Result<Given> {
        let build_file = match (&read.build_file, read.inputs.is_empty()) {
            (Some(_), false) => bail!("give inputs or a build file (-c), not both"),
            (Some(file), true) => Some(file.clone()),
            (None, true) if Path::new(buildfile::NAME).is_file() => {
                Some(PathBuf::from(buildfile::NAME))
            }
            (None, true) => bail!(
                "give definition files or directories, a build file with -c, or run where a {} \
                 is",
                buildfile::NAME
            ),
            (None, false) => None,
        };
        let Some(file) = build_file else {
            return Ok(Given {
                paths: read.inputs.iter().map(|path| (path.clone(), 0)).collect(),
                imports: vec![read.xtce.options()],
                build_file: None,
                optioned: vec![read.xtce.given()],
                notes: Vec::new(),
            });
        };
        let mut notes = Vec::new();
        if read.build_file.is_none() {
            notes.push(format!(
                "no inputs were given, so the definitions are the ones ./{} lists",
                buildfile::NAME
            ));
        }
        if read.xtce.given() {
            bail!(
                "with a build file, each input's import options are written in it ({})",
                file.display()
            );
        }
        let listed = buildfile::read(&file)?;
        Ok(Given {
            paths: listed
                .iter()
                .enumerate()
                .map(|(index, input)| (input.path.clone(), index))
                .collect(),
            optioned: listed.iter().map(|input| input.has_options).collect(),
            imports: listed.into_iter().map(|input| input.options).collect(),
            build_file: Some(file),
            notes,
        })
    }

    /// The definition files it names; import options only where there are
    /// XTCE documents to use them.
    fn find(&mut self) -> anyhow::Result<Vec<Input>> {
        let inputs = inputs::find(&self.paths)?;
        self.notes.extend(self.toml_order(&inputs));
        for (import, optioned) in self.optioned.iter().enumerate() {
            let xtce = inputs
                .iter()
                .any(|i| i.frontend == Frontend::Xtce && i.import == import);
            if !optioned || xtce {
                continue;
            }
            match (&self.build_file, self.paths.get(import)) {
                (Some(file), Some((path, _))) => bail!(
                    "{}: input {} has import options but no .xml files",
                    file.display(),
                    inputs::shown_path(path)
                ),
                _ => bail!("the XTCE options apply to .xml inputs, and none were given"),
            }
        }
        Ok(inputs)
    }

    /// Every TOML file is one bundle, placed where the first one is, so a
    /// build file's TOML input listed after an XTCE or JSON input that
    /// itself follows an earlier TOML input still takes precedence over
    /// it: worth saying, since the list reads otherwise.
    fn toml_order(&self, inputs: &[Input]) -> Vec<String> {
        if self.build_file.is_none() {
            return Vec::new();
        }
        let kinds = |import: usize| {
            let of = |kind: Frontend| {
                inputs
                    .iter()
                    .any(|input| input.import == import && input.frontend == kind)
            };
            (of(Frontend::Toml), of(Frontend::Xtce) || of(Frontend::Json))
        };
        let name = |import: usize| {
            self.paths
                .get(import)
                .map_or_else(String::new, |(path, _)| inputs::shown_path(path))
        };
        let mut notes = Vec::new();
        let mut first_toml = None;
        let mut other_since = None;
        for import in 0..self.paths.len() {
            let (toml, other) = kinds(import);
            match (first_toml, other_since) {
                (None, _) if toml => first_toml = Some(import),
                (Some(first), Some(other)) if toml => notes.push(format!(
                    "{} comes after {} in the build file, but every TOML file is read as one \
                     bundle where the first TOML input ({}) is, so it takes precedence over it",
                    name(import),
                    name(other),
                    name(first)
                )),
                _ => {}
            }
            if first_toml.is_some() && other && other_since.is_none() {
                other_since = Some(import);
            }
        }
        notes
    }
}

/// The definitions in `sources`, merged and validated, with where each
/// value came from; or none, with every problem in `report`. Warnings go
/// to `report` either way.
fn definitions(
    imports: &[mdl_xtce::Options],
    inputs: &[Input],
    sources: &Sources,
    report: &mut Report,
) -> Option<(Validated, SourceMap)> {
    let parsed = inputs::parse(sources, inputs, imports).and_then(merge);
    let Parsed {
        bundle,
        map,
        warnings,
        ..
    } = match parsed {
        Ok(parsed) => parsed,
        Err(problems) => {
            let xtce_imports: std::collections::BTreeSet<usize> = inputs
                .iter()
                .filter(|input| input.frontend == Frontend::Xtce)
                .map(|input| input.import)
                .collect();
            if xtce_imports.len() > 1 && problems.iter().any(|p| p.code == "xtce.reference") {
                report.note(
                    "references between XTCE documents resolve within one import: documents \
                     that refer to each other are one input in the build file (their \
                     directory, say)",
                );
            }
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

/// Where a build comes from: one artifact store's latest build, or
/// definitions built in memory.
enum Origin {
    Store(PathBuf),
    Definitions {
        inputs: Vec<Input>,
        imports: Vec<mdl_xtce::Options>,
        notes: Vec<String>,
    },
}

impl Origin {
    /// A store when it's the only input given, else definitions.
    fn of(read: &ReadArgs) -> anyhow::Result<Origin> {
        if let [only] = read.inputs.as_slice()
            && only.is_dir()
            && store::is_store(only)
        {
            if read.xtce.given() {
                bail!(
                    "{} is an artifact store; the XTCE options apply only to definitions",
                    only.display()
                );
            }
            return Ok(Origin::Store(only.clone()));
        }
        let mut given = Given::of(read)?;
        let inputs = given.find()?;
        Ok(Origin::Definitions {
            inputs,
            imports: given.imports,
            notes: given.notes,
        })
    }

    /// The text diagnostics point into: the definition files, or none.
    fn sources(&self) -> anyhow::Result<Sources> {
        match self {
            Origin::Store(_) => Ok(Sources::new()),
            Origin::Definitions { inputs, .. } => inputs::read(inputs),
        }
    }
}

/// A build's artifacts, and the bundle they hold, validated as one.
struct Opened {
    validated: Validated,
    artifacts: Artifacts,
    /// The canonical text of every catalog its sets pin: its own, and in a
    /// store the older ones kept sets still pin.
    catalogs: Vec<String>,
}

/// The build `origin` holds; none when its definitions have problems,
/// which are in `report`. Definitions show what a store of them would
/// hold: the bundle their artifacts carry, sets listing packets by name.
fn open(origin: &Origin, sources: &Sources, report: &mut Report) -> anyhow::Result<Option<Opened>> {
    let (artifacts, catalogs) = match origin {
        Origin::Store(dir) => {
            let stored = store::load(dir)
                .with_context(|| format!("can't read the store at {}", dir.display()))?;
            let catalogs = stored.catalogs().map(str::to_string).collect();
            (stored.artifacts, catalogs)
        }
        Origin::Definitions {
            inputs,
            imports,
            notes,
        } => {
            for note in notes {
                report.note(note.clone());
            }
            let Some((validated, _)) = definitions(imports, inputs, sources, report) else {
                return Ok(None);
            };
            let artifacts = artifact::build(&validated).context("can't write the artifacts")?;
            let catalogs = vec![artifacts.catalog.canonical.clone()];
            (artifacts, catalogs)
        }
    };
    let validated = canonical(&artifacts).context("the build doesn't validate as one bundle")?;
    Ok(Some(Opened {
        validated,
        artifacts,
        catalogs,
    }))
}

/// A build's catalog, families and sets, validated together. A set kept
/// from an older build pins an older catalog the build's own is
/// compatible with, so it validates against the build's.
fn canonical(artifacts: &Artifacts) -> anyhow::Result<Validated> {
    let Artifact::Catalog { catalog, .. } = &artifacts.catalog.artifact else {
        bail!("its catalog artifact isn't a catalog");
    };
    let mut bundle = Bundle::new(catalog.clone());
    for written in artifacts.families.values() {
        if let Artifact::Family { family, .. } = &written.artifact {
            bundle.families.push(family.clone());
        }
    }
    for written in artifacts.sets.values() {
        if let Artifact::Set { set, .. } = &written.artifact {
            bundle.sets.push(set.clone());
        }
    }
    validate(bundle).map_err(|diagnostics| anyhow!("{diagnostics}"))
}

/// The family's sets, assembled from the artifacts as a consumer
/// assembles them.
fn family_sets(opened: &Opened, family: &str) -> anyhow::Result<FamilySets> {
    let load = |canonical: &str| -> anyhow::Result<Loaded<Artifact>> {
        read_artifact(canonical.as_bytes()).context("an artifact doesn't read back")
    };
    let written = opened
        .artifacts
        .families
        .get(family)
        .with_context(|| format!("there's no family {family}"))?;
    let family_artifact = load(&written.canonical)?;
    let mut sets = Vec::new();
    for written in opened.artifacts.sets.values() {
        if let Artifact::Set { set, .. } = &written.artifact
            && set.family == family
        {
            sets.push(load(&written.canonical)?);
        }
    }
    let catalogs = opened
        .catalogs
        .iter()
        .map(|text| load(text))
        .collect::<anyhow::Result<Vec<_>>>()?;
    FamilySets::new(&family_artifact, &sets, &catalogs)
        .with_context(|| format!("family {family}'s sets don't assemble"))
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::Cli;

    /// Clap checks the whole command line here (unique names, groups,
    /// conflicts), not only when a command is first run.
    #[test]
    fn the_command_line_is_well_formed() {
        Cli::command().debug_assert();
    }
}
