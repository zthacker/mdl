//! `morphc inspect`: what definitions or a store hold, for people or
//! tools. The overview names every family and set with its identity; a
//! set's datasheet shows where each entry sits; a param or command shows
//! every packet that sends or encodes it.

use std::collections::BTreeMap;
use std::process::ExitCode;

use anyhow::{Context, anyhow, bail};
use clap::{Args, Subcommand};
use mdl::v2::artifact::{self, Artifacts};
use mdl::v2::ir::{
    Argument, Artifact, Bundle, ByteLength, Command, Composite, Count, Element, Encoding, Entry,
    Family, Param, Set, StructType,
};
use mdl::v2::layout::{self, ContainerLayout, LayoutError, Placement, SetLayout};
use mdl::v2::source::Sources;
use mdl::v2::validate::{Validated, validate};
use serde::Serialize;

use crate::describe::{self, ir};
use crate::report::{Outcome, Report};
use crate::{ReadArgs, definitions, find_inputs, inputs, store};

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct InspectArgs {
    #[command(subcommand)]
    view: Option<View>,

    // Last, since the XTCE options' heading applies to every argument
    // after them.
    #[command(flatten)]
    read: ReadArgs,
}

#[derive(Debug, Subcommand)]
enum View {
    /// One set: its packets, command packets and struct types, with where
    /// each entry sits.
    Set(SetView),
    /// One catalog param: its definition and every packet entry that
    /// sends it.
    Param(ParamView),
    /// One catalog command: its arguments and every command packet that
    /// encodes it.
    Command(CommandView),
}

#[derive(Debug, Args)]
struct SetView {
    /// The set's name.
    #[arg(value_name = "SET")]
    name: String,

    #[command(flatten)]
    read: ReadArgs,
}

#[derive(Debug, Args)]
struct ParamView {
    /// The catalog param's name.
    #[arg(value_name = "PARAM")]
    name: String,

    #[command(flatten)]
    read: ReadArgs,
}

#[derive(Debug, Args)]
struct CommandView {
    /// The catalog command's name.
    #[arg(value_name = "COMMAND")]
    name: String,

    #[command(flatten)]
    read: ReadArgs,
}

impl InspectArgs {
    pub fn read(&self) -> &ReadArgs {
        match &self.view {
            Some(View::Set(view)) => &view.read,
            Some(View::Param(view)) => &view.read,
            Some(View::Command(view)) => &view.read,
            None => &self.read,
        }
    }
}

pub fn run(args: &InspectArgs) -> anyhow::Result<ExitCode> {
    let read = args.read();
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
        let stored = store::load(only)
            .with_context(|| format!("can't read the store at {}", only.display()))?;
        let validated = validated_store(&stored.artifacts).with_context(|| {
            format!("the store at {} doesn't hold a valid build", only.display())
        })?;
        let inspected = inspect(&validated, &stored.artifacts, args.view.as_ref())?;
        let sources = Sources::new();
        let report = Report::new(&sources, read.format, read.all_warnings);
        return Ok(report.finish(Some(inspected)));
    }
    let inputs = find_inputs(read).map_err(|err| match view_word(read) {
        Some(word) => err.context(format!(
            "views come before the inputs (`morphc inspect {word} NAME INPUT...`), and an \
             input named {word} is written ./{word}"
        )),
        None => err,
    })?;
    let sources = inputs::read(&inputs)?;
    let mut report = Report::new(&sources, read.format, read.all_warnings);
    let Some((validated, _)) = definitions(read, &inputs, &sources, &mut report) else {
        return Ok(report.finish::<Inspected>(None));
    };
    // What the artifacts hold, so definitions show what a store of them
    // would (sets list their packets by name there).
    let artifacts = artifact::build(&validated).context("can't write the artifacts")?;
    let canonical = validated_store(&artifacts).context("the built artifacts don't validate")?;
    let inspected = inspect(&canonical, &artifacts, args.view.as_ref())?;
    Ok(report.finish(Some(inspected)))
}

/// A view word among the inputs: given after an input or an option,
/// it's taken as an input.
fn view_word(read: &ReadArgs) -> Option<&'static str> {
    ["set", "param", "command"]
        .into_iter()
        .find(|word| read.inputs.iter().any(|input| input.as_os_str() == *word))
}

/// A store's latest build as one bundle: its catalog, families and sets,
/// validated together. A set kept from an older build pins an older
/// catalog the latest one is compatible with, so it validates against the
/// latest.
fn validated_store(artifacts: &Artifacts) -> anyhow::Result<Validated> {
    let Artifact::Catalog { catalog, .. } = &artifacts.catalog.artifact else {
        bail!("its manifest's catalog isn't a catalog");
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

fn inspect(
    validated: &Validated,
    artifacts: &Artifacts,
    view: Option<&View>,
) -> anyhow::Result<Inspected> {
    let bundle = validated.bundle();
    Ok(match view {
        None => Inspected::Overview(overview(bundle, artifacts)),
        Some(View::Set(view)) => Inspected::Set(set_sheet(validated, artifacts, &view.name)?),
        Some(View::Param(view)) => Inspected::Param(param_uses(bundle, &view.name)?),
        Some(View::Command(view)) => Inspected::Command(command_uses(bundle, &view.name)?),
    })
}

/// What `morphc inspect` found, under `inspect` in the JSON document,
/// tagged with its view.
#[derive(Debug, Serialize)]
#[serde(tag = "view", rename_all = "snake_case")]
pub enum Inspected {
    Overview(Overview),
    Set(SetSheet),
    Param(ParamUses),
    Command(CommandUses),
}

impl Outcome for Inspected {
    const KEY: &'static str = "inspect";

    fn human(&self) -> String {
        match self {
            Inspected::Overview(overview) => overview.human(),
            Inspected::Set(sheet) => sheet.human(),
            Inspected::Param(uses) => uses.human(),
            Inspected::Command(uses) => uses.human(),
        }
    }
}

/// The digest of the catalog a set pins when it's older than the build's
/// own (a set kept by rebuilds).
fn pinned_catalog(artifacts: &Artifacts, set: &str) -> Option<String> {
    match &artifacts.sets.get(set)?.artifact {
        Artifact::Set { catalog, .. } if *catalog != artifacts.catalog.digest => {
            Some(catalog.to_string())
        }
        _ => None,
    }
}

fn digest_of(written: Option<&artifact::Written>) -> String {
    written.map_or_else(String::new, |w| w.digest.to_string())
}

/// `binary, big-endian`, or `json`.
fn encoding_text(family: &Family) -> String {
    match family.byte_order {
        Some(order) => format!("{}, {}-endian", ir(&family.encoding), ir(&order)),
        None => ir(&family.encoding),
    }
}

// ---- Overview --------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct Overview {
    mdl_version: String,
    catalog: CatalogSummary,
    families: Vec<FamilySummary>,
}

#[derive(Debug, Serialize)]
struct CatalogSummary {
    digest: String,
    params: usize,
    commands: usize,
}

#[derive(Debug, Serialize)]
struct FamilySummary {
    name: String,
    digest: String,
    encoding: String,
    byte_order: Option<String>,
    /// Where an asset's own id is, as the IR writes a locator.
    alias: serde_json::Value,
    /// Where a set's version is, when messages carry one.
    version: Option<serde_json::Value>,
    metadata: Vec<String>,
    sets: Vec<SetSummary>,
}

#[derive(Debug, Serialize)]
struct SetSummary {
    name: String,
    digest: String,
    version: Option<serde_json::Value>,
    packets: usize,
    commands: usize,
    types: usize,
    /// The catalog it pins, when older than the build's.
    pins_catalog: Option<String>,
}

fn overview(bundle: &Bundle, artifacts: &Artifacts) -> Overview {
    let families = bundle
        .families
        .iter()
        .map(|family| FamilySummary {
            name: family.name.clone(),
            digest: digest_of(artifacts.families.get(&family.name)),
            encoding: ir(&family.encoding),
            byte_order: family.byte_order.as_ref().map(ir),
            alias: to_json(&family.alias),
            version: family.version.as_ref().map(to_json),
            metadata: family.metadata.clone(),
            sets: bundle
                .sets
                .iter()
                .filter(|set| set.family == family.name)
                .map(|set| SetSummary {
                    name: set.name.clone(),
                    digest: digest_of(artifacts.sets.get(&set.name)),
                    version: set.version.as_ref().map(to_json),
                    packets: set.packets.len(),
                    commands: set.commands.len(),
                    types: set.types.len(),
                    pins_catalog: pinned_catalog(artifacts, &set.name),
                })
                .collect(),
        })
        .collect();
    Overview {
        mdl_version: bundle.mdl_version.to_string(),
        catalog: CatalogSummary {
            digest: artifacts.catalog.digest.to_string(),
            params: bundle.catalog.params.len(),
            commands: bundle.catalog.commands.len(),
        },
        families,
    }
}

/// An IR value as a JSON value, for documents that carry it as the IR
/// writes it.
fn to_json<T: Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
}

impl Overview {
    fn human(&self) -> String {
        let count = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
        let mut text = format!(
            "catalog  {}  {}, {}\n",
            self.catalog.digest,
            count(self.catalog.params, "param"),
            count(self.catalog.commands, "command")
        );
        for family in &self.families {
            text.push_str(&format!("\nfamily {}  {}\n", family.name, family.digest));
            let encoding = match &family.byte_order {
                Some(order) => format!("{}, {order}-endian", family.encoding),
                None => family.encoding.clone(),
            };
            let chosen = match &family.version {
                Some(version) => format!("set chosen by the version at {}", locator_text(version)),
                None => "set chosen by binding".to_string(),
            };
            text.push_str(&format!(
                "  {encoding}; asset id: {}; {chosen}\n",
                locator_text(&family.alias)
            ));
            if !family.metadata.is_empty() {
                text.push_str(&format!("  metadata: {}\n", family.metadata.join(", ")));
            }
            for set in &family.sets {
                let mut parts = Vec::new();
                if let Some(version) = &set.version {
                    parts.push(format!("version {version}"));
                }
                parts.push(count(set.packets, "packet"));
                parts.push(count(set.commands, "command"));
                text.push_str(&format!(
                    "  set {}  {}  {}\n",
                    set.name,
                    set.digest,
                    parts.join(", ")
                ));
                if let Some(pinned) = &set.pins_catalog {
                    text.push_str(&format!("    pins the older catalog {pinned}\n"));
                }
            }
        }
        text
    }
}

/// A locator from its JSON form, as `describe::locator` shows it.
fn locator_text(value: &serde_json::Value) -> String {
    serde_json::from_value(value.clone())
        .map_or_else(|_| value.to_string(), |l| describe::locator(&l))
}

// ---- A set's datasheet ----------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct SetSheet {
    name: String,
    family: String,
    digest: String,
    encoding: String,
    pins_catalog: Option<String>,
    /// The set as the IR writes it.
    set: serde_json::Value,
    /// Where each entry sits; none for a self-describing family.
    layout: Option<JsonLayout>,
    /// The datasheet as people read it.
    #[serde(skip)]
    text: String,
}

#[derive(Debug, Serialize)]
struct JsonLayout {
    packets: Vec<JsonContainer>,
    commands: Vec<JsonContainer>,
    types: BTreeMap<String, JsonContainer>,
}

#[derive(Debug, Serialize)]
struct JsonContainer {
    name: String,
    size_min_bits: u64,
    size_max_bits: Option<u64>,
    /// One per entry, in the IR's order.
    entries: Vec<JsonPlacement>,
}

#[derive(Debug, Serialize)]
struct JsonPlacement {
    static_offset_bits: Option<u64>,
    start_min_bits: u64,
    start_max_bits: Option<u64>,
    size_min_bits: u64,
    size_max_bits: Option<u64>,
    conditional: bool,
}

impl JsonContainer {
    fn of(container: &ContainerLayout) -> Self {
        JsonContainer {
            name: container.name.clone(),
            size_min_bits: container.size_min_bits,
            size_max_bits: container.size_max_bits,
            entries: container
                .entries
                .iter()
                .map(|p| JsonPlacement {
                    static_offset_bits: p.static_offset_bits,
                    start_min_bits: p.start_min_bits,
                    start_max_bits: p.start_max_bits,
                    size_min_bits: p.size_min_bits,
                    size_max_bits: p.size_max_bits,
                    conditional: p.conditional,
                })
                .collect(),
        }
    }
}

fn set_sheet(validated: &Validated, artifacts: &Artifacts, name: &str) -> anyhow::Result<SetSheet> {
    let bundle = validated.bundle();
    let Some(set) = bundle.sets.iter().find(|s| s.name == name) else {
        let names: Vec<&str> = bundle.sets.iter().map(|s| s.name.as_str()).collect();
        bail!("there's no set {name}; the sets are {}", names.join(", "));
    };
    let family = bundle
        .families
        .iter()
        .find(|f| f.name == set.family)
        .ok_or_else(|| anyhow!("set {name}'s family {} isn't defined", set.family))?;
    let layout = match layout::of_set(validated, name) {
        Ok(layout) => Some(layout),
        Err(LayoutError::SelfDescribing { .. }) => None,
        Err(err) => return Err(err.into()),
    };
    let sheet = Sheet {
        set,
        layout: layout.as_ref(),
        binary: family.encoding == Encoding::Binary,
    };
    let mut text = format!(
        "set {}  {}\n  family {}: {}\n",
        set.name,
        digest_of(artifacts.sets.get(&set.name)),
        family.name,
        encoding_text(family)
    );
    if let Some(version) = &set.version {
        text.push_str(&format!("  version {}\n", describe::scalar(version)));
    }
    if let Some(time) = &set.time {
        text.push_str(&format!("  time: {}\n", describe::time(time)));
    }
    if let Some(pinned) = pinned_catalog(artifacts, &set.name) {
        text.push_str(&format!("  pins the older catalog {pinned}\n"));
    }
    text.push_str(&sheet.human());
    Ok(SetSheet {
        name: set.name.clone(),
        family: family.name.clone(),
        digest: digest_of(artifacts.sets.get(&set.name)),
        encoding: ir(&family.encoding),
        pins_catalog: pinned_catalog(artifacts, &set.name),
        set: serde_json::to_value(set).context("can't write the set as JSON")?,
        layout: layout.as_ref().map(|layout| JsonLayout {
            packets: layout.packets.iter().map(JsonContainer::of).collect(),
            commands: layout.commands.iter().map(JsonContainer::of).collect(),
            types: layout
                .types
                .iter()
                .map(|(name, t)| (name.clone(), JsonContainer::of(t)))
                .collect(),
        }),
        text,
    })
}

impl SetSheet {
    fn human(&self) -> String {
        self.text.clone()
    }
}

/// A set with its layout, for the datasheet's tables.
struct Sheet<'a> {
    set: &'a Set,
    layout: Option<&'a SetLayout>,
    binary: bool,
}

/// One row of a datasheet table.
struct Line {
    at: String,
    size: String,
    entry: String,
    ty: String,
    target: String,
    notes: Vec<String>,
}

impl Sheet<'_> {
    fn human(&self) -> String {
        let mut text = String::new();
        for packet in &self.set.packets {
            let mut header = vec![format!("packet {}", packet.name)];
            if let Some(length) = &packet.length {
                header.push(describe::length(length));
            }
            if !packet.identify.is_empty() {
                let keys: Vec<String> = packet
                    .identify
                    .iter()
                    .map(|(key, value)| format!("{key} = {}", describe::scalar(value)))
                    .collect();
                header.push(format!("identified by {}", keys.join(", ")));
            }
            if let Some(time) = &packet.time_field {
                header.push(format!("time from {time}"));
            }
            let container = self.layout.and_then(|l| l.packet(&packet.name));
            text.push('\n');
            text.push_str(&header.join("; "));
            text.push('\n');
            text.push_str(&self.table(&packet.entries, container, false, "maps to"));
            for composite in &packet.composites {
                text.push_str(&composite_line(composite));
            }
        }
        for packet in &self.set.commands {
            let mut header = vec![
                format!("command {}", packet.name),
                format!("encodes {}", packet.canonical),
            ];
            if !packet.metadata.is_empty() {
                let keys: Vec<String> = packet
                    .metadata
                    .iter()
                    .map(|(key, value)| format!("{key} = {}", describe::scalar(value)))
                    .collect();
                header.push(format!("metadata {}", keys.join(", ")));
            }
            let container = self.layout.and_then(|l| l.command(&packet.name));
            text.push('\n');
            text.push_str(&header.join("; "));
            text.push('\n');
            text.push_str(&self.table(&packet.entries, container, true, "from"));
        }
        for (name, struct_type) in &self.set.types {
            let container = self.layout.and_then(|l| l.types.get(name));
            text.push_str(&format!("\ntype {name}\n"));
            text.push_str(&self.table(&struct_type.entries, container, false, "maps to"));
        }
        text
    }

    /// The entries of one container, a row each, in aligned columns.
    fn table(
        &self,
        entries: &[Entry],
        layout: Option<&ContainerLayout>,
        command: bool,
        target: &str,
    ) -> String {
        let at = if self.binary { "bit" } else { "at" };
        let mut lines = vec![Line {
            at: at.to_string(),
            size: if self.binary { "size" } else { "" }.to_string(),
            entry: "entry".to_string(),
            ty: "type".to_string(),
            target: target.to_string(),
            notes: vec!["notes".to_string()],
        }];
        let gives = sizes_given(entries);
        for (index, entry) in entries.iter().enumerate() {
            let placement = layout.and_then(|l| l.entries.get(index));
            let mut line = line(entry, placement, command);
            if let Some(given) = describe::entry_name(entry).and_then(|name| gives.get(name)) {
                line.notes.insert(0, given.join("; "));
            }
            lines.push(line);
        }
        let width = |column: fn(&Line) -> &str| {
            lines
                .iter()
                .map(|l| column(l).chars().count())
                .max()
                .unwrap_or(0)
        };
        let widths = [
            width(|l| &l.at),
            width(|l| &l.size),
            width(|l| &l.entry),
            width(|l| &l.ty),
            width(|l| &l.target),
        ];
        let mut text = String::new();
        for line in &lines {
            let cells = [&line.at, &line.size, &line.entry, &line.ty, &line.target];
            let mut row: Vec<String> = cells
                .iter()
                .zip(widths)
                .filter(|(_, width)| *width > 0)
                .map(|(cell, width)| format!("{cell:width$}"))
                .collect();
            row.push(line.notes.join("; "));
            text.push_str(format!("  {}", row.join("  ")).trim_end());
            text.push('\n');
        }
        if lines
            .iter()
            .any(|line| line.at.ends_with(describe::NOT_STATIC))
        {
            text.push_str(&format!(
                "  {} starts there in every message, without a static position (decoding \
                 spec 5.2)\n",
                describe::NOT_STATIC
            ));
        }
        text
    }
}

fn line(entry: &Entry, placement: Option<&Placement>, command: bool) -> Line {
    let (at, size) = match placement {
        Some(placement) => (describe::start(placement), describe::size(placement)),
        None => (pointer(entry).unwrap_or_default(), String::new()),
    };
    let mut notes = Vec::new();
    let mut target = String::new();
    match entry {
        Entry::Field(field) => {
            if command {
                if let Some(argument) = &field.argument {
                    target = argument.clone();
                } else if let Some(value) = &field.value {
                    target = format!("= {}", describe::scalar(value));
                } else if field.target == Some(true) {
                    target = "the asset's id".to_string();
                }
            } else if let Some(canonical) = &field.canonical {
                target = canonical.clone();
            }
            if let Some(unit) = &field.unit {
                notes.push(format!("unit {unit}"));
            }
            if let Some(conversion) = &field.conversion {
                notes.push(describe::conversion(conversion));
            }
            if let Some(inverse) = &field.inverse {
                notes.push(format!("inverse {}", describe::conversion(inverse)));
            }
            if !field.invalid.is_empty() {
                let invalid: Vec<String> = field.invalid.iter().map(describe::scalar).collect();
                notes.push(format!("invalid {}", invalid.join(", ")));
            }
        }
        Entry::Array(array) => {
            target = if command {
                array.argument.clone().unwrap_or_default()
            } else {
                array.canonical.clone().unwrap_or_default()
            };
            if let Element::Scalar(element) = &array.element {
                if let Some(unit) = &element.unit {
                    notes.push(format!("unit {unit}"));
                }
                if let Some(conversion) = &element.conversion {
                    notes.push(describe::conversion(conversion));
                }
            }
        }
        Entry::Checksum(checksum) => {
            if let Some(from) = checksum.from_bits {
                notes.push(format!("over bits from {from}"));
            }
        }
        _ => {}
    }
    if let Some(when) = condition(entry) {
        notes.push(format!("when {}", describe::condition(when)));
    }
    Line {
        at,
        size,
        entry: describe::entry_name(entry).map_or_else(|| "(padding)".to_string(), str::to_string),
        ty: describe::entry_type(entry),
        target,
        notes,
    }
}

/// For each field another entry of the container takes its count or
/// length from: `count of xs`, `length of label`. A command's encoder works
/// such a field out from the value it measures.
fn sizes_given(entries: &[Entry]) -> BTreeMap<&str, Vec<String>> {
    let mut given: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for entry in entries {
        let (from, what) = match entry {
            Entry::Array(array) => match &array.count {
                Some(Count::Field(from)) => (from, format!("count of {}", array.name)),
                _ => continue,
            },
            Entry::Field(field) => match &field.length {
                Some(ByteLength::Field(from)) => (from, format!("length of {}", field.name)),
                _ => continue,
            },
            _ => continue,
        };
        given.entry(from.field.as_str()).or_default().push(what);
    }
    given
}

fn pointer(entry: &Entry) -> Option<String> {
    match entry {
        Entry::Field(field) => field.pointer.clone(),
        Entry::Array(array) => array.pointer.clone(),
        Entry::Struct(entry) => entry.pointer.clone(),
        _ => None,
    }
}

fn condition(entry: &Entry) -> Option<&mdl::v2::ir::Condition> {
    match entry {
        Entry::Field(field) => field.when.as_ref(),
        Entry::Array(array) => array.when.as_ref(),
        Entry::Struct(entry) => entry.when.as_ref(),
        Entry::Padding(padding) => padding.when.as_ref(),
        _ => None,
    }
}

fn composite_line(composite: &Composite) -> String {
    format!(
        "  composite {} of {} in {}\n",
        composite.canonical,
        composite.members.join(", "),
        ir(&composite.frame)
    )
}

// ---- A param's uses ---------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct ParamUses {
    name: String,
    /// The catalog's definition, as the IR writes it.
    param: serde_json::Value,
    uses: Vec<ParamUse>,
    #[serde(skip)]
    text: String,
}

/// One entry, or composite, that sends the param.
#[derive(Debug, Serialize)]
struct ParamUse {
    set: String,
    packet: String,
    /// The entry, as a path through struct entries (`pos.x`) and arrays
    /// (`cells[].v`); none for a composite.
    entry: Option<String>,
    /// A composite's members, by path; none for an entry.
    members: Option<Vec<String>>,
    /// The frame a composite's members are in; consumers get the param's.
    frame: Option<String>,
    /// When the entry is present, if a condition on it (or on a struct
    /// it's in) can leave it out.
    when: Option<String>,
    unit: Option<String>,
    conversion: Option<serde_json::Value>,
}

fn param_uses(bundle: &Bundle, name: &str) -> anyhow::Result<ParamUses> {
    let Some(param) = bundle.catalog.params.get(name) else {
        bail!("the catalog has no param {name}");
    };
    let mut uses = Vec::new();
    for set in &bundle.sets {
        for packet in &set.packets {
            let mut found = Vec::new();
            let path = Path::default();
            sends(&packet.entries, &set.types, name, &path, &mut found);
            for sent in found {
                uses.push(ParamUse {
                    set: set.name.clone(),
                    packet: packet.name.clone(),
                    entry: Some(sent.entry),
                    members: None,
                    frame: None,
                    when: sent.when,
                    unit: sent.unit,
                    conversion: sent.conversion,
                });
            }
            for composite in packet.composites.iter().filter(|c| c.canonical == name) {
                uses.push(ParamUse {
                    set: set.name.clone(),
                    packet: packet.name.clone(),
                    entry: None,
                    members: Some(composite.members.clone()),
                    frame: Some(ir(&composite.frame)),
                    when: None,
                    unit: None,
                    conversion: None,
                });
            }
        }
    }
    let text = param_text(name, param, &uses);
    Ok(ParamUses {
        name: name.to_string(),
        param: serde_json::to_value(param).context("can't write the param as JSON")?,
        uses,
        text,
    })
}

/// Where the walk is: the path so far (`pos.`, `cells[].`) and the
/// conditions that must hold for it to be there.
#[derive(Default)]
struct Path {
    prefix: String,
    when: Vec<String>,
}

impl Path {
    /// Into an entry: its name, then `suffix`, with its condition.
    fn into(&self, entry: &Entry, suffix: &str) -> Path {
        let mut when = self.when.clone();
        if let Some(condition) = condition(entry) {
            when.push(describe::condition(condition));
        }
        let name = describe::entry_name(entry).unwrap_or_default();
        Path {
            prefix: format!("{}{name}{suffix}", self.prefix),
            when,
        }
    }

    fn when(&self) -> Option<String> {
        match self.when.as_slice() {
            [] => None,
            [one] => Some(one.clone()),
            many => Some(
                many.iter()
                    .map(|w| format!("({w})"))
                    .collect::<Vec<_>>()
                    .join(" and "),
            ),
        }
    }
}

/// An entry that sends a param.
struct Sent {
    entry: String,
    when: Option<String>,
    unit: Option<String>,
    conversion: Option<serde_json::Value>,
}

/// Every entry below `entries` that maps to `name`.
fn sends(
    entries: &[Entry],
    types: &BTreeMap<String, StructType>,
    name: &str,
    path: &Path,
    found: &mut Vec<Sent>,
) {
    let json = |c: &mdl::v2::ir::Conversion| serde_json::to_value(c).ok();
    for entry in entries {
        match entry {
            Entry::Field(field) if field.canonical.as_deref() == Some(name) => {
                let here = path.into(entry, "");
                found.push(Sent {
                    entry: here.prefix.clone(),
                    when: here.when(),
                    unit: field.unit.clone(),
                    conversion: field.conversion.as_ref().and_then(json),
                });
            }
            Entry::Array(array) => match &array.element {
                Element::Scalar(element) if array.canonical.as_deref() == Some(name) => {
                    let here = path.into(entry, "[]");
                    found.push(Sent {
                        entry: here.prefix.clone(),
                        when: here.when(),
                        unit: element.unit.clone(),
                        conversion: element.conversion.as_ref().and_then(json),
                    });
                }
                Element::Struct { name: ty, .. } => {
                    if let Some(struct_type) = types.get(ty) {
                        sends(
                            &struct_type.entries,
                            types,
                            name,
                            &path.into(entry, "[]."),
                            found,
                        );
                    }
                }
                _ => {}
            },
            Entry::Struct(inner) => {
                if let Some(struct_type) = types.get(&inner.struct_type) {
                    sends(
                        &struct_type.entries,
                        types,
                        name,
                        &path.into(entry, "."),
                        found,
                    );
                }
            }
            _ => {}
        }
    }
}

fn param_text(name: &str, param: &Param, uses: &[ParamUse]) -> String {
    let mut head = vec![ir(&param.ty)];
    if let Some(unit) = &param.unit {
        head.push(unit.clone());
    }
    if let Some(frame) = &param.frame {
        head.push(format!("frame {}", ir(frame)));
    }
    let mut text = format!("param {name}  {}\n", head.join(", "));
    if let Some(description) = &param.description {
        text.push_str(&format!("  {description}\n"));
    }
    if !param.states.is_empty() {
        text.push_str(&format!("  states {}\n", param.states.join(", ")));
    }
    if let Some(limits) = &param.limits {
        text.push_str(&format!("  limits {}\n", describe::limits(limits)));
    }
    if let Some(deprecated) = &param.deprecated {
        text.push_str(&format!("  deprecated: {deprecated}\n"));
    }
    if uses.is_empty() {
        text.push_str("\nno packet sends it\n");
        return text;
    }
    text.push_str("\nsent by\n");
    for one in uses {
        let mut notes = Vec::new();
        if let Some(unit) = &one.unit {
            notes.push(format!("unit {unit}"));
        }
        if let Some(conversion) = one
            .conversion
            .as_ref()
            .and_then(|c| serde_json::from_value(c.clone()).ok())
        {
            notes.push(describe::conversion(&conversion));
        }
        let what = match (&one.entry, &one.members) {
            (Some(entry), _) => format!("entry {entry}"),
            (None, Some(members)) => format!("composite of {}", members.join(", ")),
            (None, None) => String::new(),
        };
        if let Some(frame) = &one.frame {
            notes.push(format!("in {frame}"));
        }
        if let Some(when) = &one.when {
            notes.push(format!("when {when}"));
        }
        let mut line = format!("  set {}  packet {}  {what}", one.set, one.packet);
        if !notes.is_empty() {
            line.push_str(&format!("  ({})", notes.join("; ")));
        }
        text.push_str(&line);
        text.push('\n');
    }
    text
}

impl ParamUses {
    fn human(&self) -> String {
        self.text.clone()
    }
}

// ---- A command's encodings ----------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct CommandUses {
    name: String,
    /// The catalog's definition, as the IR writes it.
    command: serde_json::Value,
    packets: Vec<CommandUse>,
    #[serde(skip)]
    text: String,
}

/// One command packet that encodes the command.
#[derive(Debug, Serialize)]
struct CommandUse {
    set: String,
    packet: String,
    /// Each argument and the entry that encodes it (`slot` to `slot`,
    /// `waypoints` to `track[].x`).
    arguments: Vec<ArgumentUse>,
}

#[derive(Debug, Serialize)]
struct ArgumentUse {
    argument: String,
    entry: String,
}

fn command_uses(bundle: &Bundle, name: &str) -> anyhow::Result<CommandUses> {
    let Some(command) = bundle.catalog.commands.get(name) else {
        bail!("the catalog has no command {name}");
    };
    let mut packets = Vec::new();
    for set in &bundle.sets {
        for packet in set.commands.iter().filter(|c| c.canonical == name) {
            let mut arguments = Vec::new();
            encodes(&packet.entries, &set.types, "", &mut arguments);
            packets.push(CommandUse {
                set: set.name.clone(),
                packet: packet.name.clone(),
                arguments,
            });
        }
    }
    let text = command_text(name, command, &packets);
    Ok(CommandUses {
        name: name.to_string(),
        command: serde_json::to_value(command).context("can't write the command as JSON")?,
        packets,
        text,
    })
}

/// Every entry below `entries` that encodes an argument.
fn encodes(
    entries: &[Entry],
    types: &BTreeMap<String, StructType>,
    prefix: &str,
    found: &mut Vec<ArgumentUse>,
) {
    for entry in entries {
        match entry {
            Entry::Field(field) => {
                if let Some(argument) = &field.argument {
                    found.push(ArgumentUse {
                        argument: argument.clone(),
                        entry: format!("{prefix}{}", field.name),
                    });
                }
            }
            Entry::Array(array) => {
                if let Some(argument) = &array.argument {
                    found.push(ArgumentUse {
                        argument: argument.clone(),
                        entry: format!("{prefix}{}[]", array.name),
                    });
                }
                if let Element::Struct { name: ty, .. } = &array.element
                    && let Some(struct_type) = types.get(ty)
                {
                    let inner = format!("{prefix}{}[].", array.name);
                    encodes(&struct_type.entries, types, &inner, found);
                }
            }
            Entry::Struct(entry) => {
                if let Some(struct_type) = types.get(&entry.struct_type) {
                    let inner = format!("{prefix}{}.", entry.name);
                    encodes(&struct_type.entries, types, &inner, found);
                }
            }
            _ => {}
        }
    }
}

fn argument_text(argument: &Argument) -> String {
    let mut parts = vec![ir(&argument.ty)];
    if let Some(unit) = &argument.unit {
        parts.push(unit.clone());
    }
    if !argument.states.is_empty() {
        parts.push(format!("states {}", argument.states.join(", ")));
    }
    if let Some(range) = &argument.range {
        parts.push(format!("range {}", describe::bounds(range)));
    }
    if let Some(list) = &argument.list {
        parts.push(format!("list of {}", describe::bounds(list)));
    }
    if let Some(default) = &argument.default {
        parts.push(format!("default {}", ir(default)));
    }
    let mut text = format!("    {}  {}", argument.name, parts.join(", "));
    if let Some(description) = &argument.description {
        text.push_str(&format!("  ({description})"));
    }
    text
}

fn command_text(name: &str, command: &Command, packets: &[CommandUse]) -> String {
    let mut text = format!("command {name}\n");
    if let Some(description) = &command.description {
        text.push_str(&format!("  {description}\n"));
    }
    if let Some(deprecated) = &command.deprecated {
        text.push_str(&format!("  deprecated: {deprecated}\n"));
    }
    if command.arguments.is_empty() {
        text.push_str("  no arguments\n");
    } else {
        text.push_str("  arguments\n");
        for argument in &command.arguments {
            text.push_str(&argument_text(argument));
            text.push('\n');
        }
    }
    if packets.is_empty() {
        text.push_str("\nno command packet encodes it\n");
        return text;
    }
    text.push_str("\nencoded by\n");
    for packet in packets {
        let arguments: Vec<String> = packet
            .arguments
            .iter()
            .map(|a| {
                if a.argument == a.entry {
                    a.argument.clone()
                } else {
                    format!("{} in {}", a.argument, a.entry)
                }
            })
            .collect();
        let mut line = format!("  set {}  command {}", packet.set, packet.packet);
        if !arguments.is_empty() {
            line.push_str(&format!("  ({})", arguments.join(", ")));
        }
        text.push_str(&line);
        text.push('\n');
    }
    text
}

impl CommandUses {
    fn human(&self) -> String {
        self.text.clone()
    }
}
