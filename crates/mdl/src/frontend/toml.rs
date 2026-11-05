//! The TOML syntax of MDL: serde structs that mirror the files exactly as
//! written (the AST), and the translation into the IR.
//!
//! Closed sets of words (encodings, types, frames) are enums, so a typo like
//! `type = "unit"` fails at parse time with the line number. Anything that
//! refers to something else (a canonical name, a field in a composite) stays
//! a string; `crate::validate` checks it, the same way for every frontend.
//!
//! AST - Abstract Syntax Tree "the source as written, parsed into a data structure, before anything has been checked for meaning"
//! TOML text ──parse──► AST (as written) ──validate──► IR (checked, resolved) ──► plan
//! This is specifically for the TOML and at the end it's the parsed form of the TOML

// sorted map
pub use crate::ir::{Encoding, Endian, FieldType, Frame, IdentValue, Limits, ParamType};
use std::collections::BTreeMap;

use serde::Deserialize;

use std::path::Path;

use crate::diagnostics::{Diagnostic, Errors, Report};
use crate::ir::{
    self, Alias, BitRange, CanonicalParam, Composite, DefinitionSet, Definitions, Field, Location,
    Packet, Time,
};

// ---- catalog.toml -----------------------------------------------------------

/// `definitions/catalog.toml`: the canonical model, shared by every set.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogFile {
    pub canonical: Vec<CanonicalDef>,
}

// One `[[canonical]]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalDef {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: ParamType,
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub states: Vec<String>,
    pub frame: Option<Frame>,
    pub limits: Option<Limits>,
    #[serde(default)]
    pub description: String,
}

// ---- sets/<set>.toml ---------------------------------------------------------

/// One `definitions/sets/<set>.toml`: a definition set.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetFile {
    pub set: SetDef,
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
    #[serde(rename = "packet")]
    pub packets: Vec<PacketDef>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetDef {
    pub name: String,
    /// How the payload is written once the link removes the envelope:
    /// "binary" or "json".
    pub encoding: Encoding,
    /// Binary encoding only
    pub endian: Option<Endian>,
    pub time: Option<TimeDef>,
    /// Where the vendor-natice asset id lives in each packet (apid, sysid)
    pub alias: Option<Locator>,
}

/// How a packet's event time is found. `kind` picks the variant:
/// `time = { kind = "cuc", ... }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TimeDef {
    /// CCSDS unsegmented time code: whole seconds, then a binary fraction.
    Cuc {
        offset_bits: u32,
        coarse_bytes: u8,
        fine_bytes: u8,
        /// RFC 3339, e.g. "2000-01-01T00:00:00Z".
        epoch: String,
    },
    /// An ISO 8601 string at a JSON Pointer path, e.g. "/t".
    Iso8601 { path: String },
    /// Milliseconds since boot (each packet names its field with
    /// `time_field`), turned into absolute time by a correlation packet that
    /// carries both clocks.
    BootMs { correlate: Correlate },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Correlate {
    pub packet: String,
    pub unix_us: String,
    pub boot_ms: String,
}

/// Where something is in a packet. Exactly one form is allowed, so the keys
/// are all optional here and the translation picks the one used:
/// `offset_bits` + `size_bits` (binary payload), `metadata` (a value the
/// link's stack provides, e.g. MAVLink "sysid"), or `path` (a JSON Pointer).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Locator {
    pub offset_bits: Option<u32>,
    pub size_bits: Option<u32>,
    pub metadata: Option<String>,
    pub path: Option<String>,
}

/// One `[[packet]]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PacketDef {
    pub name: String,
    /// Values that must all match for a message to be this packet. Keys are
    /// JSON Pointers (`"/ch"`) for json; for binary, either names of this
    /// packet's own fields or metadata the link provides (`apid`, `msgid`):
    /// `{ apid = 0x064 }`, `{ msgid = 1 }`, `{ "/ch" = "PWR.BUS.V" }`,
    /// `{ frame_type = 7 }`.
    pub identify: BTreeMap<String, IdentValue>,
    pub length_bytes: Option<u32>,
    pub time_field: Option<String>,
    #[serde(default, rename = "field")]
    pub fields: Vec<FieldDef>,
    #[serde(default, rename = "composite")]
    pub composites: Vec<CompositeDef>,
}

/// One `[[packet.field]]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldDef {
    pub name: String,
    /// Binary: bits from the start of the payload.
    pub offset_bits: Option<u32>,
    pub size_bits: Option<u32>,
    /// JSON lines: a JSON Pointer, e.g. "/v" or "/v/0".
    pub path: Option<String>,
    #[serde(rename = "type")]
    pub ty: FieldType,
    pub conversion: Option<Conversion>,
    /// The unit after conversion. Must match the canonical param's unit.
    pub unit: Option<String>,
    pub canonical: Option<String>,
    /// Raw values that mean "no reading" (e.g. MAVLink's -1). Consumers
    /// mark them bad instead of converting them.
    #[serde(default)]
    pub invalid: Vec<i64>,
}

/// Raw value to engineering value. Written as a one-key table:
/// `{ linear = { scale = 0.01 } }`, `{ poly = [0.0, 0.5] }`,
/// `{ states = { 0 = "SAFE", 1 = "NOMINAL" } }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Conversion {
    /// `raw * scale + offset`
    Linear {
        #[serde(default = "one")]
        scale: f64,
        #[serde(default)]
        offset: f64,
    },
    /// Polynomial coefficients, lowest power first: c0 + c1*x + c2*x^2 ...
    Poly(Vec<f64>),
    /// Raw integer (TOML keys are strings, so "0") to state name.
    States(BTreeMap<String, String>),
}

fn one() -> f64 {
    1.0
}

/// One `[[packet.composite]]`: several fields become one vec3 or quat sample.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositeDef {
    pub canonical: String,
    /// The frame the source reports in; a consumer converts to the catalog's.
    pub frame: Frame,
    /// x, y, z for a vec3; w, x, y, z for a quat.
    pub fields: Vec<String>,
}

// ---- Translation into the IR --------------------------------------------------
//
// Only *format* problems are reported here: a locator with the wrong mix of
// keys, a state key that isn't a number, an unparseable epoch. Whether the
// definitions make sense (sizes, units, the catalog, encodings) is checked by
// `crate::validate`, the same way for every frontend.

/// One TOML file: its name, for error messages, and its text.
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub name: String,
    pub text: String,
}

/// Read `dir/catalog.toml` and every `dir/sets/*.toml`, and translate them.
pub fn load_dir(dir: &Path) -> Result<Definitions, Errors> {
    let catalog = read_source(&dir.join("catalog.toml"))?;
    let sets_dir = dir.join("sets");
    let mut set_paths = Vec::new();
    for entry in std::fs::read_dir(&sets_dir).map_err(|err| io_error(&sets_dir, err))? {
        let path = entry.map_err(|err| io_error(&sets_dir, err))?.path();
        if path.extension().is_some_and(|ext| ext == "toml") {
            set_paths.push(path);
        }
    }
    // Directory order is arbitrary; sort so errors come out the same every time.
    set_paths.sort();
    let sets = set_paths
        .iter()
        .map(|path| read_source(path))
        .collect::<Result<Vec<_>, _>>()?;
    load(&catalog, &sets)
}

/// Translate files that are already read. Tests call this with strings.
pub fn load(catalog: &SourceFile, sets: &[SourceFile]) -> Result<Definitions, Errors> {
    let mut report = Report::new(&catalog.name);
    let mut definitions = Definitions {
        mdl_version: ir::MDL_VERSION,
        catalog: BTreeMap::new(),
        sets: BTreeMap::new(),
    };
    match toml::from_str::<CatalogFile>(&catalog.text) {
        Ok(file) => definitions.catalog = translate_catalog(file, &mut report),
        Err(err) => report.error("", err.to_string()),
    }

    for source in sets {
        report.source = source.name.clone();
        let file = match toml::from_str::<SetFile>(&source.text) {
            Ok(file) => file,
            Err(err) => {
                report.error("", err.to_string());
                continue;
            }
        };
        // One set per file, named after it, so a validation error about set
        // `vendor_a` always points at vendor_a.toml.
        let expected = format!("{}.toml", file.set.name);
        if source.name != expected {
            report.error(
                "[set]",
                format!(
                    "defines set {:?}, so the file should be named {expected}",
                    file.set.name
                ),
            );
        }
        if let Some(set) = translate_set(file, &mut report) {
            if definitions.sets.contains_key(&set.name) {
                report.error("[set]", format!("set {:?} is defined twice", set.name));
            } else {
                definitions.sets.insert(set.name.clone(), set);
            }
        }
    }
    report.finish(definitions)
}

fn read_source(path: &Path) -> Result<SourceFile, Errors> {
    let text = std::fs::read_to_string(path).map_err(|err| io_error(path, err))?;
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    Ok(SourceFile { name, text })
}

fn io_error(path: &Path, err: std::io::Error) -> Errors {
    Errors(vec![Diagnostic {
        source: path.display().to_string(),
        location: String::new(),
        message: err.to_string(),
    }])
}

fn translate_catalog(file: CatalogFile, report: &mut Report) -> BTreeMap<String, CanonicalParam> {
    let mut catalog = BTreeMap::new();
    for def in file.canonical {
        // A map can't hold the same key twice, so this one is caught here.
        if catalog.contains_key(&def.name) {
            report.error(format!("canonical {}", def.name), "defined twice");
            continue;
        }
        let param = CanonicalParam {
            name: def.name.clone(),
            ty: def.ty,
            unit: def.unit,
            states: def.states,
            frame: def.frame,
            limits: def.limits,
            description: def.description,
        };
        catalog.insert(def.name, param);
    }
    catalog
}

fn translate_set(file: SetFile, report: &mut Report) -> Option<DefinitionSet> {
    let before = report.count();
    let SetFile {
        set,
        aliases,
        packets,
    } = file;
    let alias = translate_alias(set.alias, &aliases, report);
    let time = translate_time(set.time, report);
    let packets: Vec<Packet> = packets
        .into_iter()
        .filter_map(|packet| translate_packet(packet, report))
        .collect();

    if report.count() > before {
        return None;
    }
    Some(DefinitionSet {
        name: set.name,
        encoding: set.encoding,
        endian: set.endian,
        time: time?,
        alias: alias?,
        aliases,
        packets,
    })
}

/// The loose `Locator` becomes exactly one `Alias` variant. Whether that
/// variant suits the encoding is validation's call.
fn translate_alias(
    locator: Option<Locator>,
    aliases: &BTreeMap<String, String>,
    report: &mut Report,
) -> Option<Alias> {
    let place = "[set] alias";
    let Some(locator) = locator else {
        // No locator: a single-asset source, named by its one alias.
        if aliases.len() == 1 {
            return aliases.values().next().map(|asset| Alias::Fixed {
                asset: asset.clone(),
            });
        }
        report.error(
            place,
            format!(
                "without an alias locator, [aliases] must have exactly one entry; it has {}",
                aliases.len()
            ),
        );
        return None;
    };
    match (
        locator.offset_bits,
        locator.size_bits,
        locator.metadata,
        locator.path,
    ) {
        (Some(offset_bits), Some(size_bits), None, None) => Some(Alias::Bits(BitRange {
            offset_bits,
            size_bits,
        })),
        (None, None, Some(name), None) => Some(Alias::Metadata { name }),
        (None, None, None, Some(pointer)) => Some(Alias::Path { pointer }),
        _ => {
            report.error(
                place,
                "use exactly one of: offset_bits + size_bits, metadata, or path",
            );
            None
        }
    }
}

fn translate_time(time: Option<TimeDef>, report: &mut Report) -> Option<Time> {
    match time {
        None => Some(Time::None),
        Some(TimeDef::Cuc {
            offset_bits,
            coarse_bytes,
            fine_bytes,
            epoch,
        }) => {
            let Some(epoch_unix_ns) = chrono::DateTime::parse_from_rfc3339(&epoch)
                .ok()
                .and_then(|t| t.timestamp_nanos_opt())
            else {
                report.error(
                    "[set] time",
                    format!("epoch {epoch:?} isn't an RFC 3339 time like \"2000-01-01T00:00:00Z\""),
                );
                return None;
            };
            Some(Time::Cuc {
                offset_bits,
                coarse_bytes,
                fine_bytes,
                epoch_unix_ns,
            })
        }
        Some(TimeDef::Iso8601 { path }) => Some(Time::Iso8601 { path }),
        Some(TimeDef::BootMs { correlate }) => Some(Time::BootMs {
            correlate: ir::Correlate {
                packet: correlate.packet,
                unix_us: correlate.unix_us,
                boot_ms: correlate.boot_ms,
            },
        }),
    }
}

fn translate_packet(packet: PacketDef, report: &mut Report) -> Option<Packet> {
    let before = report.count();
    let place = format!("packet {}", packet.name);
    let fields: Vec<Field> = packet
        .fields
        .into_iter()
        .filter_map(|field| translate_field(&place, field, report))
        .collect();
    let composites = packet
        .composites
        .into_iter()
        .map(|c| Composite {
            canonical: c.canonical,
            frame: c.frame,
            fields: c.fields,
        })
        .collect();

    if report.count() > before {
        return None;
    }
    Some(Packet {
        name: packet.name,
        identify: packet.identify,
        length_bytes: packet.length_bytes,
        time_field: packet.time_field,
        fields,
        composites,
    })
}

fn translate_field(packet_place: &str, field: FieldDef, report: &mut Report) -> Option<Field> {
    let before = report.count();
    let place = format!("{packet_place}, field {}", field.name);
    let location = match (field.offset_bits, field.size_bits, field.path) {
        (Some(offset_bits), Some(size_bits), None) => Some(Location::Bits(BitRange {
            offset_bits,
            size_bits,
        })),
        (None, None, Some(pointer)) => Some(Location::Path { pointer }),
        _ => {
            report.error(
                &place,
                "use offset_bits + size_bits (binary) or path (json), not a mix",
            );
            None
        }
    };
    let conversion = field
        .conversion
        .map(|conversion| translate_conversion(conversion, &place, report));

    if report.count() > before {
        return None;
    }
    Some(Field {
        name: field.name,
        location: location?,
        ty: field.ty,
        conversion,
        unit: field.unit,
        canonical: field.canonical,
        invalid: field.invalid,
    })
}

/// Same shapes, except state keys become real integers. A key that isn't one
/// is reported and left out.
fn translate_conversion(
    conversion: Conversion,
    place: &str,
    report: &mut Report,
) -> ir::Conversion {
    match conversion {
        Conversion::Linear { scale, offset } => ir::Conversion::Linear { scale, offset },
        Conversion::Poly(coefficients) => ir::Conversion::Poly { coefficients },
        Conversion::States(states) => {
            let mut parsed = Vec::new();
            for (raw, name) in states {
                match raw.parse::<i64>() {
                    Ok(raw) => parsed.push(ir::State { raw, name }),
                    Err(_) => report.error(place, format!("state key {raw:?} isn't an integer")),
                }
            }
            // TOML keys sort as text ("10" before "2"); order by number.
            parsed.sort_by_key(|state| state.raw);
            ir::Conversion::States { states: parsed }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VENDOR: &str = r#"
        [set]
        name     = "vendor_a"
        encoding = "binary"
        endian   = "big"
        time     = { kind = "cuc", offset_bits = 0, coarse_bytes = 4, fine_bytes = 2, epoch = "2000-01-01T00:00:00Z" }
        alias    = { offset_bits = 48, size_bits = 16 }

        [aliases]
        "101" = "SAT-1"

        [[packet]]
        name         = "EPS_HK"
        identify     = { apid = 0x064 }
        length_bytes = 15

        [[packet.field]]
        name        = "bus_voltage_counts"
        offset_bits = 64
        size_bits   = 16
        type        = "uint"
        conversion  = { linear = { scale = 0.01 } }
        unit        = "V"
        canonical   = "eps.bus_voltage"

        [[packet.field]]
        name        = "mode"
        offset_bits = 118
        size_bits   = 2
        type        = "uint"
        conversion  = { states = { 0 = "SAFE", 1 = "NOMINAL", 2 = "PAYLOAD" } }
        canonical   = "eps.mode"

        [[packet.composite]]
        canonical = "nav.pos_ecef"
        frame     = "ECI_J2000"
        fields    = ["x", "y", "z"]
    "#;

    #[test]
    fn a_vendor_file_parses() {
        let file: SetFile = toml::from_str(VENDOR).unwrap();
        assert_eq!(file.set.encoding, Encoding::Binary);
        assert!(matches!(
            file.set.time,
            Some(TimeDef::Cuc {
                coarse_bytes: 4,
                ..
            })
        ));
        assert_eq!(file.aliases["101"], "SAT-1");
        let packet = &file.packets[0];
        assert_eq!(packet.identify["apid"], IdentValue::Int(0x064));
        assert_eq!(packet.fields[0].ty, FieldType::Uint);
        assert!(matches!(
            packet.fields[0].conversion,
            Some(Conversion::Linear { scale, offset }) if scale == 0.01 && offset == 0.0
        ));
        assert!(
            matches!(&packet.fields[1].conversion, Some(Conversion::States(s)) if s["1"] == "NOMINAL")
        );
        assert_eq!(packet.composites[0].frame, Frame::EciJ2000);
    }

    #[test]
    fn identify_takes_integers_or_strings_under_any_key() {
        let file: SetFile = toml::from_str(
            r#"
            [set]
            name     = "vendor_b"
            encoding = "json"
            alias    = { path = "/sat" }

            [[packet]]
            name     = "PWR_BUS_V"
            identify = { "/ch" = "PWR.BUS.V" }

            [[packet]]
            name     = "LEGACY_HK"
            identify = { frame_type = 7 }
            "#,
        )
        .unwrap();
        assert_eq!(file.set.encoding, Encoding::Json);
        assert!(file.set.alias.is_some());
        assert_eq!(
            file.packets[0].identify["/ch"],
            IdentValue::Str("PWR.BUS.V".into())
        );
        assert_eq!(file.packets[1].identify["frame_type"], IdentValue::Int(7));
    }

    #[test]
    fn a_catalog_parses() {
        let file: CatalogFile = toml::from_str(
            r#"
            [[canonical]]
            name   = "eps.bus_voltage"
            type   = "f64"
            unit   = "V"
            limits = { red_low = 6.2, red_high = 8.5 }

            [[canonical]]
            name   = "eps.mode"
            type   = "enum"
            states = ["SAFE", "NOMINAL", "PAYLOAD"]
            "#,
        )
        .unwrap();
        assert_eq!(file.canonical[1].ty, ParamType::Enum);
        assert_eq!(file.canonical[0].limits.unwrap().red_high, Some(8.5));
        assert_eq!(file.canonical[1].unit, "");
    }

    #[test]
    fn typos_fail_at_parse_time_with_a_line_number() {
        let typo = VENDOR.replace(
            "type        = \"uint\"\n        conversion  = { linear",
            "type        = \"unit\"\n        conversion  = { linear",
        );
        let error = toml::from_str::<SetFile>(&typo).unwrap_err().to_string();
        assert!(error.contains("unit"), "{error}");
        assert!(error.contains("line"), "{error}");

        let unknown_key = VENDOR.replace("length_bytes = 15", "length_bytes = 15\nlenght = 3");
        let error = toml::from_str::<SetFile>(&unknown_key)
            .unwrap_err()
            .to_string();
        assert!(error.contains("lenght"), "{error}");
    }

    // ---- Translation ----

    fn source(name: &str, text: &str) -> SourceFile {
        SourceFile {
            name: name.into(),
            text: text.into(),
        }
    }

    const CATALOG: &str = r#"
        [[canonical]]
        name = "eps.bus_voltage"
        type = "f64"
        unit = "V"
    "#;

    fn load_vendor(text: &str) -> Result<Definitions, Errors> {
        load(
            &source("catalog.toml", CATALOG),
            &[source("vendor_a.toml", text)],
        )
    }

    #[test]
    fn a_set_translates_into_the_ir() {
        let definitions = load_vendor(VENDOR).unwrap();
        let set = &definitions.sets["vendor_a"];
        assert_eq!(
            set.alias,
            Alias::Bits(BitRange {
                offset_bits: 48,
                size_bits: 16
            })
        );
        assert!(matches!(
            set.time,
            Time::Cuc {
                epoch_unix_ns: 946_684_800_000_000_000,
                ..
            }
        ));
        let mode = &set.packets[0].fields[1];
        assert_eq!(
            mode.location,
            Location::Bits(BitRange {
                offset_bits: 118,
                size_bits: 2
            })
        );
        assert!(
            matches!(&mode.conversion, Some(ir::Conversion::States { states }) if states[1].raw == 1 && states[1].name == "NOMINAL")
        );
        assert_eq!(definitions.catalog["eps.bus_voltage"].unit, "V");
    }

    #[test]
    fn a_single_alias_without_a_locator_is_a_fixed_asset() {
        let text = VENDOR.replace("alias    = { offset_bits = 48, size_bits = 16 }\n", "");
        let definitions = load_vendor(&text).unwrap();
        assert_eq!(
            definitions.sets["vendor_a"].alias,
            Alias::Fixed {
                asset: "SAT-1".into()
            }
        );
    }

    #[test]
    fn format_mistakes_are_reported_together_with_their_place() {
        let text = VENDOR
            .replace(
                "alias    = { offset_bits = 48, size_bits = 16 }",
                "alias    = { offset_bits = 48, metadata = \"sysid\" }",
            )
            .replace("0 = \"SAFE\"", "zero = \"SAFE\"")
            .replace("2000-01-01T00:00:00Z", "Jan 1 2000");
        let errors = load_vendor(&text).unwrap_err();
        let lines: Vec<String> = errors.0.iter().map(ToString::to_string).collect();
        assert_eq!(
            lines,
            [
                "vendor_a.toml: [set] alias: use exactly one of: offset_bits + size_bits, metadata, or path",
                "vendor_a.toml: [set] time: epoch \"Jan 1 2000\" isn't an RFC 3339 time like \"2000-01-01T00:00:00Z\"",
                "vendor_a.toml: packet EPS_HK, field mode: state key \"zero\" isn't an integer",
            ]
        );
    }

    #[test]
    fn a_field_needs_bits_or_a_path_not_a_mix() {
        let text = VENDOR.replace(
            "offset_bits = 64\n",
            "offset_bits = 64\n        path = \"/v\"\n",
        );
        let errors = load_vendor(&text).unwrap_err().to_string();
        assert!(
            errors.contains("field bus_voltage_counts: use offset_bits + size_bits"),
            "{errors}"
        );
    }

    #[test]
    fn file_names_must_match_set_names() {
        let errors = load(
            &source("catalog.toml", CATALOG),
            &[source("sat1.toml", VENDOR)],
        )
        .unwrap_err()
        .to_string();
        assert!(errors.contains("should be named vendor_a.toml"), "{errors}");
    }

    #[test]
    fn duplicate_params_and_parse_errors_name_the_file() {
        let doubled = format!("{CATALOG}{CATALOG}");
        let errors = load(
            &source("catalog.toml", &doubled),
            &[source("vendor_a.toml", "[set]\nname = 3\n")],
        )
        .unwrap_err();
        assert_eq!(
            errors.0[0].to_string(),
            "catalog.toml: canonical eps.bus_voltage: defined twice"
        );
        assert!(
            errors.0[1].to_string().starts_with("vendor_a.toml: "),
            "{errors}"
        );
    }

    #[test]
    fn load_dir_reads_the_catalog_and_every_file_in_sets() {
        let dir = std::env::temp_dir().join(format!("mdl-load-dir-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sets")).unwrap();
        std::fs::write(dir.join("catalog.toml"), CATALOG).unwrap();
        std::fs::write(dir.join("sets/vendor_a.toml"), VENDOR).unwrap();
        std::fs::write(dir.join("sets/README.md"), "not a definition").unwrap();
        let loaded = load_dir(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(loaded.unwrap().sets.len(), 1);

        let missing = load_dir(Path::new("/definitely/not/here"))
            .unwrap_err()
            .to_string();
        assert!(missing.contains("catalog.toml"), "{missing}");
    }
}
