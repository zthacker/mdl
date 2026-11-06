//! Canonical mapping: every field, array and composite that names a
//! catalog param produces what the param holds, in its unit and frame.

use std::collections::{BTreeMap, BTreeSet};

use crate::v2::diagnostics::{Code, Place, Report};
use crate::v2::ir::{
    ArrayEntry, Catalog, Composite, Conversion, Count, Element, Entry, Field, Frame, Packet, Param,
    ParamType, ScalarElement, ScalarType, StructType,
};

/// A scalar that may map to a param: a field, or the element of an array.
struct Source<'a> {
    ty: ScalarType,
    size_bits: Option<u32>,
    conversion: Option<&'a Conversion>,
    unit: Option<&'a str>,
}

impl<'a> Source<'a> {
    fn of_field(field: &'a Field) -> Self {
        Source {
            ty: field.ty,
            size_bits: field.size_bits,
            conversion: field.conversion.as_ref(),
            unit: field.unit.as_deref(),
        }
    }

    fn of_element(element: &'a ScalarElement) -> Self {
        Source {
            ty: element.ty,
            size_bits: element.size_bits,
            conversion: element.conversion.as_ref(),
            unit: element.unit.as_deref(),
        }
    }

    fn numeric(&self) -> bool {
        matches!(
            self.ty,
            ScalarType::Uint | ScalarType::Int | ScalarType::Float
        )
    }
}

/// Mapped entries of one container (a packet's or a struct type's).
pub(super) fn check_entries(
    entries: &[Entry],
    catalog: &Catalog,
    place: &Place,
    report: &mut Report,
) {
    for (index, entry) in entries.iter().enumerate() {
        match entry {
            Entry::Field(field) => {
                let entry_place = place.item("entries", index, "field", &field.name);
                if let Some(param) = mapped(&field.canonical, catalog) {
                    let source = Source::of_field(field);
                    check_source(&source, param, &entry_place, report);
                }
            }
            Entry::Array(array) => {
                let entry_place = place.item("entries", index, "array", &array.name);
                check_array(array, catalog, &entry_place, report);
            }
            Entry::Struct(_) | Entry::Padding(_) => {}
        }
    }
}

fn mapped<'c>(canonical: &Option<String>, catalog: &'c Catalog) -> Option<(&'c str, &'c Param)> {
    let name = canonical.as_deref()?;
    // Unknown names are reported by the reference check.
    catalog
        .params
        .get_key_value(name)
        .map(|(k, v)| (k.as_str(), v))
}

fn check_array(array: &ArrayEntry, catalog: &Catalog, place: &Place, report: &mut Report) {
    let Some(param) = mapped(&array.canonical, catalog) else {
        return;
    };
    match &array.element {
        Element::Scalar(element) => {
            check_source(&Source::of_element(element), param, place, report)
        }
        Element::Struct { .. } => report.error(
            Code::CanonicalType,
            &place.key("canonical"),
            "an array of structs maps the fields inside the struct type, not the array",
        ),
    }
}

/// Does this source produce what the param holds, in its unit?
fn check_source(
    source: &Source,
    (name, param): (&str, &Param),
    place: &Place,
    report: &mut Report,
) {
    let states = matches!(source.conversion, Some(Conversion::States { .. }));
    let plain = source.conversion.is_none();
    let fits = match param.ty {
        ParamType::F64 => source.numeric() && !states,
        // A JSON uint has no declared size; a value past i64 is then
        // caught per message.
        ParamType::I64 => {
            plain
                && (source.ty == ScalarType::Int
                    || (source.ty == ScalarType::Uint && source.size_bits.is_none_or(|s| s <= 63)))
        }
        ParamType::U64 => plain && source.ty == ScalarType::Uint,
        // Integer flags map too: zero is false, anything else true.
        ParamType::Bool => {
            plain
                && matches!(
                    source.ty,
                    ScalarType::Bool | ScalarType::Uint | ScalarType::Int
                )
        }
        ParamType::Str => plain && source.ty == ScalarType::String,
        ParamType::Bytes => plain && source.ty == ScalarType::Bytes,
        ParamType::Enum => states || (plain && source.ty == ScalarType::String),
        ParamType::Vec3 | ParamType::Quat => {
            report.error(
                Code::CanonicalType,
                &place.key("canonical"),
                format!(
                    "{name} is a vector; several fields map to it through a composite, not one \
                     field"
                ),
            );
            return;
        }
    };
    if !fits {
        report.error(
            Code::CanonicalType,
            &place.key("canonical"),
            format!(
                "this can't map to {name}, which holds {}",
                requirement(param.ty)
            ),
        );
        return;
    }
    // An enum param without states is reported on the param; don't repeat
    // it for every field that maps to it.
    if let (ParamType::Enum, Some(Conversion::States { states }), false) =
        (param.ty, source.conversion, param.states.is_empty())
    {
        for (index, state) in states.iter().enumerate() {
            if !param.states.contains(&state.name) {
                report.error(
                    Code::CanonicalType,
                    &place.key("conversion").at("states", index),
                    format!("state {:?} isn't one of {name}'s states", state.name),
                );
            }
        }
    }
    // Enums and strings carry names, not quantities; their params have no
    // unit to match.
    if param.ty != ParamType::Enum {
        check_unit(
            source.unit,
            param.unit.as_deref(),
            name,
            &place.key("unit"),
            report,
        );
    }
}

fn requirement(ty: ParamType) -> &'static str {
    match ty {
        ParamType::F64 => "an f64: a number, unconverted or through linear or poly",
        ParamType::I64 => "an i64: an int field, or a uint of at most 63 bits, unconverted",
        ParamType::U64 => "a u64: a uint field, unconverted",
        ParamType::Bool => "a bool: a bool or integer field, unconverted (nonzero is true)",
        ParamType::Str => "a str: a string field, unconverted",
        ParamType::Bytes => "bytes: a bytes field, unconverted",
        ParamType::Enum => "an enum: an integer field with a states conversion, or a string field",
        ParamType::Vec3 | ParamType::Quat => "a vector",
    }
}

pub(super) fn check_unit(
    unit: Option<&str>,
    expected: Option<&str>,
    param: &str,
    place: &Place,
    report: &mut Report,
) {
    if unit == expected {
        return;
    }
    let message = match (unit, expected) {
        (None, Some(expected)) => format!("the unit is missing; {param} is in {expected:?}"),
        (Some(unit), None) => format!("{param} has no unit, so this can't be in {unit:?}"),
        (Some(unit), Some(expected)) => {
            format!("unit {unit:?} doesn't match {param}, which is in {expected:?}")
        }
        (None, None) => return,
    };
    report.error(Code::UnitMismatch, place, message);
}

/// Composites of one packet: members resolve to numeric entries, in the
/// count, unit and a frame the param needs. Members are resolved even when
/// the param is unknown or not a vector, so a misspelled member is still
/// reported.
pub(super) fn check_composites(
    packet: &Packet,
    types: &BTreeMap<String, StructType>,
    catalog: &Catalog,
    place: &Place,
    report: &mut Report,
) {
    for (index, composite) in packet.composites.iter().enumerate() {
        let composite_place = place.item("composites", index, "composite", &composite.canonical);
        let param = catalog.params.get(&composite.canonical);
        check_composite(composite, param, packet, types, &composite_place, report);
    }
}

/// The params a container maps through its fields and arrays and through
/// any depth of struct entries, but not through arrays of structs.
pub(super) type Mapped<'a> = BTreeSet<&'a str>;

/// What every struct type maps, computed once per type in nesting order
/// (`heights`), so a struct entry takes its type's set instead of walking
/// into it again: linear in the size of the set, however the types fan
/// out. A type that maps one param twice is reported on the type itself.
pub(super) fn type_mappings<'a>(
    types: &'a BTreeMap<String, StructType>,
    heights: &BTreeMap<&str, usize>,
    catalog: &Catalog,
    place: &Place,
    report: &mut Report,
) -> BTreeMap<&'a str, Mapped<'a>> {
    let mut order: Vec<(&String, &StructType)> = types.iter().collect();
    order.sort_by_key(|(name, _)| heights.get(name.as_str()).copied().unwrap_or(0));
    let mut mapped = BTreeMap::new();
    for (name, struct_type) in order {
        let type_place = place.map_item("types", name, "type");
        let own = collect_mapped(
            "struct type",
            &struct_type.entries,
            &mapped,
            catalog,
            &type_place,
            report,
        );
        mapped.insert(name.as_str(), own);
    }
    mapped
}

/// Two entries of one packet that map to the same param would give two
/// samples at one instant with nothing to tell them apart. Fields inside
/// struct entries count, through any depth of structs; arrays of structs
/// don't, because their samples carry an index. A mapping that's already
/// wrong for its param (a field on a vector, a composite on a scalar) is
/// reported elsewhere and not counted here.
pub(super) fn check_duplicate_mappings(
    packet: &Packet,
    type_mappings: &BTreeMap<&str, Mapped>,
    catalog: &Catalog,
    place: &Place,
    report: &mut Report,
) {
    let mut mapped = collect_mapped(
        "packet",
        &packet.entries,
        type_mappings,
        catalog,
        place,
        report,
    );
    for (index, composite) in packet.composites.iter().enumerate() {
        if is_vector(catalog, &composite.canonical) != Some(true) {
            continue;
        }
        let at = place
            .item("composites", index, "composite", &composite.canonical)
            .key("canonical");
        add_mapping("packet", &mut mapped, &composite.canonical, &at, report);
    }
}

fn is_vector(catalog: &Catalog, name: &str) -> Option<bool> {
    catalog
        .params
        .get(name)
        .map(|p| matches!(p.ty, ParamType::Vec3 | ParamType::Quat))
}

/// One container's mapped params: its own fields and arrays, plus what its
/// struct entries' types map. Reports each entry that maps a param a
/// previous entry already maps.
fn collect_mapped<'a>(
    container: &str,
    entries: &'a [Entry],
    type_mappings: &BTreeMap<&str, Mapped<'a>>,
    catalog: &Catalog,
    place: &Place,
    report: &mut Report,
) -> Mapped<'a> {
    let mut mapped = Mapped::new();
    for (index, entry) in entries.iter().enumerate() {
        match entry {
            Entry::Field(Field {
                name,
                canonical: Some(canonical),
                ..
            }) if is_vector(catalog, canonical) == Some(false) => {
                let at = place.item("entries", index, "field", name).key("canonical");
                add_mapping(container, &mut mapped, canonical, &at, report);
            }
            Entry::Array(ArrayEntry {
                name,
                canonical: Some(canonical),
                ..
            }) if is_vector(catalog, canonical) == Some(false) => {
                let at = place.item("entries", index, "array", name).key("canonical");
                add_mapping(container, &mut mapped, canonical, &at, report);
            }
            Entry::Struct(inner) => {
                let Some(inner_mapped) = type_mappings.get(inner.struct_type.as_str()) else {
                    continue;
                };
                let at = place.item("entries", index, "struct", &inner.name);
                for param in inner_mapped {
                    add_mapping(container, &mut mapped, param, &at, report);
                }
            }
            _ => {}
        }
    }
    mapped
}

fn add_mapping<'a>(
    container: &str,
    mapped: &mut Mapped<'a>,
    param: &'a str,
    at: &Place,
    report: &mut Report,
) {
    if !mapped.insert(param) {
        report.error(
            Code::DuplicateMapping,
            at,
            format!(
                "{param} is already mapped by another entry of this {container}; each param comes \
                 from one entry per packet"
            ),
        );
    }
}

fn check_composite(
    composite: &Composite,
    param: Option<&Param>,
    packet: &Packet,
    types: &BTreeMap<String, StructType>,
    place: &Place,
    report: &mut Report,
) {
    let name = &composite.canonical;
    // Unknown params are reported by the reference check.
    let vector = param.and_then(|param| match param.ty {
        ParamType::Vec3 => Some((param, 3)),
        ParamType::Quat => Some((param, 4)),
        _ => {
            report.error(
                Code::CompositeShape,
                &place.key("canonical"),
                format!("{name} isn't a vector; composites map to vec3 or quat params"),
            );
            None
        }
    });
    if let Some((param, expected)) = vector {
        if !composite.members.is_empty() && composite.members.len() != expected {
            report.error(
                Code::CompositeShape,
                &place.key("members"),
                format!(
                    "{name} needs {expected} members ({}), got {}",
                    if expected == 3 {
                        "x, y, z"
                    } else {
                        "w, x, y, z"
                    },
                    composite.members.len()
                ),
            );
        }
        if let Some(to) = param.frame
            && !frame_converts(composite.frame, to, param.ty == ParamType::Quat)
        {
            report.error(
                Code::FrameMismatch,
                &place.key("frame"),
                format!(
                    "{} can't be converted to {name}'s frame {}",
                    frame_name(composite.frame),
                    frame_name(to)
                ),
            );
        }
    }
    let geodetic = composite.frame == Frame::Geodetic;
    let mut seen = BTreeSet::new();
    for (index, member) in composite.members.iter().enumerate() {
        let member_place = place.at("members", index);
        if !seen.insert(member.as_str()) {
            report.error(
                Code::DuplicateValue,
                &member_place,
                format!("{member} is listed twice"),
            );
            continue;
        }
        let source = match resolve(member, &packet.entries, types) {
            Ok(source) => source,
            Err(Some(message)) => {
                report.error(Code::UnknownEntry, &member_place, message);
                continue;
            }
            // Through a struct type that's already reported.
            Err(None) => continue,
        };
        if !source.numeric()
            || source
                .conversion
                .is_some_and(|c| matches!(c, Conversion::States { .. }))
        {
            report.error(
                Code::CanonicalType,
                &member_place,
                format!("{member} isn't a number, so it can't be part of {name}"),
            );
            continue;
        }
        let Some((param, _)) = vector else {
            continue;
        };
        // Geodetic positions are latitude and longitude in degrees, then
        // altitude in the param's unit.
        let unit = if geodetic && index < 2 {
            Some("deg")
        } else {
            param.unit.as_deref()
        };
        check_unit(source.unit, unit, name, &member_place, report);
    }
}

/// Frames consumers convert between: a vector converts to itself, and
/// along any chain of these steps: ECEF and ECI_J2000 into each other (with
/// time), NED and ENU into each other (axis order), and NED, ENU or
/// GEODETIC into ECEF (with the asset's position). So GEODETIC reaches
/// ECI_J2000 through ECEF. Attitude quaternions only keep their frame.
fn frame_converts(from: Frame, to: Frame, quaternion: bool) -> bool {
    use Frame::*;
    if from == to {
        return true;
    }
    if quaternion {
        return false;
    }
    let steps = |frame: Frame| -> &'static [Frame] {
        match frame {
            Ecef => &[EciJ2000],
            EciJ2000 => &[Ecef],
            Ned => &[Enu, Ecef],
            Enu => &[Ned, Ecef],
            Geodetic => &[Ecef],
            Body => &[],
        }
    };
    let mut reached = vec![from];
    let mut next = 0;
    while let Some(&frame) = reached.get(next) {
        for &step in steps(frame) {
            if step == to {
                return true;
            }
            if !reached.contains(&step) {
                reached.push(step);
            }
        }
        next += 1;
    }
    false
}

fn frame_name(frame: Frame) -> &'static str {
    match frame {
        Frame::Ecef => "ECEF",
        Frame::EciJ2000 => "ECI_J2000",
        Frame::Body => "BODY",
        Frame::Ned => "NED",
        Frame::Enu => "ENU",
        Frame::Geodetic => "GEODETIC",
    }
}

/// A struct type's entries. A type that's missing or empty is already
/// reported, so a path through it stops quietly (`Err(None)`).
fn type_entries<'a>(
    types: &'a BTreeMap<String, StructType>,
    name: &str,
) -> Result<&'a [Entry], Option<String>> {
    match types.get(name) {
        Some(struct_type) if !struct_type.entries.is_empty() => Ok(&struct_type.entries),
        _ => Err(None),
    }
}

/// Follow a member path (`pos.x`, `cells[2].volts`) to a scalar. `Err(None)`
/// means the path runs through a struct type that's already reported.
fn resolve<'a>(
    path: &str,
    entries: &'a [Entry],
    types: &'a BTreeMap<String, StructType>,
) -> Result<Source<'a>, Option<String>> {
    let fail = |message: String| Err(Some(message));
    let mut container = entries;
    let mut parts = path.split('.').peekable();
    while let Some(part) = parts.next() {
        let last = parts.peek().is_none();
        let (name, index) = match part.split_once('[') {
            Some((name, rest)) => match rest.strip_suffix(']').and_then(canonical_index) {
                Some(index) => (name, Some(index)),
                None => {
                    return fail(format!(
                        "{part:?} in {path} isn't name[index] with a plain decimal index (no \
                         sign or leading zeros)"
                    ));
                }
            },
            None => (part, None),
        };
        let Some(entry) = container.iter().find(|e| match e {
            Entry::Field(f) => f.name == name,
            Entry::Array(a) => a.name == name,
            Entry::Struct(s) => s.name == name,
            Entry::Padding(_) => false,
        }) else {
            return fail(format!("{path}: there is no entry {name}"));
        };
        match (entry, index) {
            (Entry::Field(field), None) if last => return Ok(Source::of_field(field)),
            (Entry::Struct(s), None) if !last => {
                container = type_entries(types, &s.struct_type)?;
            }
            (Entry::Array(array), Some(i)) => {
                match &array.count {
                    Some(Count::Fixed { count }) if i >= *count => {
                        return fail(format!(
                            "{path}: {name} has {count} elements, so [{i}] is past its end"
                        ));
                    }
                    Some(Count::Field(_) | Count::Remaining { .. }) => {
                        return fail(format!(
                            "{path}: {name} has a varying number of elements, so a member can't \
                             name one by index"
                        ));
                    }
                    _ => {}
                }
                match (&array.element, last) {
                    (Element::Scalar(element), true) => return Ok(Source::of_element(element)),
                    (Element::Struct { name: type_name }, false) => {
                        container = type_entries(types, type_name)?;
                    }
                    _ => return fail(format!("{path} doesn't end at a number")),
                }
            }
            _ => return fail(format!("{path} doesn't lead to a single number")),
        }
    }
    fail(format!("{path} doesn't lead to a single number"))
}

/// An index written one way only: decimal digits, no sign, no leading zero.
fn canonical_index(text: &str) -> Option<u32> {
    let plain = !text.is_empty()
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    if plain { text.parse().ok() } else { None }
}
