//! Set checks: names and references, versions, time, identification, and
//! numbers inside layouts and conversions.

use std::collections::{BTreeMap, BTreeSet};

use super::MAX_NESTING;
use super::command;
use super::layout::{Container, Layout, Shape, exact_type_sizes, static_offsets, type_name};
use super::mapping;
use super::rules::{
    check_finite, check_integer_spelling, check_member_pointer, check_not_empty, check_pointer,
    check_scalar, check_unit_code, integer_value, is_identifier, is_token,
};
use crate::checksum;
use crate::convert::spline_is_finite;
use crate::diagnostics::{Code, Place, Report};
use crate::ir::{
    ArrayEntry, Bundle, ByteOrder, Catalog, Checksum, ChecksumEntry, Condition, Conversion,
    Element, Encoding, Entry, Family, Field, Interpolation, Locator, Packet, Point, Scalar,
    ScalarElement, ScalarType, Set, StructType, TextEncoding, Time, TimeUnit,
};
use crate::layout::{ContainerLayout, Placement, SetLayout};
use crate::time::Timestamp;

/// What entries may refer to.
struct Scope<'a> {
    catalog: &'a Catalog,
    types: &'a BTreeMap<String, StructType>,
}

pub(super) fn check_all(
    bundle: &Bundle,
    families: &BTreeMap<&str, &Family>,
    root: &Place,
    report: &mut Report,
) {
    let mut names: BTreeMap<&str, usize> = BTreeMap::new();
    let mut versions: BTreeMap<(&str, KeyValue), &str> = BTreeMap::new();
    for (index, set) in bundle.sets.iter().enumerate() {
        let place = root.item("sets", index, "set", &set.name);
        if !is_token(&set.name) {
            report.error(
                Code::InvalidName,
                &place.key("name"),
                "set names are lowercase letters, digits and '_', starting with a letter",
            );
        }
        if let Some(first) = names.get(set.name.as_str()) {
            report.duplicate(
                Code::DuplicateName,
                &place,
                &root.at("sets", *first),
                format!("set {} is defined twice", set.name),
            );
        } else {
            names.insert(&set.name, index);
        }

        let family = families.get(set.family.as_str()).copied();
        match family {
            None => report.error(
                Code::UnknownFamily,
                &place.key("family"),
                format!("family {} isn't defined", set.family),
            ),
            Some(family) => check_version(set, family, &mut versions, &place, report),
        }
        check_time(set, family.map(|f| f.encoding), &place, report);

        let scope = Scope {
            catalog: &bundle.catalog,
            types: &set.types,
        };
        let heights = check_types(set, &scope, &place, report);
        let type_sizes = exact_type_sizes(&set.types, &heights);
        check_packets(set, family, &scope, &type_sizes, &place, report);
        let writes = command::check_packets(set, family, &bundle.catalog, &place, report);
        command::check_record_uses(set, &place, report);
        for (index, packet) in set.commands.iter().enumerate() {
            let packet_place = place.item("commands", index, "command", &packet.name);
            check_entries(
                &packet.entries,
                Container::Command,
                &scope,
                &packet_place,
                report,
            );
        }
        if let Some(family) = family {
            check_layout(set, family, &bundle.catalog, &heights, &place, report);
            command::check_distinct(set, family, &type_sizes, &place, report);
            command::check_writes(&writes, family, report);
        }
    }
}

/// Layout and canonical mapping, which need the family's encoding.
fn check_layout(
    set: &Set,
    family: &Family,
    catalog: &Catalog,
    heights: &BTreeMap<&str, usize>,
    place: &Place,
    report: &mut Report,
) {
    let mut layout = Layout {
        binary: family.encoding == Encoding::Binary,
        byte_order: family.byte_order,
        types: BTreeMap::new(),
    };
    // A type's size is known before anything that uses it is checked.
    let mut order: Vec<(&String, &StructType)> = set.types.iter().collect();
    order.sort_by_key(|(name, _)| heights.get(name.as_str()).copied().unwrap_or(0));
    let records = command::record_types(set);
    for (name, struct_type) in order {
        let type_place = place.map_item("types", name, "type");
        let shape = layout.check_container(
            &struct_type.entries,
            None,
            Container::StructType,
            &type_place,
            report,
        );
        mapping::check_entries(&struct_type.entries, catalog, &type_place, report);
        // A command record's fields are checked with its command packets.
        if !records.contains(name.as_str()) {
            command::check_telemetry_fields(&struct_type.entries, &type_place, report);
        }
        if let Some(shape) = shape {
            layout.types.insert(name, shape);
        }
    }
    let type_mappings = mapping::type_mappings(&set.types, heights, catalog, place, report);
    for (index, packet) in set.packets.iter().enumerate() {
        let packet_place = place.item("packets", index, "packet", &packet.name);
        layout.check_container(
            &packet.entries,
            packet.length.as_ref(),
            Container::Packet,
            &packet_place,
            report,
        );
        mapping::check_entries(&packet.entries, catalog, &packet_place, report);
        command::check_telemetry_fields(&packet.entries, &packet_place, report);
        mapping::check_composites(packet, &set.types, catalog, &packet_place, report);
        mapping::check_duplicate_mappings(packet, &type_mappings, catalog, &packet_place, report);
        check_fixed_regions(set, family, packet, &packet_place, report);
    }
    for (index, packet) in set.commands.iter().enumerate() {
        let packet_place = place.item("commands", index, "command", &packet.name);
        layout.check_container(
            &packet.entries,
            None,
            Container::Command,
            &packet_place,
            report,
        );
    }
}

/// Bits read from every packet regardless of layout (the family's
/// locators, a CUC time code) must fit a packet of exact length.
fn check_fixed_regions(
    set: &Set,
    family: &Family,
    packet: &Packet,
    place: &Place,
    report: &mut Report,
) {
    let Some(crate::ir::Length::Exact { bytes }) = &packet.length else {
        return;
    };
    let length_bits = u64::from(*bytes) * 8;
    let mut regions: Vec<(&str, u64, u64)> = Vec::new();
    for (what, locator) in [
        ("alias", Some(&family.alias)),
        ("version", family.version.as_ref()),
    ] {
        if let Some(Locator::Bits {
            offset_bits,
            size_bits,
        }) = locator
        {
            regions.push((what, u64::from(*offset_bits), u64::from(*size_bits)));
        }
    }
    if let Some(Time::Cuc {
        offset_bits,
        coarse_bytes,
        fine_bytes,
        ..
    }) = &set.time
    {
        regions.push((
            "CUC time code",
            u64::from(*offset_bits),
            (u64::from(*coarse_bytes) + u64::from(*fine_bytes)) * 8,
        ));
    }
    for (what, offset, size) in regions {
        if offset + size > length_bits {
            report.error(
                Code::OutOfBounds,
                &place.key("length"),
                format!(
                    "packet {} is {bytes} bytes, too short for the {what} at bits {offset} to {}",
                    packet.name,
                    offset + size - 1
                ),
            );
        }
    }
}

/// A value as a key reads it. Bits and metadata read integers, so a
/// decimal string compares by value (`"2"` is `2`); a JSON Pointer reads a
/// JSON value, where the string `"2"` and the number `2` differ; a run of
/// bytes compares byte for byte.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum KeyValue {
    Int(i128),
    Str(String),
    Bytes(Vec<u8>),
}

pub(crate) fn by_value(scalar: &Scalar) -> KeyValue {
    match integer_value(scalar) {
        Some(n) => KeyValue::Int(n),
        None => exact(scalar),
    }
}

pub(crate) fn exact(scalar: &Scalar) -> KeyValue {
    match scalar {
        Scalar::Int(n) => KeyValue::Int(i128::from(*n)),
        Scalar::Str(text) => KeyValue::Str(text.clone()),
    }
}

fn show(scalar: &Scalar) -> String {
    match scalar {
        Scalar::Int(n) => n.to_string(),
        Scalar::Str(s) => format!("{s:?}"),
    }
}

fn check_version<'a>(
    set: &'a Set,
    family: &'a Family,
    versions: &mut BTreeMap<(&'a str, KeyValue), &'a str>,
    place: &Place,
    report: &mut Report,
) {
    let version_place = place.key("version");
    match (&family.version, &set.version) {
        (Some(_), None) => report.error(
            Code::SetVersion,
            &version_place,
            format!(
                "family {} reads a layout version from each message, so each of its sets has a \
                 version",
                family.name
            ),
        ),
        (None, Some(_)) => report.error(
            Code::SetVersion,
            &version_place,
            format!(
                "family {} has no version locator, so its sets have no version (the asset \
                 registry picks the set)",
                family.name
            ),
        ),
        (Some(locator), Some(version)) => {
            check_scalar(version, "version", &version_place, report);
            if let Scalar::Str(text) = version {
                check_not_empty(text, "version", &version_place, report);
            }
            if matches!(locator, Locator::Metadata { .. }) {
                check_integer_spelling(version, "version", &version_place, report);
            }
            if let Locator::Bits { size_bits, .. } = locator {
                check_integer_spelling(version, "version", &version_place, report);
                let max = if *size_bits >= 64 {
                    i128::from(u64::MAX)
                } else {
                    (1_i128 << size_bits) - 1
                };
                let fits = integer_value(version).is_some_and(|n| (0..=max).contains(&n));
                if !fits {
                    report.error(
                        Code::SetVersion,
                        &version_place,
                        format!(
                            "version {} can't come from family {}'s {size_bits}-bit version \
                             locator, which reads integers from 0 to {max}",
                            show(version),
                            family.name
                        ),
                    );
                }
            }
            let key = match locator {
                Locator::Path { .. } => exact(version),
                _ => by_value(version),
            };
            if let Some(other) = versions.insert((&family.name, key), &set.name) {
                report.error(
                    Code::SetVersion,
                    &version_place,
                    format!(
                        "sets {other} and {} both have version {} in family {}",
                        set.name,
                        show(version),
                        family.name
                    ),
                );
            }
        }
        (None, None) => {}
    }
}

fn check_time(set: &Set, encoding: Option<Encoding>, place: &Place, report: &mut Report) {
    let Some(time) = &set.time else {
        return;
    };
    let set_place = place;
    let place = place.key("time");
    match time {
        Time::Cuc {
            coarse_bytes,
            fine_bytes,
            epoch,
            ..
        } => {
            if encoding.is_some_and(|e| e != Encoding::Binary) {
                report.error(
                    Code::WrongEncoding,
                    &place,
                    "CUC time is in binary payloads; self-describing families use iso8601",
                );
            }
            if let Time::Cuc { offset_bits, .. } = time
                && offset_bits % 8 != 0
            {
                report.error(
                    Code::Alignment,
                    &place.key("offset_bits"),
                    format!("a CUC time code starts on a byte boundary, not at bit {offset_bits}"),
                );
            }
            if !(1..=4).contains(coarse_bytes) || *fine_bytes > 3 {
                report.error(
                    Code::CucSize,
                    &place,
                    format!(
                        "a CUC time code has 1 to 4 coarse bytes and 0 to 3 fine bytes, not \
                         {coarse_bytes} and {fine_bytes}"
                    ),
                );
            }
            check_epoch(epoch, &place.key("epoch"), report);
        }
        Time::Elapsed { epoch, .. } => check_epoch(epoch, &place.key("epoch"), report),
        Time::Gps { week_bits, .. } => {
            if let Some(bits) = week_bits
                && !(1..=32).contains(bits)
            {
                report.error(
                    Code::BitSize,
                    &place.key("week_bits"),
                    format!("a rolling-over week count has 1 to 32 bits, not {bits}"),
                );
            }
        }
        Time::Iso8601 { pointer } => {
            if encoding == Some(Encoding::Binary) {
                report.error(
                    Code::WrongEncoding,
                    &place,
                    "iso8601 time is in self-describing payloads; binary families use cuc or boot",
                );
            }
            check_member_pointer(pointer, "an iso8601 time", &place.key("pointer"), report);
        }
        Time::Boot { correlate, unit } => {
            let correlate_place = place.key("correlate");
            check_epoch(&correlate.epoch, &correlate_place.key("epoch"), report);
            if correlate.clock_field == correlate.boot_field {
                report.error(
                    Code::CorrelateFields,
                    &correlate_place,
                    format!(
                        "clock_field and boot_field both name {}; the correlation packet carries \
                         two clocks in two fields",
                        correlate.boot_field
                    ),
                );
            }
            match set
                .packets
                .iter()
                .enumerate()
                .find(|(_, p)| p.name == correlate.packet)
            {
                None => report.error(
                    Code::UnknownPacket,
                    &correlate_place.key("packet"),
                    format!(
                        "packet {} isn't defined in set {}",
                        correlate.packet, set.name
                    ),
                ),
                Some((packet_index, packet)) => {
                    let packet_place =
                        set_place.item("packets", packet_index, "packet", &packet.name);
                    for (key, field, counts) in [
                        (
                            "clock_field",
                            &correlate.clock_field,
                            unit_code(correlate.clock_unit),
                        ),
                        ("boot_field", &correlate.boot_field, unit_code(*unit)),
                    ] {
                        let field_place = correlate_place.key(key);
                        match top_level_field(packet, field) {
                            Some((index, field)) => check_clock_field(
                                field,
                                key,
                                (&[ScalarType::Uint], counts),
                                &field_place,
                                &packet_place.item("entries", index, "field", &field.name),
                                report,
                            ),
                            None => report.error(
                                Code::UnknownEntry,
                                &field_place,
                                format!("packet {} has no top-level field {field}", packet.name),
                            ),
                        }
                    }
                }
            }
        }
    }
}

fn check_epoch(epoch: &str, place: &Place, report: &mut Report) {
    if epoch.parse::<Timestamp>().is_err() {
        report.error(
            Code::InvalidEpoch,
            place,
            format!(
                "epoch {epoch:?} is not an instant like \"2000-01-01T00:00:00Z\" \
                 (YYYY-MM-DDTHH:MM:SS, optional fraction without trailing zeros, then Z)"
            ),
        );
    }
}

/// The field types a packet's `time_field` may have, by time source:
/// `None` when the source reads no `time_field`.
fn clock_types(time: Option<&Time>) -> Option<(&'static [ScalarType], &'static str)> {
    match time? {
        Time::Boot { unit, .. } => Some((&[ScalarType::Uint], unit_code(*unit))),
        Time::Elapsed { unit, .. } => Some((
            &[ScalarType::Uint, ScalarType::Int, ScalarType::Float],
            unit_code(*unit),
        )),
        Time::Gps { unit, .. } => Some((&[ScalarType::Uint, ScalarType::Float], unit_code(*unit))),
        Time::Cuc { .. } | Time::Iso8601 { .. } => None,
    }
}

/// A time unit's UCUM code.
fn unit_code(unit: TimeUnit) -> &'static str {
    match unit {
        TimeUnit::S => "s",
        TimeUnit::Ms => "ms",
        TimeUnit::Us => "us",
        TimeUnit::Ns => "ns",
    }
}

/// A clock (a packet's `time_field`, a GPS week, a correlation's two
/// clocks) is read from every message it's in, so it's a number of one of
/// `types` that's always there. It's read raw, as a count of `unit` (§8),
/// so it has no conversion, and no unit but that one, which it may restate
/// so it can map to a param in another unit. `place` is where the source
/// names it; `entry` is the field itself.
fn check_clock_field(
    field: &Field,
    what: &str,
    (types, unit): (&[ScalarType], &str),
    place: &Place,
    entry: &Place,
    report: &mut Report,
) {
    if field.conversion.is_some() {
        report.error(
            Code::ReferenceType,
            &entry.key("conversion"),
            format!(
                "{what} {} is read raw, as a count, so it has no conversion",
                field.name
            ),
        );
    }
    if let Some(given) = field.unit.as_deref().filter(|given| *given != unit) {
        report.error(
            Code::ReferenceType,
            &entry.key("unit"),
            format!(
                "{what} {} is read raw, as a count of {unit:?}, so its unit is {unit:?} or none, \
                 not {given:?}",
                field.name
            ),
        );
    }
    let problem = if !types.contains(&field.ty) {
        Some(format!("a {} field", type_name(field.ty)))
    } else if field.when.is_some() {
        Some("conditional".to_string())
    } else if field.optional == Some(true) {
        Some("optional".to_string())
    } else {
        None
    };
    if let Some(problem) = problem {
        let names: Vec<&str> = types.iter().map(|ty| type_name(*ty)).collect();
        report.error(
            Code::ReferenceType,
            place,
            format!(
                "{what} {} is read from every message, so it's an unconditional {} field, not \
                 {problem}",
                field.name,
                names.join(" or ")
            ),
        );
    }
}

/// A top-level field of a packet by name, with its position in `entries`.
fn top_level_field<'a>(packet: &'a Packet, name: &str) -> Option<(usize, &'a Field)> {
    packet
        .entries
        .iter()
        .enumerate()
        .find_map(|(index, entry)| match entry {
            Entry::Field(field) if field.name == name => Some((index, field)),
            _ => None,
        })
}

fn check_types<'a>(
    set: &'a Set,
    scope: &Scope,
    place: &Place,
    report: &mut Report,
) -> BTreeMap<&'a str, usize> {
    for (name, struct_type) in &set.types {
        let type_place = place.map_item("types", name, "type");
        if !is_identifier(name) {
            report.error(
                Code::InvalidName,
                &type_place,
                "type names are letters, digits and '_', not starting with a digit",
            );
        }
        if struct_type.entries.is_empty() {
            report.error(
                Code::Empty,
                &type_place.key("entries"),
                "a struct type has at least one entry",
            );
        }
        check_entries(
            &struct_type.entries,
            Container::StructType,
            scope,
            &type_place,
            report,
        );
    }
    check_type_graph(&set.types, place, report)
}

fn check_packets(
    set: &Set,
    family: Option<&Family>,
    scope: &Scope,
    type_sizes: &BTreeMap<&str, u64>,
    place: &Place,
    report: &mut Report,
) {
    let encoding = family.map(|f| f.encoding);
    let clock_types = clock_types(set.time.as_ref());
    let mut names: BTreeMap<&str, usize> = BTreeMap::new();
    let mut identities = Vec::with_capacity(set.packets.len());
    for (index, packet) in set.packets.iter().enumerate() {
        let packet_place = place.item("packets", index, "packet", &packet.name);
        if !is_identifier(&packet.name) {
            report.error(
                Code::InvalidName,
                &packet_place.key("name"),
                "packet names are letters, digits and '_', not starting with a digit",
            );
        }
        if let Some(first) = names.get(packet.name.as_str()) {
            report.duplicate(
                Code::DuplicateName,
                &packet_place,
                &place.at("packets", *first),
                format!("packet {} is defined twice in this set", packet.name),
            );
        } else {
            names.insert(&packet.name, index);
        }

        if let Some(time_field) = &packet.time_field {
            let field_place = packet_place.key("time_field");
            match clock_types {
                None => report.error(
                    Code::TimeField,
                    &field_place,
                    "time_field only applies when the set's time is boot, elapsed or gps",
                ),
                Some(types) => match top_level_field(packet, time_field) {
                    Some((index, field)) => check_clock_field(
                        field,
                        "time_field",
                        types,
                        &field_place,
                        &packet_place.item("entries", index, "field", &field.name),
                        report,
                    ),
                    None => report.error(
                        Code::UnknownEntry,
                        &field_place,
                        format!("packet {} has no top-level field {time_field}", packet.name),
                    ),
                },
            }
            // A GPS time also needs the week, in a field of the same name in
            // every timed packet.
            if let Some(Time::Gps { week_field, .. }) = &set.time {
                match top_level_field(packet, week_field) {
                    Some((index, field)) => check_clock_field(
                        field,
                        "week field",
                        (&[ScalarType::Uint], "wk"),
                        &field_place,
                        &packet_place.item("entries", index, "field", &field.name),
                        report,
                    ),
                    None => report.error(
                        Code::UnknownEntry,
                        &field_place,
                        format!(
                            "packet {} has a time_field, so it needs the GPS week field \
                             {week_field} at its top level",
                            packet.name
                        ),
                    ),
                }
            }
        }
        if packet.length.is_some() && encoding.is_some_and(|e| e != Encoding::Binary) {
            report.error(
                Code::WrongEncoding,
                &packet_place.key("length"),
                "only binary payloads have a length",
            );
        }
        identities.push(resolve_identify(
            index,
            packet,
            family,
            type_sizes,
            &packet_place,
            report,
        ));
        check_entries(
            &packet.entries,
            Container::Packet,
            scope,
            &packet_place,
            report,
        );
        for (index, composite) in packet.composites.iter().enumerate() {
            let composite_place =
                packet_place.item("composites", index, "composite", &composite.canonical);
            check_canonical(
                &composite.canonical,
                scope,
                &composite_place.key("canonical"),
                report,
            );
            if composite.members.is_empty() {
                report.error(
                    Code::Empty,
                    &composite_place.key("members"),
                    "a composite lists its member entries",
                );
            }
        }
    }
    check_ambiguity(set, &identities, place, report);
}

/// What an identify key reads, so keys in different packets can be compared
/// by what they read rather than by name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum KeyId {
    Metadata(String),
    Pointer(String),
    /// A field at a known position, read in `order`: the same bits read in
    /// the other order are another key.
    Bits {
        offset: u64,
        size: u32,
        signed: bool,
        order: ByteOrder,
    },
    /// Whole bytes at a known byte-aligned position: a fixed-length string
    /// constant of a command packet, as the bytes it writes.
    Bytes {
        offset: u64,
        length: u32,
    },
    /// A field whose position isn't known statically: equal to no other key.
    Unplaced {
        packet: usize,
        name: String,
    },
}

/// Where every entry of a binary set's containers can be, worked out by
/// the walk [`check_layout`] makes. Only meaningful for a validated set and
/// its family; `None` for a self-describing one.
pub(crate) fn layout_of(set: &Set, family: &Family) -> Option<SetLayout> {
    if family.encoding != Encoding::Binary {
        return None;
    }
    // A validated set reports nothing here.
    let mut scratch = Report::default();
    let place = Place::root("");
    let heights = check_type_graph(&set.types, &place, &mut scratch);
    let type_sizes = exact_type_sizes(&set.types, &heights);
    let mut layout = Layout {
        binary: true,
        byte_order: family.byte_order,
        types: BTreeMap::new(),
    };
    let laid = |name: &str,
                entries: &[Entry],
                placed: (Option<Shape>, Vec<Option<Placement>>)|
     -> Option<(ContainerLayout, Shape)> {
        let (shape, placements) = placed;
        let shape = shape?;
        let placements: Vec<Placement> = placements.into_iter().collect::<Option<_>>()?;
        let entries = placements
            .into_iter()
            .zip(static_offsets(entries, &type_sizes))
            .map(|(placement, offset)| Placement {
                static_offset_bits: offset,
                ..placement
            })
            .collect();
        let container = ContainerLayout {
            name: name.to_string(),
            entries,
            size_min_bits: shape.size.min,
            size_max_bits: shape.size.max,
        };
        Some((container, shape))
    };
    let mut order: Vec<(&String, &StructType)> = set.types.iter().collect();
    order.sort_by_key(|(name, _)| heights.get(name.as_str()).copied().unwrap_or(0));
    let mut types = BTreeMap::new();
    for (name, struct_type) in order {
        let placed = layout.place_container(
            &struct_type.entries,
            None,
            Container::StructType,
            &place,
            &mut scratch,
        );
        let (container, shape) = laid(name, &struct_type.entries, placed)?;
        layout.types.insert(name, shape);
        types.insert(name.clone(), container);
    }
    let mut packets = Vec::new();
    for packet in &set.packets {
        let placed = layout.place_container(
            &packet.entries,
            packet.length.as_ref(),
            Container::Packet,
            &place,
            &mut scratch,
        );
        packets.push(laid(&packet.name, &packet.entries, placed)?.0);
    }
    let mut commands = Vec::new();
    for packet in &set.commands {
        let placed = layout.place_container(
            &packet.entries,
            None,
            Container::Command,
            &place,
            &mut scratch,
        );
        commands.push(laid(&packet.name, &packet.entries, placed)?.0);
    }
    Some(SetLayout {
        packets,
        commands,
        types,
    })
}

/// The identify keys of each packet of a set, resolved exactly as
/// validation resolves them, so the decoder identifies packets the way the
/// ambiguity check assumed. Also the exact size of each struct type that
/// has one. Only meaningful for a validated set and its family.
pub(crate) fn decoding_facts<'a>(
    set: &'a Set,
    family: &Family,
) -> (Vec<BTreeMap<KeyId, KeyValue>>, BTreeMap<&'a str, u64>) {
    // A validated set reports nothing here.
    let mut scratch = Report::default();
    let place = Place::root("");
    let heights = check_type_graph(&set.types, &place, &mut scratch);
    let type_sizes = exact_type_sizes(&set.types, &heights);
    let identities = set
        .packets
        .iter()
        .enumerate()
        .map(|(index, packet)| {
            resolve_identify(
                index,
                packet,
                Some(family),
                &type_sizes,
                &place,
                &mut scratch,
            )
            .keys
        })
        .collect();
    (identities, type_sizes)
}

struct Identity {
    keys: BTreeMap<KeyId, KeyValue>,
    /// Field keys whose position isn't known, for the diagnostic's advice.
    unplaced: Vec<String>,
    /// False when the family is unknown or a key didn't resolve: the
    /// identity is then partial, and comparing it would report overlaps
    /// that are really the earlier error.
    complete: bool,
}

fn resolve_identify(
    packet_index: usize,
    packet: &Packet,
    family: Option<&Family>,
    type_sizes: &BTreeMap<&str, u64>,
    place: &Place,
    report: &mut Report,
) -> Identity {
    let offsets = static_offsets(&packet.entries, type_sizes);
    let mut identity = Identity {
        keys: BTreeMap::new(),
        unplaced: Vec::new(),
        complete: family.is_some(),
    };
    for (key, value) in &packet.identify {
        let key_place = place.map_item("identify", key, "identify");
        check_scalar(value, "identify value", &key_place, report);
        let Some(family) = family else {
            continue;
        };
        let packet_keys = PacketKeys {
            index: packet_index,
            packet,
            family,
            offsets: &offsets,
        };
        let resolved = packet_keys.resolve(key, value, &key_place, &mut identity.unplaced, report);
        match resolved {
            Some((id, value)) => {
                identity.keys.insert(id, value);
            }
            None => identity.complete = false,
        }
    }
    identity
}

/// What resolving one packet's identify keys needs.
struct PacketKeys<'a> {
    index: usize,
    packet: &'a Packet,
    family: &'a Family,
    offsets: &'a [Option<u64>],
}

impl PacketKeys<'_> {
    fn resolve(
        &self,
        key: &str,
        value: &Scalar,
        place: &Place,
        unplaced: &mut Vec<String>,
        report: &mut Report,
    ) -> Option<(KeyId, KeyValue)> {
        let PacketKeys {
            index: packet_index,
            packet,
            family,
            offsets,
        } = *self;
        let declared = family.metadata.iter().any(|m| m == key);
        if key.is_empty() && family.encoding == Encoding::Json {
            report.error(
                Code::InvalidPointer,
                place,
                "the empty pointer is the whole document, a JSON object (decoding.md §5.3), \
                 never a string or an integer to identify by; point it at a member, like \"/ch\"",
            );
            return None;
        }
        if key.starts_with('/') {
            if family.encoding == Encoding::Binary {
                report.error(
                    Code::WrongEncoding,
                    place,
                    "JSON Pointer keys only apply to self-describing payloads; binary packets \
                 identify by their own top-level fields or declared metadata",
                );
                return None;
            }
            check_pointer(key, place, report);
            return Some((KeyId::Pointer(key.to_string()), exact(value)));
        }
        let field = match family.encoding {
            Encoding::Binary => top_level_field(packet, key),
            Encoding::Json => None,
        };
        match (field, declared) {
            (Some(_), true) => {
                report.error(
                    Code::DuplicateName,
                    place,
                    format!(
                        "{key} names both a top-level field of packet {} and metadata family {} \
                     declares, so which one identifies the packet is unclear; rename the field \
                     or the metadata",
                        packet.name, family.name
                    ),
                );
                None
            }
            (Some((index, field)), false) => {
                let value = check_field_value(field, value, place, report)?;
                let id = match (offsets[index], field.size_bits) {
                    (Some(offset), Some(size)) if field.when.is_none() => KeyId::Bits {
                        offset,
                        size,
                        signed: field.ty == ScalarType::Int,
                        order: key_order(field, family, offset, size),
                    },
                    _ => {
                        unplaced.push(key.to_string());
                        KeyId::Unplaced {
                            packet: packet_index,
                            name: key.to_string(),
                        }
                    }
                };
                Some((id, value))
            }
            (None, true) => {
                check_integer_spelling(value, "identify value", place, report);
                Some((KeyId::Metadata(key.to_string()), by_value(value)))
            }
            (None, false) => {
                let what = if family.encoding == Encoding::Binary {
                    format!(
                        "{key:?} is neither a top-level field of packet {} nor a metadata name \
                     family {} declares",
                        packet.name, family.name
                    )
                } else {
                    format!(
                        "{key:?} is neither a JSON Pointer (\"/ch\") nor a metadata name family {} \
                     declares",
                        family.name
                    )
                };
                report.error(Code::UnknownMetadata, place, what);
                None
            }
        }
    }
}

/// The byte order an identify key at these bits is read in: the field's
/// own, else its family's. Inside one byte both orders read the same, so
/// such a key is the same key whichever order its field names. Layout
/// validation already refuses a `byte_order` there; this keeps a refused
/// definition from adding an ambiguity error on top.
pub(super) fn key_order(field: &Field, family: &Family, offset: u64, size: u32) -> ByteOrder {
    if offset % 8 + u64::from(size) <= 8 {
        return ByteOrder::Big;
    }
    // A binary family without one is reported; big keeps the checks going.
    field
        .byte_order
        .or(family.byte_order)
        .unwrap_or(ByteOrder::Big)
}

/// An identify value for a field key: an integer the field can hold.
fn check_field_value(
    field: &Field,
    value: &Scalar,
    place: &Place,
    report: &mut Report,
) -> Option<KeyValue> {
    let kind = match field.ty {
        ScalarType::Uint => "unsigned",
        ScalarType::Int => "signed",
        ScalarType::Bool => "bool",
        _ => {
            report.error(
                Code::IdentifyValue,
                place,
                format!(
                    "identify keys that name a field need an integer or bool field; {} isn't one",
                    field.name
                ),
            );
            return None;
        }
    };
    check_integer_spelling(value, "identify value", place, report);
    let Some(n) = integer_value(value) else {
        report.error(
            Code::IdentifyValue,
            place,
            format!(
                "identify value {value:?} isn't an integer, so field {} can never hold it",
                field.name
            ),
        );
        return None;
    };
    // An invalid size is reported by the layout check; skip the range then.
    let size = field.size_bits.filter(|s| (1..=64).contains(s))?;
    let (low, high) = match field.ty {
        ScalarType::Bool => (0, 1),
        ScalarType::Int => (-(1_i128 << (size - 1)), (1_i128 << (size - 1)) - 1),
        _ => (0, (1_i128 << size) - 1),
    };
    if !(low..=high).contains(&n) {
        report.error(
            Code::IdentifyValue,
            place,
            format!(
                "identify value {n} is out of range for {kind} {size}-bit field {} ({low} to \
                 {high}), so it can never match",
                field.name
            ),
        );
        return None;
    }
    Some(KeyValue::Int(n))
}

/// Packets are tried most specific first. A message matches a packet when
/// every identify key matches. If two packets can both match one message,
/// the one whose keys strictly contain the other's wins; if neither contains
/// the other (or they're identical), decoding would depend on packet order,
/// so that's an error.
fn check_ambiguity(set: &Set, identities: &[Identity], place: &Place, report: &mut Report) {
    for (index, later) in identities.iter().enumerate() {
        let conflicts: Vec<usize> = identities[..index]
            .iter()
            .enumerate()
            .filter(|(_, earlier)| overlap_without_winner(earlier, later))
            .map(|(i, _)| i)
            .collect();
        let Some(&first) = conflicts.first() else {
            continue;
        };
        let names: Vec<&str> = conflicts
            .iter()
            .take(3)
            .map(|i| set.packets[*i].name.as_str())
            .collect();
        let others = match conflicts.len() {
            n if n > 3 => format!(" and {} others", n - 3),
            _ => String::new(),
        };
        let mut message = format!(
            "packet {} can match the same message as {}{others}, and neither is more specific \
             (its identify keys don't strictly contain the other's); give them a key with \
             different values",
            set.packets[index].name,
            names.join(", ")
        );
        let unplaced: BTreeSet<&str> = later
            .unplaced
            .iter()
            .chain(&identities[first].unplaced)
            .map(String::as_str)
            .collect();
        if !unplaced.is_empty() {
            let list: Vec<&str> = unplaced.into_iter().collect();
            message.push_str(&format!(
                ". The position of {} isn't known without decoding, so it counts as a different \
                 key in each packet; an explicit offset_bits lets this check see it's the same \
                 field",
                list.join(", ")
            ));
        }
        report.error(
            Code::AmbiguousIdentify,
            &place
                .item("packets", index, "packet", &set.packets[index].name)
                .key("identify"),
            message,
        );
    }
}

fn overlap_without_winner(a: &Identity, b: &Identity) -> bool {
    if !a.complete || !b.complete {
        return false;
    }
    let disagree = a
        .keys
        .iter()
        .any(|(key, value)| b.keys.get(key).is_some_and(|other| other != value));
    if disagree {
        return false;
    }
    let a_in_b = a.keys.keys().all(|key| b.keys.contains_key(key));
    let b_in_a = b.keys.keys().all(|key| a.keys.contains_key(key));
    // Strict containment either way means one is more specific.
    a_in_b == b_in_a
}

/// The entries of a container: names, and each entry's own rules. A
/// struct type can't hold a checksum; a command packet's telemetry-only
/// keys are reported by command.rs.
fn check_entries(
    entries: &[Entry],
    kind: Container,
    scope: &Scope,
    container: &Place,
    report: &mut Report,
) {
    let top_level = kind != Container::StructType;
    let mut names: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        let (label, name) = match entry {
            Entry::Field(field) => ("field", Some(field.name.as_str())),
            Entry::Array(array) => ("array", Some(array.name.as_str())),
            Entry::Struct(entry) => ("struct", Some(entry.name.as_str())),
            Entry::Checksum(checksum) => ("checksum", Some(checksum.name.as_str())),
            Entry::Padding(_) => ("padding", None),
        };
        let place = container.item("entries", index, label, name.unwrap_or(""));
        if let Some(name) = name {
            if !is_identifier(name) {
                report.error(
                    Code::InvalidName,
                    &place.key("name"),
                    "entry names are letters, digits and '_', not starting with a digit (no dots \
                     or brackets: output paths use them)",
                );
            }
            if let Some(first) = names.get(name) {
                report.duplicate(
                    Code::DuplicateName,
                    &place,
                    &container.at("entries", *first),
                    format!("entry {name} is defined twice here"),
                );
            } else {
                names.insert(name, index);
            }
        }
        match entry {
            Entry::Field(field) => check_field(field, kind, scope, &place, report),
            Entry::Array(array) => check_array(array, kind, scope, &place, report),
            Entry::Struct(entry) => {
                check_type_ref(&entry.struct_type, scope, &place.key("struct"), report);
                if let Some(pointer) = &entry.pointer {
                    check_pointer(pointer, &place.key("pointer"), report);
                }
                if let Some(when) = &entry.when {
                    check_condition(when, &place.key("when"), 1, report);
                }
            }
            Entry::Padding(padding) => {
                if let Some(when) = &padding.when {
                    check_condition(when, &place.key("when"), 1, report);
                }
            }
            Entry::Checksum(checksum) => check_checksum(checksum, top_level, &place, report),
        }
    }
}

/// A checksum covers whole bytes of a packet up to its own start, with
/// parameters that fit its width.
fn check_checksum(checksum: &ChecksumEntry, top_level: bool, place: &Place, report: &mut Report) {
    if !top_level {
        report.error(
            Code::Checksum,
            place,
            "a checksum covers a packet's bytes, so it sits at the packet's top level, not in a \
             struct type",
        );
    }
    let algorithm_place = place.key("algorithm");
    let width = match &checksum.algorithm {
        Checksum::Crc { width, .. } | Checksum::Sum { width } => Some(*width),
        Checksum::Xor {} | Checksum::Iso8473 {} => None,
    };
    if let Some(width) = width
        && !checksum::is_width(width)
    {
        report.error(
            Code::Checksum,
            &algorithm_place.key("width"),
            format!("a checksum is whole bytes from 8 to 64 bits wide, not {width}"),
        );
    }
    if let Checksum::Crc {
        width,
        poly,
        init,
        xor_out,
        ..
    } = &checksum.algorithm
    {
        for (key, value) in [("poly", poly), ("init", init), ("xor_out", xor_out)] {
            let value_place = algorithm_place.key(key);
            check_scalar(value, key, &value_place, report);
            check_integer_spelling(value, key, &value_place, report);
            let fits = checksum::is_width(*width)
                && integer_value(value)
                    .and_then(|n| u64::try_from(n).ok())
                    .is_some_and(|n| n <= checksum::mask(u32::from(*width)));
            if checksum::is_width(*width) && !fits {
                report.error(
                    Code::Checksum,
                    &value_place,
                    format!("{key} is an unsigned integer that fits the CRC's {width} bits"),
                );
            }
        }
    }
    if let Some(from) = checksum.from_bits {
        if !from.is_multiple_of(8) {
            report.error(
                Code::Alignment,
                &place.key("from_bits"),
                format!(
                    "a checksum covers whole bytes, so from_bits is a byte boundary, not {from}"
                ),
            );
        }
        if let Some(offset) = checksum.offset_bits
            && from > offset
        {
            report.error(
                Code::Checksum,
                &place.key("from_bits"),
                format!(
                    "a checksum covers bytes before its own start ({offset}), so from_bits can't \
                     be {from}"
                ),
            );
        }
    }
}

fn check_field(field: &Field, kind: Container, scope: &Scope, place: &Place, report: &mut Report) {
    if let Some(canonical) = field
        .canonical
        .as_ref()
        .filter(|_| kind != Container::Command)
    {
        check_canonical(canonical, scope, &place.key("canonical"), report);
    }
    if let Some(pointer) = &field.pointer {
        check_member_pointer(pointer, "a field", &place.key("pointer"), report);
    }
    if let Some(unit) = &field.unit {
        check_unit_code(unit, &place.key("unit"), report);
    }
    check_text(field.text, place, report);
    if let Some(conversion) = &field.conversion {
        check_conversion(conversion, &place.key("conversion"), report);
    }
    if let Some(inverse) = &field.inverse {
        check_conversion(inverse, &place.key("inverse"), report);
    }
    check_invalid(&field.invalid, place, report);
    if let Some(when) = &field.when {
        check_condition(when, &place.key("when"), 1, report);
    }
}

fn check_text(text: Option<TextEncoding>, place: &Place, report: &mut Report) {
    if text == Some(TextEncoding::Utf8) {
        report.error(
            Code::ExplicitDefault,
            &place.key("text"),
            "utf8 is the default text encoding; omit text",
        );
    }
}

fn check_array(
    array: &ArrayEntry,
    kind: Container,
    scope: &Scope,
    place: &Place,
    report: &mut Report,
) {
    if let Some(canonical) = array
        .canonical
        .as_ref()
        .filter(|_| kind != Container::Command)
    {
        check_canonical(canonical, scope, &place.key("canonical"), report);
    }
    if let Some(interval) = array.sample_interval_ms {
        let interval_place = place.key("sample_interval_ms");
        check_finite(interval, "sample_interval_ms", &interval_place, report);
        if interval.is_finite() && interval <= 0.0 {
            report.error(
                Code::SampleInterval,
                &interval_place,
                format!("sample_interval_ms must be positive, not {interval}"),
            );
        }
        if array.canonical.is_none() {
            report.error(
                Code::SampleInterval,
                &interval_place,
                "sample_interval_ms spaces out canonical samples, so it needs canonical",
            );
        }
    }
    if let Some(pointer) = &array.pointer {
        check_member_pointer(pointer, "an array", &place.key("pointer"), report);
    }
    match &array.element {
        Element::Scalar(element) => check_element(element, &place.key("element"), report),
        Element::Struct { name } => check_type_ref(name, scope, &place.key("element"), report),
    }
    if let Some(when) = &array.when {
        check_condition(when, &place.key("when"), 1, report);
    }
}

fn check_element(element: &ScalarElement, place: &Place, report: &mut Report) {
    if let Some(unit) = &element.unit {
        check_unit_code(unit, &place.key("unit"), report);
    }
    check_text(element.text, place, report);
    if let Some(conversion) = &element.conversion {
        check_conversion(conversion, &place.key("conversion"), report);
    }
    if let Some(inverse) = &element.inverse {
        check_conversion(inverse, &place.key("inverse"), report);
    }
    check_invalid(&element.invalid, place, report);
}

fn check_invalid(invalid: &[Scalar], place: &Place, report: &mut Report) {
    for (index, value) in invalid.iter().enumerate() {
        check_scalar(value, "invalid value", &place.at("invalid", index), report);
    }
}

fn check_canonical(name: &str, scope: &Scope, place: &Place, report: &mut Report) {
    if !scope.catalog.params.contains_key(name) {
        report.error(
            Code::UnknownCanonical,
            place,
            format!("param {name} isn't in the catalog"),
        );
    }
}

fn check_type_ref(name: &str, scope: &Scope, place: &Place, report: &mut Report) {
    if !scope.types.contains_key(name) {
        report.error(
            Code::UnknownType,
            place,
            format!("struct type {name} isn't defined in this set's types"),
        );
    }
}

fn check_conversion(conversion: &Conversion, place: &Place, report: &mut Report) {
    match conversion {
        Conversion::Linear { scale, offset } => {
            check_finite(*scale, "scale", &place.key("scale"), report);
            check_finite(*offset, "offset", &place.key("offset"), report);
        }
        Conversion::Poly { coefficients } => {
            if coefficients.is_empty() {
                report.error(
                    Code::Empty,
                    &place.key("coefficients"),
                    "a polynomial has at least one coefficient",
                );
            }
            for (index, c) in coefficients.iter().enumerate() {
                check_finite(
                    *c,
                    "a coefficient",
                    &place.at("coefficients", index),
                    report,
                );
            }
        }
        Conversion::Table {
            points,
            interpolation,
            extrapolate,
        } => {
            check_points(points, place, report);
            if *interpolation == Some(Interpolation::Linear) {
                report.error(
                    Code::ExplicitDefault,
                    &place.key("interpolation"),
                    "linear is the default interpolation; omit it",
                );
            }
            check_extrapolate(*extrapolate, place, report);
        }
        Conversion::Spline {
            points,
            extrapolate,
        } => {
            if check_points(points, place, report) && !spline_is_finite(points) {
                report.error(
                    Code::NonFinite,
                    &place.key("points"),
                    "a spline through these points has an infinite slope or curvature; space the \
                     points further apart",
                );
            }
            check_extrapolate(*extrapolate, place, report);
        }
        Conversion::States { states } => {
            if states.is_empty() {
                report.error(
                    Code::Empty,
                    &place.key("states"),
                    "a states conversion lists at least one state",
                );
            }
            let mut raws = BTreeSet::new();
            for (index, state) in states.iter().enumerate() {
                let state_place = place.at("states", index);
                let raw_place = state_place.key("raw");
                check_scalar(&state.raw, "raw value", &raw_place, report);
                check_integer_spelling(&state.raw, "raw value", &raw_place, report);
                check_not_empty(&state.name, "a state name", &state_place, report);
                let Some(raw) = integer_value(&state.raw) else {
                    report.error(
                        Code::ReferenceType,
                        &raw_place,
                        "a state's raw value is an integer: a number, or a decimal string past \
                         ±(2^53 - 1)",
                    );
                    continue;
                };
                if !raws.insert(raw) {
                    report.error(
                        Code::DuplicateValue,
                        &state_place,
                        format!("raw value {raw} has two states"),
                    );
                }
            }
        }
    }
}

/// A table's or spline's points: at least two, finite, in strictly
/// increasing raw order, so each raw value falls in one segment, with
/// finite differences between neighbours. Whether they are.
fn check_points(points: &[Point], place: &Place, report: &mut Report) -> bool {
    let mut good = points.len() >= 2;
    if points.len() < 2 {
        report.error(
            Code::Empty,
            &place.key("points"),
            "a table or spline has at least two points",
        );
    }
    for (index, point) in points.iter().enumerate() {
        let point_place = place.at("points", index);
        check_finite(point.raw, "raw", &point_place.key("raw"), report);
        check_finite(point.value, "value", &point_place.key("value"), report);
        good &= point.raw.is_finite() && point.value.is_finite();
    }
    for (index, pair) in points.windows(2).enumerate() {
        let gaps_finite =
            (pair[1].raw - pair[0].raw).is_finite() && (pair[1].value - pair[0].value).is_finite();
        if good && !gaps_finite {
            good = false;
            report.error(
                Code::NonFinite,
                &place.at("points", index + 1),
                "the difference from the point before is too large for a double",
            );
        }
        if pair[1].raw <= pair[0].raw {
            good = false;
            report.error(
                Code::NotSorted,
                &place.at("points", index + 1).key("raw"),
                format!(
                    "points go in strictly increasing raw order, and {} doesn't come after {}",
                    pair[1].raw, pair[0].raw
                ),
            );
        }
    }
    good
}

fn check_extrapolate(extrapolate: Option<bool>, place: &Place, report: &mut Report) {
    if extrapolate == Some(false) {
        report.error(
            Code::ExplicitDefault,
            &place.key("extrapolate"),
            "false is the default for extrapolate; omit it",
        );
    }
}

/// Conditions nest at most `MAX_NESTING` deep; deeper ones are reported and
/// not walked, so the check's own recursion is bounded too.
fn check_condition(condition: &Condition, place: &Place, depth: usize, report: &mut Report) {
    if depth > MAX_NESTING {
        report.error(
            Code::NestingDepth,
            place,
            format!("conditions nest deeper than the limit of {MAX_NESTING} levels"),
        );
        return;
    }
    match condition {
        Condition::Compare { value, .. } => {
            check_scalar(value, "condition value", &place.key("value"), report);
        }
        Condition::In { values, .. } => {
            if values.is_empty() {
                report.error(
                    Code::Empty,
                    &place.key("values"),
                    "an `in` condition lists at least one value",
                );
            }
            for (index, value) in values.iter().enumerate() {
                check_scalar(value, "condition value", &place.at("values", index), report);
            }
        }
        Condition::All { conditions } | Condition::Any { conditions } => {
            if conditions.is_empty() {
                report.error(
                    Code::Empty,
                    &place.key("conditions"),
                    "`all` and `any` list at least one condition",
                );
            }
            for (index, inner) in conditions.iter().enumerate() {
                check_condition(inner, &place.at("conditions", index), depth + 1, report);
            }
        }
        Condition::Not { condition } => {
            check_condition(condition, &place.key("condition"), depth + 1, report)
        }
    }
}

/// Struct types may nest, never contain themselves, and nest at most
/// `MAX_NESTING` deep, so decoding depth is bounded by a fixed limit every
/// implementation shares. The walk is iterative: a type graph is a flat map
/// in JSON, so its depth isn't bounded by the parser's nesting limit.
fn check_type_graph<'a>(
    types: &'a BTreeMap<String, StructType>,
    place: &Place,
    report: &mut Report,
) -> BTreeMap<&'a str, usize> {
    let uses: BTreeMap<&str, BTreeSet<&str>> = types
        .iter()
        .map(|(name, struct_type)| (name.as_str(), used_types(&struct_type.entries)))
        .collect();
    // On the stack while being explored; height once done (cycle members
    // count as height 1 so the walk can finish).
    let mut on_stack: BTreeSet<&str> = BTreeSet::new();
    let mut height: BTreeMap<&str, usize> = BTreeMap::new();
    let mut cycles: BTreeSet<BTreeSet<&str>> = BTreeSet::new();
    for &start in uses.keys() {
        if height.contains_key(start) {
            continue;
        }
        let mut stack: Vec<(&str, std::collections::btree_set::Iter<&str>)> =
            vec![(start, uses[start].iter())];
        on_stack.insert(start);
        while let Some((name, next)) = stack.last_mut() {
            let name = *name;
            match next.next().copied() {
                None => {
                    let below = uses[name]
                        .iter()
                        .filter_map(|used| height.get(used))
                        .max()
                        .copied()
                        .unwrap_or(0);
                    height.insert(name, below + 1);
                    on_stack.remove(name);
                    stack.pop();
                }
                Some(used) if on_stack.contains(used) => {
                    let at = stack.iter().position(|(n, _)| *n == used).unwrap_or(0);
                    let mut cycle: Vec<&str> = stack[at..].iter().map(|(n, _)| *n).collect();
                    cycle.push(used);
                    let members: BTreeSet<&str> = cycle.iter().copied().collect();
                    if cycles.insert(members) {
                        report.error(
                            Code::TypeCycle,
                            &place.map_item("types", cycle[0], "type"),
                            format!("struct types contain themselves: {}", cycle.join(" -> ")),
                        );
                    }
                }
                Some(used) if height.contains_key(used) => {}
                Some(used) => {
                    if let Some(next) = uses.get(used) {
                        on_stack.insert(used);
                        stack.push((used, next.iter()));
                    }
                }
            }
        }
    }
    if let Some((name, deepest)) = height
        .iter()
        .max_by_key(|(name, h)| (**h, std::cmp::Reverse(**name)))
        && *deepest > MAX_NESTING
    {
        report.error(
            Code::NestingDepth,
            &place.map_item("types", name, "type"),
            format!("struct type {name} nests {deepest} levels deep; the limit is {MAX_NESTING}"),
        );
    }
    height
}

pub(super) fn used_types(entries: &[Entry]) -> BTreeSet<&str> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Struct(entry) => Some(entry.struct_type.as_str()),
            Entry::Array(array) => match &array.element {
                Element::Struct { name } => Some(name.as_str()),
                Element::Scalar(_) => None,
            },
            Entry::Field(_) | Entry::Padding(_) | Entry::Checksum(_) => None,
        })
        .collect()
}
