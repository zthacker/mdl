//! Validation: IR → Plan.
//!
//! Every check on what the definitions *mean* lives here and runs on the IR,
//! so definitions from any frontend (TOML today; XTCE, C headers or a
//! database later) are held to the same rules. Every problem is collected,
//! so one `morphc build` lists everything wrong, and the only way to get a
//! `Plan` is to pass. Diagnostics name the set ("vendor_a"); the TOML
//! frontend keeps each set in a file of the same name.
//!
//! Nothing here knows about envelopes (CCSDS, MAVLink): names a definition
//! uses for link metadata (`apid`, `sysid`) are checked against the link's
//! stack when a consumer pairs a link with a set, not here.

use std::collections::{BTreeMap, BTreeSet};

use crate::bits::byte_order_fits;
use crate::diagnostics::{Errors, Report};
use crate::ir::{
    Alias, BitRange, CanonicalParam, Composite, Conversion, DefinitionSet, Definitions, Encoding,
    Endian, Field, FieldType, Frame, IdentValue, Limits, Location, MDL_VERSION, Packet, ParamType,
    Time,
};
use crate::plan::Plan;

/// Check everything; return a `Plan` only if there's nothing wrong.
pub fn validate(definitions: Definitions) -> Result<Plan, Errors> {
    // A document in a format version we don't know can't be read reliably,
    // so nothing else is checked.
    if definitions.mdl_version != MDL_VERSION {
        let mut report = Report::new("ir");
        report.error(
            "mdl_version",
            format!(
                "this compiler reads MDL IR version {MDL_VERSION}, got {}",
                definitions.mdl_version
            ),
        );
        return conclude(report, definitions);
    }

    let mut report = Report::new("catalog");
    for param in definitions.catalog.values() {
        check_catalog_entry(param, &mut report);
    }

    // An asset belongs to exactly one definition set.
    let mut owners: BTreeMap<&str, &str> = BTreeMap::new();
    for set in definitions.sets.values() {
        report.source = set.name.clone();
        check_set(set, &definitions.catalog, &mut report);
        for asset in set.aliases.values() {
            if let Some(other) = owners.insert(asset, &set.name)
                && other != set.name
            {
                report.error(
                    "[aliases]",
                    format!(
                        "asset {asset} is also claimed by set {other}; an asset belongs to one set"
                    ),
                );
            }
        }
    }
    conclude(report, definitions)
}

/// Every problem found, or the plan if there were none.
fn conclude(report: Report, definitions: Definitions) -> Result<Plan, Errors> {
    report.finish(())?;
    Plan::new(definitions)
}

// ---- Catalog ------------------------------------------------------------------

fn check_catalog_entry(param: &CanonicalParam, report: &mut Report) {
    let place = format!("canonical {}", param.name);
    if !is_canonical_name(&param.name) {
        report.error(
            &place,
            "names are lowercase and dot-separated, like eps.bus_voltage",
        );
    }
    if (param.ty == ParamType::Enum) == param.states.is_empty() {
        report.error(
            &place,
            "states are required for enum params and only allowed there",
        );
    }
    let composite = matches!(param.ty, ParamType::Vec3 | ParamType::Quat);
    if composite != param.frame.is_some() {
        report.error(
            &place,
            "frame is required for vec3 and quat params and only allowed there",
        );
    }
    if let Some(limits) = &param.limits {
        if !matches!(param.ty, ParamType::F64 | ParamType::I64) {
            report.error(&place, "limits only apply to f64 and i64 params");
        }
        let values = [
            limits.red_low,
            limits.yellow_low,
            limits.yellow_high,
            limits.red_high,
        ];
        if values.iter().flatten().any(|n| !n.is_finite()) {
            report.error(&place, "limits must be finite, not nan or inf");
        } else if !limits_ordered(limits) {
            report.error(
                &place,
                "limits must be ordered red_low <= yellow_low <= yellow_high <= red_high",
            );
        }
    }
}

// ---- Definition sets ------------------------------------------------------------

fn check_set(set: &DefinitionSet, catalog: &BTreeMap<String, CanonicalParam>, report: &mut Report) {
    let binary = set.encoding == Encoding::Binary;
    if !is_token(&set.name) {
        report.error(
            "[set]",
            format!("name {:?} must be lowercase letters, digits and '_' (set names end up in subjects, topics and file names)", set.name),
        );
    }
    match (binary, set.endian) {
        (true, None) => report.error("[set]", "endian is required for binary payloads"),
        (false, Some(_)) => report.error("[set]", "endian doesn't apply to json"),
        _ => {}
    }
    check_alias(set, report);
    check_time(set, report);

    let mut names = BTreeSet::new();
    for packet in &set.packets {
        if !names.insert(packet.name.as_str()) {
            report.error(format!("packet {}", packet.name), "defined twice");
        }
        check_packet(set, packet, catalog, report);
    }
    check_identify_unique(set, report);
}

fn check_alias(set: &DefinitionSet, report: &mut Report) {
    let place = "[set] alias";
    match &set.alias {
        Alias::Fixed { .. } => {}
        Alias::Bits(range) => {
            if set.encoding == Encoding::Json {
                report.error(place, "json sets locate the alias with path, not bits");
            } else if !(1..=64).contains(&range.size_bits) {
                report.error(place, "an alias can be 1 to 64 bits");
            } else {
                check_byte_order(range, set.endian, place, report);
                for packet in &set.packets {
                    if packet_bits(packet).is_some_and(|bits| end_bits(range) > bits) {
                        report.error(
                            place,
                            format!(
                                "bits {}..{} don't fit in packet {}",
                                range.offset_bits,
                                end_bits(range) - 1,
                                packet.name
                            ),
                        );
                    }
                }
            }
        }
        Alias::Metadata { name } => {
            if !is_token(name) {
                report.error(
                    place,
                    format!("metadata name {name:?} must be lowercase letters, digits and '_'"),
                );
            }
        }
        Alias::Path { pointer } => {
            if set.encoding != Encoding::Json {
                report.error(place, "only json sets locate the alias with path");
            }
            check_pointer(pointer, place, report);
        }
    }

    if set.aliases.is_empty() {
        report.error("[aliases]", "at least one alias is required");
    }
    let mut assets = BTreeSet::new();
    for (vendor_id, asset) in &set.aliases {
        if !is_asset_id(asset) {
            report.error(
                "[aliases]",
                format!("{asset:?} isn't a valid asset id (letters, digits, '_' and '-', up to 32, starting with a letter or digit)"),
            );
        }
        if !assets.insert(asset.as_str()) {
            report.error(
                "[aliases]",
                format!("{vendor_id:?} maps to {asset}, which another alias already names"),
            );
        }
    }
}

fn check_time(set: &DefinitionSet, report: &mut Report) {
    let place = "[set] time";
    match &set.time {
        Time::None => {}
        Time::Cuc {
            offset_bits,
            coarse_bytes,
            fine_bytes,
            ..
        } => {
            if set.encoding == Encoding::Json {
                report.error(place, "cuc time needs a binary payload");
            }
            if !(1..=4).contains(coarse_bytes) || *fine_bytes > 3 {
                report.error(
                    place,
                    format!("cuc needs 1-4 coarse bytes and 0-3 fine bytes, got {coarse_bytes} and {fine_bytes}"),
                );
            }
            let end =
                u64::from(*offset_bits) + 8 * (u64::from(*coarse_bytes) + u64::from(*fine_bytes));
            for packet in &set.packets {
                if packet_bits(packet).is_some_and(|bits| end > bits) {
                    report.error(
                        place,
                        format!(
                            "the time code ends at bit {end}, past the end of packet {}",
                            packet.name
                        ),
                    );
                }
            }
        }
        Time::Iso8601 { path } => {
            if set.encoding != Encoding::Json {
                report.error(place, "iso8601 time needs a json payload");
            }
            check_pointer(path, place, report);
        }
        Time::BootMs { correlate } => match set.packets.iter().find(|p| p.name == correlate.packet)
        {
            None => report.error(
                place,
                format!("correlation packet {:?} isn't defined", correlate.packet),
            ),
            Some(packet) => {
                for name in [&correlate.unix_us, &correlate.boot_ms] {
                    if !has_field(packet, name) {
                        report.error(
                            place,
                            format!("correlation packet {} has no field {name:?}", packet.name),
                        );
                    }
                }
            }
        },
    }

    let boot_ms = matches!(set.time, Time::BootMs { .. });
    for packet in &set.packets {
        if let Some(name) = &packet.time_field {
            let place = format!("packet {}", packet.name);
            if !boot_ms {
                report.error(&place, "time_field only applies to boot_ms time");
            } else if !has_field(packet, name) {
                report.error(
                    &place,
                    format!("time_field {name:?} isn't a field of this packet"),
                );
            }
        }
    }
}

// ---- Packets --------------------------------------------------------------------

fn check_packet(
    set: &DefinitionSet,
    packet: &Packet,
    catalog: &BTreeMap<String, CanonicalParam>,
    report: &mut Report,
) {
    let place = format!("packet {}", packet.name);
    let binary = set.encoding == Encoding::Binary;
    check_identify(set.encoding, packet, &place, report);
    match (binary, packet.length_bytes) {
        (true, None) => report.error(&place, "length_bytes is required for binary payloads"),
        (true, Some(0)) => report.error(&place, "length_bytes must be at least 1"),
        (false, Some(_)) => report.error(&place, "length_bytes doesn't apply to json"),
        _ => {}
    }

    let mut names = BTreeSet::new();
    for field in &packet.fields {
        let field_place = format!("{place}, field {}", field.name);
        if !names.insert(field.name.as_str()) {
            report.error(&field_place, "defined twice");
        }
        check_field(
            set.encoding,
            set.endian,
            packet,
            field,
            &field_place,
            catalog,
            report,
        );
    }
    check_overlaps(packet, &place, report);
    for composite in &packet.composites {
        check_composite(packet, composite, &place, catalog, report);
    }
}

/// json: keys are JSON Pointers. binary: a key naming one of this packet's
/// own fields matches that decoded field (it must be an integer); any other
/// key is link metadata (`apid`, `msgid`), checked against the link by the consumer.
fn check_identify(encoding: Encoding, packet: &Packet, place: &str, report: &mut Report) {
    if packet.identify.is_empty() {
        report.error(place, "identify needs at least one key");
    }
    for (key, value) in &packet.identify {
        match encoding {
            Encoding::Json => {
                if !key.starts_with('/') {
                    report.error(
                        place,
                        format!("identify key {key:?} must be a JSON Pointer, like \"/ch\""),
                    );
                }
            }
            Encoding::Binary => {
                match packet.fields.iter().find(|f| &f.name == key) {
                    Some(field) => {
                        let integer_field = matches!(field.ty, FieldType::Uint | FieldType::Int);
                        if !integer_field || !matches!(value, IdentValue::Int(_)) {
                            report.error(
                            place,
                            format!("identify {key} must be an integer field matched against an integer"),
                        );
                        }
                    }
                    None => {
                        if !is_token(key) {
                            report.error(
                            place,
                            format!("identify key {key:?} is neither a field of this packet nor a metadata name (lowercase letters, digits and '_')"),
                        );
                        }
                    }
                }
            }
        }
    }
}

/// Two packets with the same identify keys and values would both match
/// every such message.
fn check_identify_unique(set: &DefinitionSet, report: &mut Report) {
    for (i, packet) in set.packets.iter().enumerate() {
        for earlier in &set.packets[..i] {
            if !packet.identify.is_empty() && packet.identify == earlier.identify {
                report.error(
                    format!("packet {}", packet.name),
                    format!(
                        "identifies exactly like packet {}; every message would match both",
                        earlier.name
                    ),
                );
            }
        }
    }
}

/// Bit fields must not share bits. Sorted by start, each field is compared
/// with the one reaching furthest so far, so a long field that covers
/// several later ones is caught too.
fn check_overlaps(packet: &Packet, place: &str, report: &mut Report) {
    let mut ranges: Vec<(u64, u64, &str)> = packet
        .fields
        .iter()
        .filter_map(|field| match &field.location {
            Location::Bits(range) => Some((
                u64::from(range.offset_bits),
                end_bits(range),
                field.name.as_str(),
            )),
            Location::Path { .. } => None,
        })
        .collect();
    ranges.sort();

    let mut furthest: Option<(u64, &str)> = None;
    for (start, end, name) in ranges {
        if let Some((reach, other)) = furthest
            && start < reach
        {
            report.error(
                place,
                format!("fields {other} and {name} overlap at bit {start}"),
            );
        }
        if furthest.is_none_or(|(reach, _)| end > reach) {
            furthest = Some((end, name));
        }
    }
}

// ---- Fields ---------------------------------------------------------------------

/// The build-time twin of `extract_bits`'s runtime check (`bits::byte_order_fits`).
fn check_byte_order(range: &BitRange, endian: Option<Endian>, place: &str, report: &mut Report) {
    if let Some(endian) = endian
        && !byte_order_fits(range.offset_bits, range.size_bits, endian)
    {
        report.error(
            place,
            format!(
                "bits {}..{} cross a byte boundary without being whole bytes; \
                 little-endian fields must be whole bytes on byte boundaries or fit in one byte",
                range.offset_bits,
                end_bits(range) - 1
            ),
        );
    }
}

fn check_field(
    encoding: Encoding,
    endian: Option<Endian>,
    packet: &Packet,
    field: &Field,
    place: &str,
    catalog: &BTreeMap<String, CanonicalParam>,
    report: &mut Report,
) {
    match (&field.location, encoding) {
        (Location::Bits(_), Encoding::Json) => {
            report.error(place, "json fields are located with path, not bits")
        }
        (Location::Path { pointer }, Encoding::Json) => check_pointer(pointer, place, report),
        (Location::Path { .. }, Encoding::Binary) => report.error(
            place,
            "binary fields are located with offset_bits and size_bits",
        ),
        (Location::Bits(range), _) => {
            let size_ok = match field.ty {
                FieldType::Uint | FieldType::Int => (1..=64).contains(&range.size_bits),
                FieldType::Float => range.size_bits == 32 || range.size_bits == 64,
                FieldType::Bool => range.size_bits == 1,
                FieldType::String => false,
            };
            if !size_ok {
                report.error(
                    place,
                    format!(
                        "a {} field can't be {} bits (uint/int: 1-64, float: 32 or 64, bool: 1; strings only in json)",
                        type_name(field.ty),
                        range.size_bits
                    ),
                );
            }
            check_byte_order(range, endian, place, report);
            if let Some(bits) = packet_bits(packet)
                && end_bits(range) > bits
            {
                report.error(
                    place,
                    format!(
                        "bits {}..{} run past the {bits}-bit data field",
                        range.offset_bits,
                        end_bits(range) - 1
                    ),
                );
            }
        }
    }

    if let Some(conversion) = &field.conversion {
        check_conversion(field.ty, conversion, place, report);
    }
    if !field.invalid.is_empty() && !matches!(field.ty, FieldType::Uint | FieldType::Int) {
        report.error(place, "invalid values only apply to integer fields");
    }
    if let Some(name) = &field.canonical {
        match catalog.get(name) {
            None => report.error(place, format!("canonical {name:?} isn't in the catalog")),
            Some(param) => check_canonical(field, param, place, report),
        }
    }
}

fn check_conversion(ty: FieldType, conversion: &Conversion, place: &str, report: &mut Report) {
    let numbers = match conversion {
        Conversion::Linear { scale, offset } => vec![*scale, *offset],
        Conversion::Poly { coefficients } => coefficients.clone(),
        Conversion::States { .. } => Vec::new(),
    };
    // TOML allows nan and inf; JSON can't carry them, so the plan couldn't
    // be written or hashed faithfully.
    if numbers.iter().any(|n| !n.is_finite()) {
        report.error(place, "conversion numbers must be finite, not nan or inf");
    }
    let numeric = matches!(ty, FieldType::Uint | FieldType::Int | FieldType::Float);
    match conversion {
        Conversion::Linear { .. } if !numeric => {
            report.error(
                place,
                format!(
                    "linear conversion needs a numeric field, not {}",
                    type_name(ty)
                ),
            );
        }
        Conversion::Poly { .. } if !numeric => {
            report.error(
                place,
                format!(
                    "poly conversion needs a numeric field, not {}",
                    type_name(ty)
                ),
            );
        }
        Conversion::Poly { coefficients } if coefficients.is_empty() => {
            report.error(place, "poly needs at least one coefficient");
        }
        Conversion::States { .. } if !matches!(ty, FieldType::Uint | FieldType::Int) => {
            report.error(
                place,
                format!("states need an integer field, not {}", type_name(ty)),
            );
        }
        Conversion::States { states } if states.is_empty() => {
            report.error(place, "states need at least one entry");
        }
        Conversion::States { states } => {
            let mut seen = BTreeSet::new();
            for state in states {
                if !seen.insert(state.raw) {
                    report.error(place, format!("raw value {} has two states", state.raw));
                }
            }
        }
        _ => {}
    }
}

/// Does this field produce what its canonical param holds, in its unit?
fn check_canonical(field: &Field, param: &CanonicalParam, place: &str, report: &mut Report) {
    let numeric = matches!(
        field.ty,
        FieldType::Uint | FieldType::Int | FieldType::Float
    );
    let states = matches!(field.conversion, Some(Conversion::States { .. }));
    let (fits, requirement) = match param.ty {
        ParamType::F64 => (
            numeric && !states,
            "an f64, so the field must be uint, int or float, without a states conversion",
        ),
        ParamType::I64 => (
            matches!(field.ty, FieldType::Uint | FieldType::Int) && field.conversion.is_none(),
            "an i64, so the field must be uint or int with no conversion",
        ),
        ParamType::Bool => (
            field.ty == FieldType::Bool && field.conversion.is_none(),
            "a bool, so the field must be bool with no conversion",
        ),
        ParamType::Str => (
            field.ty == FieldType::String && field.conversion.is_none(),
            "a str, so the field must be string with no conversion",
        ),
        // A string field with no conversion must already carry the state
        // names (SAT-2 sends "NOMINAL"); consumers decide what to do with other values.
        ParamType::Enum => (
            states || (field.ty == FieldType::String && field.conversion.is_none()),
            "an enum, so the field needs a states conversion (or must be a string carrying the state names)",
        ),
        ParamType::Vec3 | ParamType::Quat => {
            report.error(
                place,
                format!(
                    "{} is a vec3 or quat: map it with a [[packet.composite]], not a single field",
                    param.name
                ),
            );
            return;
        }
    };
    if !fits {
        report.error(
            place,
            format!("can't map to {}: it's {requirement}", param.name),
        );
    }
    if let Some(Conversion::States { states: map }) = &field.conversion
        && param.ty == ParamType::Enum
    {
        for state in map {
            if !param.states.contains(&state.name) {
                report.error(
                    place,
                    format!(
                        "state {:?} isn't one of {}'s states {:?}",
                        state.name, param.name, param.states
                    ),
                );
            }
        }
    }
    check_unit(
        field.unit.as_deref(),
        &param.unit,
        &param.name,
        place,
        report,
    );
}

/// The field's unit (after conversion) must be exactly `expected`: the
/// catalog's unit, or for a geodetic composite member, deg or m.
fn check_unit(unit: Option<&str>, expected: &str, param: &str, place: &str, report: &mut Report) {
    let unit = unit.unwrap_or("");
    if unit == expected {
        return;
    }
    let message = if expected.is_empty() {
        format!("{param} has no unit, but this field says {unit:?}")
    } else if unit.is_empty() {
        format!(
            "unit is missing; {param} needs {expected:?} (convert to it and set unit = {expected:?})"
        )
    } else {
        format!("unit {unit:?} doesn't match {param} in {expected:?}; convert to {expected:?}")
    };
    report.error(place, message);
}

fn check_composite(
    packet: &Packet,
    composite: &Composite,
    packet_place: &str,
    catalog: &BTreeMap<String, CanonicalParam>,
    report: &mut Report,
) {
    let place = format!("{packet_place}, composite {}", composite.canonical);
    let Some(param) = catalog.get(&composite.canonical) else {
        report.error(&place, "isn't in the catalog");
        return;
    };
    let (expected, members) = match param.ty {
        ParamType::Vec3 => (3, "x, y, z"),
        ParamType::Quat => (4, "w, x, y, z"),
        _ => {
            report.error(
                &place,
                format!(
                    "{} isn't a vec3 or quat; map it with a single field",
                    param.name
                ),
            );
            return;
        }
    };
    if composite.fields.len() != expected {
        report.error(
            &place,
            format!(
                "needs {expected} fields ({members}), got {}",
                composite.fields.len()
            ),
        );
    }
    if let Some(target) = param.frame
        && !frame_converts(composite.frame, target)
    {
        report.error(
            &place,
            format!(
                "frame {:?} can't be converted to {}'s frame {target:?}",
                composite.frame, param.name
            ),
        );
    }
    // Members carry the catalog's unit, except geodetic positions:
    // latitude and longitude in degrees, then altitude in the catalog's unit.
    let geodetic = composite.frame == Frame::Geodetic && param.ty == ParamType::Vec3;
    for (i, name) in composite.fields.iter().enumerate() {
        let expected_unit = match (geodetic, i) {
            (true, 0 | 1) => "deg",
            _ => param.unit.as_str(),
        };
        let member_place = format!("{place}, field {name}");
        match packet.fields.iter().find(|f| &f.name == name) {
            None => report.error(&member_place, "isn't a field of this packet"),
            Some(field) => {
                let numeric = matches!(
                    field.ty,
                    FieldType::Uint | FieldType::Int | FieldType::Float
                );
                if !numeric || matches!(field.conversion, Some(Conversion::States { .. })) {
                    report.error(&member_place, "composite members must be numeric");
                }
                check_unit(
                    field.unit.as_deref(),
                    expected_unit,
                    &param.name,
                    &member_place,
                    report,
                );
            }
        }
    }
}

/// The frame conversions consumers are expected to support: anything to itself, and
/// ECI J2000, geodetic or NED to ECEF.
fn frame_converts(from: Frame, to: Frame) -> bool {
    from == to
        || (to == Frame::Ecef && matches!(from, Frame::EciJ2000 | Frame::Geodetic | Frame::Ned))
}

// ---- Small helpers ----------------------------------------------------------------

/// One past the last bit of a range.
fn end_bits(range: &BitRange) -> u64 {
    u64::from(range.offset_bits) + u64::from(range.size_bits)
}

/// A binary packet's data field in bits.
fn packet_bits(packet: &Packet) -> Option<u64> {
    packet.length_bytes.map(|bytes| u64::from(bytes) * 8)
}

fn has_field(packet: &Packet, name: &str) -> bool {
    packet.fields.iter().any(|f| f.name == name)
}

fn type_name(ty: FieldType) -> &'static str {
    match ty {
        FieldType::Uint => "uint",
        FieldType::Int => "int",
        FieldType::Float => "float",
        FieldType::Bool => "bool",
        FieldType::String => "string",
    }
}

/// Lowercase letters, digits and `_`: safe in subjects, topics and file names.
fn is_token(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// `subsystem.name`: at least two dot-separated parts, each starting with a
/// lowercase letter, then lowercase letters, digits and `_`.
fn is_canonical_name(name: &str) -> bool {
    let parts: Vec<&str> = name.split('.').collect();
    parts.len() >= 2
        && parts.iter().all(|part| {
            part.starts_with(|c: char| c.is_ascii_lowercase())
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
}

/// The `assets` table's rule (migration 000003): a letter or digit, then up
/// to 31 letters, digits, '_' or '-'. Checked here so a definition can't
/// name an asset the database would reject.
fn is_asset_id(id: &str) -> bool {
    let mut chars = id.chars();
    id.len() <= 32
        && chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The limits that are present must not decrease from red_low to red_high.
fn limits_ordered(limits: &Limits) -> bool {
    let present: Vec<f64> = [
        limits.red_low,
        limits.yellow_low,
        limits.yellow_high,
        limits.red_high,
    ]
    .into_iter()
    .flatten()
    .collect();
    present.windows(2).all(|pair| pair[0] <= pair[1])
}

/// JSON Pointers (RFC 6901) start with `/`.
fn check_pointer(path: &str, place: &str, report: &mut Report) {
    if !path.starts_with('/') {
        report.error(
            place,
            format!("path {path:?} must be a JSON Pointer starting with '/', like \"/v\""),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::toml::{SourceFile, load};

    const CATALOG: &str = r#"
        [[canonical]]
        name   = "eps.bus_voltage"
        type   = "f64"
        unit   = "V"
        limits = { red_low = 6.2, red_high = 8.5 }

        [[canonical]]
        name   = "eps.mode"
        type   = "enum"
        states = ["SAFE", "NOMINAL", "PAYLOAD"]

        [[canonical]]
        name  = "nav.pos_ecef"
        type  = "vec3"
        unit  = "m"
        frame = "ECEF"
    "#;

    const VENDOR_A: &str = r#"
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

        [[packet]]
        name         = "NAV"
        identify     = { apid = 0x066 }
        length_bytes = 20

        [[packet.field]]
        name        = "pos_x"
        offset_bits = 64
        size_bits   = 32
        type        = "int"
        unit        = "m"

        [[packet.field]]
        name        = "pos_y"
        offset_bits = 96
        size_bits   = 32
        type        = "int"
        unit        = "m"

        [[packet.field]]
        name        = "pos_z"
        offset_bits = 128
        size_bits   = 32
        type        = "int"
        unit        = "m"

        [[packet.composite]]
        canonical = "nav.pos_ecef"
        frame     = "ECI_J2000"
        fields    = ["pos_x", "pos_y", "pos_z"]
    "#;

    fn source(name: &str, text: &str) -> SourceFile {
        SourceFile {
            name: name.into(),
            text: text.into(),
        }
    }

    /// The tests build IR with the TOML frontend because it's convenient;
    /// validation itself never sees TOML.
    fn definitions(set: &str) -> Definitions {
        load(
            &source("catalog.toml", CATALOG),
            &[source("vendor_a.toml", set)],
        )
        .unwrap_or_else(|errors| panic!("test fixture must load:\n{errors}"))
    }

    /// Validate `VENDOR_A` with each `(from, to)` edit applied; the errors.
    fn problems_after(edits: &[(&str, &str)]) -> String {
        let mut set = VENDOR_A.to_string();
        for (from, to) in edits {
            assert!(set.contains(from), "test bug: {from:?} not in VENDOR_A");
            set = set.replacen(from, to, 1);
        }
        validate(definitions(&set))
            .expect_err("expected validation errors")
            .to_string()
    }

    #[test]
    fn valid_definitions_become_a_plan() {
        let plan = validate(definitions(VENDOR_A)).unwrap();
        assert_eq!(plan.definitions().sets["vendor_a"].packets.len(), 2);
    }

    #[test]
    fn little_endian_is_fine_on_whole_bytes_and_inside_one_byte() {
        // Every VENDOR_A field is whole aligned bytes, except mode, which
        // sits inside byte 14; both are readable little-endian.
        let little = VENDOR_A.replacen("endian   = \"big\"", "endian   = \"little\"", 1);
        assert!(validate(definitions(&little)).is_ok());
    }

    #[test]
    fn mistakes_in_meaning_are_explained() {
        let cases: &[(&[(&str, &str)], &str)] = &[
            (
                &[("offset_bits = 118", "offset_bits = 70")],
                "fields bus_voltage_counts and mode overlap at bit 70",
            ),
            (
                &[("length_bytes = 15", "length_bytes = 10")],
                "run past the 80-bit data field",
            ),
            (
                &[("unit        = \"V\"", "unit        = \"mV\"")],
                "unit \"mV\" doesn't match eps.bus_voltage in \"V\"",
            ),
            (
                &[("unit        = \"V\"\n", "")],
                "unit is missing; eps.bus_voltage needs \"V\"",
            ),
            (
                &[(
                    "canonical   = \"eps.bus_voltage\"",
                    "canonical   = \"eps.bus_volts\"",
                )],
                "canonical \"eps.bus_volts\" isn't in the catalog",
            ),
            (
                &[("2 = \"PAYLOAD\"", "2 = \"SCIENCE\"")],
                "state \"SCIENCE\" isn't one of eps.mode's states",
            ),
            (
                &[(
                    "type        = \"uint\"\n        conversion  = { linear",
                    "type        = \"float\"\n        conversion  = { linear",
                )],
                "a float field can't be 16 bits",
            ),
            (
                &[(
                    "canonical   = \"eps.mode\"",
                    "canonical   = \"eps.bus_voltage\"",
                )],
                "can't map to eps.bus_voltage: it's an f64",
            ),
            (
                &[("{ scale = 0.01 }", "{ scale = nan }")],
                "conversion numbers must be finite, not nan or inf",
            ),
            (
                &[("endian   = \"big\"\n", "")],
                "endian is required for binary payloads",
            ),
            (
                &[(
                    "alias    = { offset_bits = 48, size_bits = 16 }",
                    "alias    = { path = \"/sat\" }",
                )],
                "only json sets locate the alias with path",
            ),
            (
                &[(
                    "identify     = { apid = 0x064 }",
                    "identify     = { bus_voltage_counts = \"high\" }",
                )],
                "identify bus_voltage_counts must be an integer field matched against an integer",
            ),
            (
                &[(
                    "identify     = { apid = 0x066 }",
                    "identify     = { apid = 0x064 }",
                )],
                "identifies exactly like packet EPS_HK",
            ),
            (
                &[(
                    "fields    = [\"pos_x\", \"pos_y\", \"pos_z\"]",
                    "fields    = [\"pos_x\", \"pos_y\"]",
                )],
                "needs 3 fields (x, y, z), got 2",
            ),
            (
                &[("frame     = \"ECI_J2000\"", "frame     = \"BODY\"")],
                "frame Body can't be converted to nav.pos_ecef's frame Ecef",
            ),
            (
                &[
                    ("endian   = \"big\"", "endian   = \"little\""),
                    ("offset_bits = 64", "offset_bits = 60"),
                ],
                "bits 60..75 cross a byte boundary without being whole bytes",
            ),
            (
                &[
                    ("endian   = \"big\"", "endian   = \"little\""),
                    (
                        "alias    = { offset_bits = 48, size_bits = 16 }",
                        "alias    = { offset_bits = 44, size_bits = 16 }",
                    ),
                ],
                "[set] alias: bits 44..59 cross a byte boundary",
            ),
            (
                &[("\"101\" = \"SAT-1\"", "\"101\" = \"SAT 1\"")],
                "\"SAT 1\" isn't a valid asset id",
            ),
        ];
        for (edits, expected) in cases {
            let problems = problems_after(edits);
            assert!(
                problems.contains(expected),
                "expected {expected:?} in:\n{problems}"
            );
        }
    }

    #[test]
    fn every_problem_is_reported_with_its_set_and_place() {
        let problems = problems_after(&[
            ("offset_bits = 118", "offset_bits = 70"),
            ("unit        = \"V\"", "unit        = \"mV\""),
        ]);
        let lines: Vec<&str> = problems.lines().collect();
        assert_eq!(
            lines,
            [
                "vendor_a: packet EPS_HK, field bus_voltage_counts: unit \"mV\" doesn't match eps.bus_voltage in \"V\"; convert to \"V\"",
                "vendor_a: packet EPS_HK: fields bus_voltage_counts and mode overlap at bit 70",
            ]
        );
    }

    #[test]
    fn an_asset_belongs_to_one_set() {
        let other = VENDOR_A.replace("name     = \"vendor_a\"", "name     = \"vendor_x\"");
        let definitions = load(
            &source("catalog.toml", CATALOG),
            &[
                source("vendor_a.toml", VENDOR_A),
                source("vendor_x.toml", &other),
            ],
        )
        .unwrap();
        let problems = validate(definitions).unwrap_err().to_string();
        assert!(
            problems.contains("asset SAT-1 is also claimed by set vendor_a"),
            "{problems}"
        );
    }

    #[test]
    fn ir_from_any_source_is_validated_the_same_way() {
        // As if the IR came from a database or a stored plan: JSON, no TOML.
        let json = serde_json::to_string(&definitions(VENDOR_A)).unwrap();
        let mut stored: Definitions = serde_json::from_str(&json).unwrap();
        assert!(validate(stored.clone()).is_ok());

        // Edit the IR directly: a json set can't have bit fields or an endian.
        if let Some(set) = stored.sets.get_mut("vendor_a") {
            set.encoding = Encoding::Json;
        }
        let problems = validate(stored).unwrap_err().to_string();
        assert!(
            problems.contains("endian doesn't apply to json"),
            "{problems}"
        );
        assert!(
            problems.contains("json fields are located with path, not bits"),
            "{problems}"
        );
    }

    #[test]
    fn a_string_field_can_feed_an_enum_directly() {
        let set = r#"
            [set]
            name     = "vendor_b"
            encoding = "json"
            alias    = { path = "/sat" }

            [aliases]
            "PX-7" = "SAT-2"

            [[packet]]
            name     = "SYS_MODE"
            identify = { "/ch" = "SYS.MODE" }

            [[packet.field]]
            name      = "v"
            path      = "/v"
            type      = "string"
            canonical = "eps.mode"
        "#;
        let definitions = load(
            &source("catalog.toml", CATALOG),
            &[source("vendor_b.toml", set)],
        )
        .unwrap();
        assert!(validate(definitions).is_ok());
    }

    #[test]
    fn geodetic_composites_take_degrees_then_meters() {
        let geodetic = VENDOR_A
            .replace("frame     = \"ECI_J2000\"", "frame     = \"GEODETIC\"")
            .replacen("unit        = \"m\"", "unit        = \"deg\"", 2);
        assert!(validate(definitions(&geodetic)).is_ok());

        // All meters is wrong for lat/lon.
        let meters = VENDOR_A.replace("frame     = \"ECI_J2000\"", "frame     = \"GEODETIC\"");
        let problems = validate(definitions(&meters)).unwrap_err().to_string();
        assert!(
            problems.contains("field pos_x: unit \"m\" doesn't match nav.pos_ecef in \"deg\""),
            "{problems}"
        );
        assert!(
            !problems.contains("field pos_z"),
            "altitude stays in meters:\n{problems}"
        );
    }

    #[test]
    fn catalog_rules_apply_to_canonical_params() {
        let catalog = CATALOG
            .replace("red_low = 6.2", "red_low = 9.0")
            .replace("eps.mode", "Eps.Mode")
            .replace(
                "unit  = \"m\"",
                "unit  = \"m\"\n        limits = { red_high = inf }",
            );
        let definitions = load(&source("catalog.toml", &catalog), &[]).unwrap();
        let problems = validate(definitions).unwrap_err().to_string();
        assert!(
            problems.contains("catalog: canonical eps.bus_voltage: limits must be ordered"),
            "{problems}"
        );
        assert!(
            problems.contains("catalog: canonical Eps.Mode: names are lowercase"),
            "{problems}"
        );
        assert!(
            problems.contains("canonical nav.pos_ecef: limits must be finite"),
            "{problems}"
        );
    }
}
