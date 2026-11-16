//! The real-world XTCE corpus: every XTCE document under a directory
//! imported, what imports taken on as a consumer takes it, and its packets
//! decoded from synthesized payloads, real corpora shipping no captured
//! telemetry. The report says what each document gave. Only a bug in MDL
//! (a panic, or one of the fuzz targets' checks broken) fails the run; an
//! XTCE feature MDL doesn't take is reported, not failed.
//!
//! Each document is imported alone. One the importer refuses only for
//! references it can't resolve (`xtce.reference`) is imported again with
//! the other documents of its directory, since XTCE references can cross
//! files, leaving out those it refused as not XTCE or not well-formed; the
//! directory's import then stands for it in the counts.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use mdl::bindings::{Binding, Bindings, NativeId, Row};
use mdl::ir::{ByteOrder, Entry, Family, Length, Locator, Packet, Scalar, Set};
use mdl::layout::{self, ContainerLayout, LayoutError};
use mdl::message::{Message, MetaValue, Payload};
use mdl::route::FamilySets;
use mdl::source::{Severity, SourceDiagnostic, Sources};
use mdl::time::Timestamp;
use mdl::validate::{Validated, validate};
use mdl_xtce::Options;

use crate::messages::{check_decoded, check_error};
use crate::pipeline::{assemble, check_order, check_placed, check_spans};

/// What the corpus gave.
pub struct Report {
    root: PathBuf,
    /// `.xml` files with no SpaceSystem in them, which aren't XTCE.
    skipped: usize,
    /// Files and directories that couldn't be read, relative to the corpus.
    unreadable: Vec<String>,
    imports: Vec<Import>,
}

/// One import: a document alone, or a directory's documents together.
struct Import {
    /// The files, relative to the corpus, `/` between directories.
    files: Vec<String>,
    /// For a directory's documents: those of its documents left out, the
    /// importer having refused each alone as not XTCE or not well-formed.
    /// `None` for a document alone.
    together: Option<Vec<String>>,
    /// For a document alone: it was imported again with its directory,
    /// whose import stands for it.
    retried: bool,
    outcome: Outcome,
    /// The importer's warnings, by code.
    warnings: Codes,
    /// What MDL got wrong.
    bug: Option<String>,
}

/// Diagnostics by code: how many, and the first one's message.
type Codes = BTreeMap<String, (usize, String)>;

enum Outcome {
    /// A file isn't UTF-8 text.
    NotText,
    /// The importer refused it; its errors.
    Refused(Codes),
    /// It imported, and validation refused the bundle; its diagnostics.
    Invalid(Codes),
    Built(Built),
    /// A bug stopped it.
    Stopped,
}

/// A bundle that built.
struct Built {
    families: usize,
    sets: usize,
    packets: usize,
    commands: usize,
    /// What decoding the synthesized payloads gave: `decoded`, `decoded as
    /// another packet`, an error's code, or why a family wasn't decoded.
    decodes: BTreeMap<String, usize>,
}

/// The last panic's message and place, which the quiet hook keeps.
static LAST_PANIC: Mutex<Option<String>> = Mutex::new(None);

/// Held while a run has the panic hook, so runs at once (tests in one
/// process) take turns and each puts back the hook it found.
static RUNNING: Mutex<()> = Mutex::new(());

/// Imports every XTCE document under `root`, as the module docs say. While
/// it runs, a panic anywhere in the process is kept for the report instead
/// of printed.
pub fn run(root: &Path) -> Report {
    let _running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
    let mut xml = Vec::new();
    let mut unreadable = Vec::new();
    collect(root, root, &mut xml, &mut unreadable);
    let mut documents = Vec::new();
    let mut skipped = 0;
    for path in xml {
        match fs::read(&path) {
            Ok(bytes) if bytes.windows(11).any(|w| w == b"SpaceSystem") => documents.push(path),
            Ok(_) => skipped += 1,
            Err(_) => unreadable.push(relative(root, &path)),
        }
    }
    let previous = panic::take_hook();
    panic::set_hook(Box::new(|info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("a panic");
        let place = info
            .location()
            .map(|at| format!(" at {}:{}", at.file(), at.line()))
            .unwrap_or_default();
        if let Ok(mut last) = LAST_PANIC.lock() {
            *last = Some(format!("{message}{place}"));
        }
    }));
    let mut imports: Vec<Import> = documents
        .iter()
        .map(|document| import(root, std::slice::from_ref(document), None))
        .collect();
    // Each directory holding a document refused only for references, in
    // the order of the first such document.
    let mut directories: Vec<&Path> = Vec::new();
    for (document, alone) in documents.iter().zip(&imports) {
        let unresolved = matches!(&alone.outcome, Outcome::Refused(errors)
            if errors.keys().all(|code| code == "xtce.reference"));
        if let Some(directory) = document.parent()
            && unresolved
            && !directories.contains(&directory)
        {
            directories.push(directory);
        }
    }
    for directory in directories {
        let mut usable = Vec::new();
        let mut left_out = Vec::new();
        for (index, document) in documents.iter().enumerate() {
            if document.parent() != Some(directory) {
                continue;
            }
            if imports[index].not_xtce() {
                left_out.push(imports[index].files.join(", "));
            } else {
                usable.push(index);
            }
        }
        if usable.len() < 2 {
            continue;
        }
        let files: Vec<PathBuf> = usable.iter().map(|&i| documents[i].clone()).collect();
        for &index in &usable {
            imports[index].retried = imports[index].unresolved();
        }
        imports.push(import(root, &files, Some(left_out)));
    }
    panic::set_hook(previous);
    Report {
        root: root.to_path_buf(),
        skipped,
        unreadable,
        imports,
    }
}

/// A path relative to the corpus, `/` between directories; `.` for the
/// corpus itself.
fn relative(root: &Path, path: &Path) -> String {
    let parts: Vec<String> = path
        .strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

/// Every `.xml` file under `dir`, in name order, skipping hidden
/// directories (`.git`) and not following links to directories, which
/// could loop; what can't be read goes in `unreadable`.
fn collect(root: &Path, dir: &Path, out: &mut Vec<PathBuf>, unreadable: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        unreadable.push(relative(root, dir));
        return;
    };
    let mut found: Vec<(PathBuf, bool)> = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                let directory = entry.file_type().is_ok_and(|kind| kind.is_dir());
                found.push((entry.path(), directory));
            }
            // An entry of `dir` that can't be listed.
            Err(_) => unreadable.push(relative(root, dir)),
        }
    }
    found.sort();
    for (path, directory) in found {
        let hidden = path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with('.'));
        if directory {
            if !hidden {
                collect(root, &path, out, unreadable);
            }
        } else if path.extension().is_some_and(|ext| ext == "xml") && !path.is_dir() {
            out.push(path);
        }
    }
}

fn import(root: &Path, files: &[PathBuf], together: Option<Vec<String>>) -> Import {
    let mut import = Import {
        files: files.iter().map(|file| relative(root, file)).collect(),
        together,
        retried: false,
        outcome: Outcome::Stopped,
        warnings: Codes::new(),
        bug: None,
    };
    let mut sources = Sources::new();
    for (file, name) in files.iter().zip(&import.files) {
        match fs::read(file)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
        {
            Some(text) => {
                sources.add(name.clone(), text);
            }
            None => {
                import.outcome = Outcome::NotText;
                return import;
            }
        }
    }
    match panic::catch_unwind(AssertUnwindSafe(|| take(&sources))) {
        Ok((outcome, warnings)) => {
            import.outcome = outcome;
            import.warnings = warnings;
        }
        Err(_) => {
            let last = LAST_PANIC.lock().ok().and_then(|mut last| last.take());
            import.bug = Some(last.unwrap_or_else(|| "a panic".to_string()));
        }
    }
    import
}

impl Import {
    /// Refused only for references it couldn't resolve.
    fn unresolved(&self) -> bool {
        matches!(&self.outcome, Outcome::Refused(errors)
            if errors.keys().all(|code| code == "xtce.reference"))
    }

    /// Refused as not XTCE, not well-formed or not text, so no other
    /// document can help it.
    fn not_xtce(&self) -> bool {
        match &self.outcome {
            Outcome::NotText => true,
            Outcome::Refused(errors) => errors
                .keys()
                .any(|code| code == "xtce.document" || code == "xtce.syntax"),
            _ => false,
        }
    }
}

/// The documents imported, and taken on as far as they go; every check
/// the fuzz targets make on the way panics if it fails.
fn take(sources: &Sources) -> (Outcome, Codes) {
    match mdl_xtce::read(sources.iter(), &Options::default()) {
        Err(diagnostics) => {
            check_spans(sources, &diagnostics);
            check_order(&diagnostics);
            let errors = codes(
                diagnostics
                    .iter()
                    .filter(|d| d.severity == Severity::Error)
                    .map(|d| (d.code, d.message.as_str())),
            );
            let warnings = codes(warnings(&diagnostics));
            (Outcome::Refused(errors), warnings)
        }
        Ok(parsed) => {
            check_spans(sources, &parsed.warnings);
            check_order(&parsed.warnings);
            let warnings = codes(warnings(&parsed.warnings));
            let map = parsed.map;
            match validate(parsed.bundle) {
                Err(diagnostics) => {
                    check_placed(sources, &map, &diagnostics);
                    let found = codes(
                        diagnostics
                            .iter()
                            .map(|d| (d.code.as_str(), d.message.as_str())),
                    );
                    (Outcome::Invalid(found), warnings)
                }
                Ok(valid) => (Outcome::Built(built(&valid)), warnings),
            }
        }
    }
}

fn warnings(diagnostics: &[SourceDiagnostic]) -> impl Iterator<Item = (&'static str, &str)> {
    diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .map(|d| (d.code, d.message.as_str()))
}

fn codes<'c, 'm>(found: impl Iterator<Item = (&'c str, &'m str)>) -> Codes {
    let mut codes = Codes::new();
    for (code, message) in found {
        codes
            .entry(code.to_string())
            .or_insert_with(|| (0, message.to_string()))
            .0 += 1;
    }
    codes
}

/// A valid bundle built, read back and assembled, and each packet decoded
/// from synthesized payloads (see [`payloads`]).
fn built(valid: &Validated) -> Built {
    let assembled = assemble(valid);
    let bundle = valid.bundle();
    let params: BTreeSet<String> = bundle.catalog.params.keys().cloned().collect();
    let mut decodes = BTreeMap::new();
    for (name, sets) in &assembled {
        let Some(family) = bundle.families.iter().find(|family| family.name == *name) else {
            continue;
        };
        for set in bundle.sets.iter().filter(|set| set.family == family.name) {
            decode_set(valid, family, set, sets, &params, &mut decodes);
        }
    }
    Built {
        families: bundle.families.len(),
        sets: bundle.sets.len(),
        packets: bundle.sets.iter().map(|set| set.packets.len()).sum(),
        commands: bundle.sets.iter().map(|set| set.commands.len()).sum(),
        decodes,
    }
}

/// Each of the set's packets decoded from its payloads, the family's
/// native id bound to the set. Decoding a message twice gives the same; a
/// decoded message passes `check_decoded`, an error `check_error`.
fn decode_set(
    valid: &Validated,
    family: &Family,
    set: &Set,
    sets: &FamilySets,
    params: &BTreeSet<String>,
    decodes: &mut BTreeMap<String, usize>,
) {
    let mut note = |label: String| *decodes.entry(label).or_default() += 1;
    let Some(bindings) = binding(family, set) else {
        note("not decoded: the family's alias isn't fixed".to_string());
        return;
    };
    let layout = match layout::of_set(valid, &set.name) {
        Ok(layout) => layout,
        Err(LayoutError::SelfDescribing { .. }) => {
            note("not decoded: a JSON family".to_string());
            return;
        }
        Err(error) => panic!("set {}: {error}", set.name),
    };
    let ground = instant(RECEIVED);
    for packet in &set.packets {
        let Some(placed) = layout.packet(&packet.name) else {
            panic!("set {}: packet {} has no layout", set.name, packet.name);
        };
        for made in payloads(packet, placed, family) {
            let Synthesized {
                bytes,
                metadata,
                keyed,
            } = made;
            let metadata: Vec<(&str, MetaValue<'_>)> = metadata
                .iter()
                .map(|(name, value)| {
                    let value = match value {
                        Scalar::Int(n) => MetaValue::Int(*n),
                        Scalar::Str(text) => MetaValue::Text(text),
                    };
                    (name.as_str(), value)
                })
                .collect();
            let message = Message::new(Payload::Binary(&bytes), ground).with_metadata(&metadata);
            let first = sets.decode(&message, &bindings);
            let second = sets.decode(&message, &bindings);
            assert_eq!(
                format!("{first:?}"),
                format!("{second:?}"),
                "the same message decoded twice"
            );
            match first {
                Ok(decoded) => {
                    check_decoded(&decoded, params);
                    note(if decoded.packet.name == packet.name {
                        "decoded".to_string()
                    } else {
                        "decoded as another packet".to_string()
                    });
                }
                Err(error) => {
                    check_error(error.code(), &error);
                    // Every key written, the packet matches (decoding.md
                    // §4), unless a version picks another set.
                    assert!(
                        !(keyed && family.version.is_none() && error.code() == "no_packet"),
                        "set {}: a payload holding every identify key of packet {} matched no \
                         packet",
                        set.name,
                        packet.name
                    );
                    note(error.code().to_string());
                }
            }
        }
    }
}

/// When the bound asset's binding takes effect (2000-01-01) and when every
/// message is received (2026-01-01), in seconds since the Unix epoch.
const BOUND: i64 = 946_684_800;
const RECEIVED: i64 = 1_767_225_600;

fn instant(seconds: i64) -> Timestamp {
    match Timestamp::new(seconds, 0) {
        Some(instant) => instant,
        None => panic!("Timestamp::new refuses {seconds} s, an instant MDL covers"),
    }
}

/// The family's fixed native id bound to an asset, which the set decodes
/// if the family has no version locator; `None` if the alias isn't fixed.
/// A validated family's fixed alias always binds (validation refuses an
/// empty one, and the names are validated), so a refusal is a bug.
fn binding(family: &Family, set: &Set) -> Option<Bindings> {
    let Locator::Fixed { value, .. } = &family.alias else {
        return None;
    };
    let mut binding = Binding::new("CORPUS");
    if family.version.is_none() {
        binding = binding.with_set(&set.name);
    }
    let row = Row::bound(
        &family.name,
        NativeId::from_text(value.as_str()),
        instant(BOUND),
        binding,
    );
    match Bindings::new(vec![row]) {
        Ok(bindings) => Some(bindings),
        Err(refused) => panic!(
            "the bindings refuse family {}'s fixed alias {value:?}: {refused}",
            family.name
        ),
    }
}

/// A payload made for a packet, and the metadata it's delivered with.
struct Synthesized {
    bytes: Vec<u8>,
    metadata: Vec<(String, Scalar)>,
    /// Whether every identify key is written in: none is a JSON Pointer,
    /// a text field value or a field without a static position.
    keyed: bool,
}

/// The packet's payloads: zeros, ones and three seeded random ones, each
/// as long as the packet can be at least (its entries' least reach, or its
/// declared length if more), with its identify keys written in: a field
/// key at the field's static position, a metadata key as the message's
/// metadata. JSON Pointer keys don't arise in a binary family.
fn payloads(packet: &Packet, placed: &ContainerLayout, family: &Family) -> Vec<Synthesized> {
    let declared = match packet.length {
        Some(Length::Exact { bytes, .. } | Length::AtLeast { bytes, .. }) => u64::from(bytes) * 8,
        _ => 0,
    };
    let bytes = placed.size_min_bits.max(declared).div_ceil(8).min(1 << 16);
    let length = usize::try_from(bytes).unwrap_or(0);
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    let mut random = || {
        (0..length)
            .map(|_| {
                // xorshift64
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed.to_le_bytes()[0]
            })
            .collect::<Vec<u8>>()
    };
    let fills = [
        vec![0; length],
        vec![0xFF; length],
        random(),
        random(),
        random(),
    ];
    fills
        .into_iter()
        .map(|mut bytes| {
            let mut metadata = Vec::new();
            let mut keyed = true;
            for (key, value) in &packet.identify {
                if family.metadata.contains(key) {
                    metadata.push((key.clone(), value.clone()));
                    continue;
                }
                let Scalar::Int(n) = value else {
                    keyed = false;
                    continue;
                };
                let field = placed
                    .with_entries(&packet.entries)
                    .find_map(|(entry, placement)| match entry {
                        Entry::Field(field) if field.name == *key => Some((field, placement)),
                        _ => None,
                    });
                let Some((field, placement)) = field else {
                    keyed = false;
                    continue;
                };
                let (Some(offset), Some(size)) =
                    (placement.static_offset_bits, placement.size_bits())
                else {
                    keyed = false;
                    continue;
                };
                let order = field.byte_order.or(family.byte_order);
                keyed &= write_bits(
                    &mut bytes,
                    offset,
                    size,
                    n.cast_unsigned(),
                    order == Some(ByteOrder::Little),
                );
            }
            Synthesized {
                bytes,
                metadata,
                keyed,
            }
        })
        .collect()
}

/// `value`'s low `size` bits written at bit `offset`: most significant
/// first, or a little-endian field of whole bytes least significant byte
/// first (decoding.md §2.1; validation refuses any other little-endian
/// field). Whether they fit: bits past the payload are left out.
fn write_bits(bytes: &mut [u8], offset: u64, size: u64, value: u64, little: bool) -> bool {
    let fits = usize::try_from(offset.saturating_add(size).div_ceil(8))
        .is_ok_and(|end| end <= bytes.len());
    if size == 0 || size > 64 {
        return false;
    }
    if little && offset.is_multiple_of(8) && size.is_multiple_of(8) {
        for (index, byte) in value
            .to_le_bytes()
            .iter()
            .take((size / 8) as usize)
            .enumerate()
        {
            if let Some(at) = usize::try_from(offset / 8)
                .ok()
                .and_then(|start| bytes.get_mut(start + index))
            {
                *at = *byte;
            }
        }
        return fits;
    }
    for index in 0..size {
        let bit = (value >> (size - 1 - index)) & 1;
        let at = offset + index;
        let Some(byte) = usize::try_from(at / 8)
            .ok()
            .and_then(|at| bytes.get_mut(at))
        else {
            continue;
        };
        let mask = 0x80 >> (at % 8);
        if bit == 1 {
            *byte |= mask;
        } else {
            *byte &= !mask;
        }
    }
    fits
}

impl Report {
    /// The bugs in MDL the corpus found.
    pub fn bugs(&self) -> usize {
        self.imports
            .iter()
            .filter(|import| import.bug.is_some())
            .count()
    }

    /// The imports that count: all but the documents imported again with
    /// their directory, whose import stands for them.
    fn counted(&self) -> impl Iterator<Item = &Import> {
        self.imports.iter().filter(|import| !import.retried)
    }

    /// How many imports built.
    pub fn built(&self) -> usize {
        self.counted()
            .filter(|import| matches!(import.outcome, Outcome::Built(_)))
            .count()
    }

    /// How many synthesized payloads decoded as the packet they were made
    /// for.
    pub fn decoded(&self) -> usize {
        self.imports
            .iter()
            .filter_map(|import| match &import.outcome {
                Outcome::Built(built) => built.decodes.get("decoded").copied(),
                _ => None,
            })
            .sum()
    }

    /// A line or two for the terminal.
    pub fn summary(&self) -> String {
        let count = |test: fn(&Outcome) -> bool| {
            self.counted()
                .filter(|import| test(&import.outcome))
                .count()
        };
        let retried = self.imports.len() - self.counted().count();
        let retried = match retried {
            0 => String::new(),
            1 => " (and a document retried with its directory)".to_string(),
            n => format!(" (and {n} documents retried with their directories)"),
        };
        format!(
            "{} imports{retried}: {} built, {} refused by the importer, {} refused by \
             validation, {} not text; {} payloads decoded; {} bugs in MDL",
            self.counted().count(),
            self.built(),
            count(|o| matches!(o, Outcome::Refused(_))),
            count(|o| matches!(o, Outcome::Invalid(_))),
            count(|o| matches!(o, Outcome::NotText)),
            self.decoded(),
            self.bugs()
        )
    }

    /// The report, as Markdown.
    pub fn markdown(&self) -> String {
        let mut out = String::new();
        let documents = self
            .imports
            .iter()
            .filter(|import| import.together.is_none())
            .count();
        let together = self.imports.len() - documents;
        out.push_str("# XTCE corpus\n\n");
        out.push_str(&format!(
            "`{}`. Documents: {documents}. Directories also imported together: {together}. \
             `.xml` files skipped for want of a SpaceSystem: {}. Unreadable files and \
             directories: {}.\n\n",
            self.root.display(),
            self.skipped,
            self.unreadable.len()
        ));
        out.push_str(&format!("{}\n\n", self.summary()));
        if !self.unreadable.is_empty() {
            out.push_str("## Unreadable\n\n");
            for path in &self.unreadable {
                out.push_str(&format!("- `{path}`\n"));
            }
            out.push('\n');
        }
        out.push_str("## Bugs in MDL\n\n");
        let bugs: Vec<&Import> = self
            .imports
            .iter()
            .filter(|import| import.bug.is_some())
            .collect();
        if bugs.is_empty() {
            out.push_str("None.\n\n");
        }
        for import in bugs {
            out.push_str(&format!(
                "- `{}`: {}\n",
                import.files.join("`, `"),
                cell(import.bug.as_deref().unwrap_or_default())
            ));
        }
        self.totals(
            &mut out,
            "What the importer refused",
            |o| match o {
                Outcome::Refused(codes) => Some(codes),
                _ => None,
            },
            |_| None,
        );
        self.totals(
            &mut out,
            "What validation refused in imported bundles",
            |o| match o {
                Outcome::Invalid(codes) => Some(codes),
                _ => None,
            },
            |_| None,
        );
        self.totals(
            &mut out,
            "What the importer left out (warnings)",
            |_| None,
            |i| Some(&i.warnings),
        );
        let mut decodes: BTreeMap<&String, usize> = BTreeMap::new();
        for import in &self.imports {
            if let Outcome::Built(built) = &import.outcome {
                for (label, count) in &built.decodes {
                    *decodes.entry(label).or_default() += count;
                }
            }
        }
        out.push_str("\n## Decoding synthesized payloads\n\n");
        out.push_str("| Outcome | Payloads |\n| --- | --- |\n");
        for (label, count) in decodes {
            out.push_str(&format!("| {} | {count} |\n", cell(label)));
        }
        out.push_str("\n## Imports\n\n");
        out.push_str("| Documents | Outcome | Warnings |\n| --- | --- | --- |\n");
        for import in &self.imports {
            let files = if let Some(left_out) = &import.together {
                // A directory's documents: the directory, then their names.
                let dir = import
                    .files
                    .first()
                    .and_then(|file| file.rsplit_once('/'))
                    .map_or(".", |(dir, _)| dir);
                let names: Vec<&str> = import
                    .files
                    .iter()
                    .map(|file| {
                        file.rsplit_once('/')
                            .map_or(file.as_str(), |(_, name)| name)
                    })
                    .collect();
                let mut files = format!("{dir}/ together: {}", names.join(", "));
                if !left_out.is_empty() {
                    let left_out: Vec<&str> = left_out
                        .iter()
                        .map(|file| {
                            file.rsplit_once('/')
                                .map_or(file.as_str(), |(_, name)| name)
                        })
                        .collect();
                    files.push_str(&format!(" (left out: {})", left_out.join(", ")));
                }
                files
            } else {
                import.files.join(", ")
            };
            let outcome = match (&import.outcome, &import.bug) {
                (_, Some(bug)) => format!("**bug**: {bug}"),
                (Outcome::NotText, _) => "not UTF-8".to_string(),
                (Outcome::Refused(codes), _) if import.retried => format!(
                    "refused alone: {}; retried with its directory",
                    listed(codes)
                ),
                (Outcome::Refused(codes), _) => format!("refused: {}", listed(codes)),
                (Outcome::Invalid(codes), _) => format!("invalid: {}", listed(codes)),
                (Outcome::Stopped, _) => "stopped".to_string(),
                (Outcome::Built(built), _) => {
                    let decodes = if built.decodes.is_empty() {
                        "none".to_string()
                    } else {
                        built
                            .decodes
                            .iter()
                            .map(|(label, count)| format!("{label} {count}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    format!(
                        "built: families {}, sets {}, packets {}, commands {}; decodes: {decodes}",
                        built.families, built.sets, built.packets, built.commands,
                    )
                }
            };
            out.push_str(&format!(
                "| {} | {} | {} |\n",
                escaped(&files),
                cell(&outcome),
                cell(&listed(&import.warnings))
            ));
        }
        out
    }

    /// A table of codes across the imports that count: in how many each
    /// came, how many times, and the first message.
    fn totals(
        &self,
        out: &mut String,
        title: &str,
        of: fn(&Outcome) -> Option<&Codes>,
        extra: fn(&Import) -> Option<&Codes>,
    ) {
        let mut totals: BTreeMap<&String, (usize, usize, &String)> = BTreeMap::new();
        for import in self.counted() {
            for codes in [of(&import.outcome), extra(import)].into_iter().flatten() {
                for (code, (count, message)) in codes {
                    let total = totals.entry(code).or_insert((0, 0, message));
                    total.0 += 1;
                    total.1 += count;
                }
            }
        }
        out.push_str(&format!("\n## {title}\n\n"));
        if totals.is_empty() {
            out.push_str("None.\n");
            return;
        }
        let mut rows: Vec<_> = totals.into_iter().collect();
        rows.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(b.0)));
        out.push_str("| Code | Imports | Times | First message |\n| --- | --- | --- | --- |\n");
        for (code, (imports, times, message)) in rows {
            out.push_str(&format!(
                "| `{code}` | {imports} | {times} | {} |\n",
                cell(message)
            ));
        }
    }
}

/// Codes with their counts: `xtce.unsupported 3, xtce.reference 1`.
fn listed(codes: &Codes) -> String {
    codes
        .iter()
        .map(|(code, (count, _))| format!("{code} {count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A message for a Markdown table cell: at most 240 characters, as
/// [`escaped`].
fn cell(text: &str) -> String {
    let short: String = text.chars().take(240).collect();
    escaped(&short)
}

/// Text for a Markdown table cell: one line, `|` escaped.
fn escaped(text: &str) -> String {
    let line: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    line.replace('|', "\\|")
}
