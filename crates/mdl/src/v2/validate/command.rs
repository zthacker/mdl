//! Command packet checks: each encodes one catalog command, every field
//! gets its value from exactly one place, and a set's command packets can
//! be told apart.

use std::collections::{BTreeMap, BTreeSet};

use super::layout::static_offsets;
use super::mapping::check_unit;
use super::rules::{
    check_integer_spelling, check_scalar, integer_value, is_identifier, pointer_tokens,
};
use super::set::{KeyId, KeyValue, by_value, exact};
use crate::v2::convert::{Converted, convert, inverse, inverts_exactly};
use crate::v2::diagnostics::{Code, Place, Report};
use crate::v2::encode::raw_unchecked;
use crate::v2::ir::{
    Argument, ByteLength, Catalog, CommandPacket, Conversion, Encoding, Entry, Family, Field,
    ParamType, Scalar, ScalarType, Set,
};
use crate::v2::text;
use crate::v2::units::convert_between;

/// Every command packet of a set: names, the command it encodes, link
/// metadata, and its fields. Positions and references are checked with
/// the set's layout.
pub(super) fn check_packets(
    set: &Set,
    family: Option<&Family>,
    catalog: &Catalog,
    place: &Place,
    report: &mut Report,
) {
    let mut names: BTreeMap<&str, usize> = BTreeMap::new();
    let mut commands: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, packet) in set.commands.iter().enumerate() {
        let packet_place = place.item("commands", index, "command", &packet.name);
        if !is_identifier(&packet.name) {
            report.error(
                Code::InvalidName,
                &packet_place.key("name"),
                "command packet names are letters, digits and '_', not starting with a digit",
            );
        }
        if let Some(first) = names.get(packet.name.as_str()) {
            report.duplicate(
                Code::DuplicateName,
                &packet_place,
                &place.at("commands", *first),
                format!(
                    "command packet {} is defined twice in this set",
                    packet.name
                ),
            );
        } else {
            names.insert(&packet.name, index);
        }
        check_metadata(packet, family, &packet_place, report);
        // The encoder looks a packet up by command, so there's one.
        if let Some(first) = commands.get(packet.canonical.as_str()) {
            report.duplicate(
                Code::DuplicateMapping,
                &packet_place.key("canonical"),
                &place.at("commands", *first),
                format!(
                    "command {} is encoded by two command packets; a set encodes each command \
                     once",
                    packet.canonical
                ),
            );
        } else {
            commands.insert(&packet.canonical, index);
        }
        let command = catalog.commands.get(&packet.canonical);
        if command.is_none() {
            report.error(
                Code::UnknownCommand,
                &packet_place.key("canonical"),
                format!("the catalog has no command {}", packet.canonical),
            );
        }
        let arguments = command.map(|command| command.arguments.as_slice());
        check_entries(packet, arguments, &packet_place, report);
    }
}

/// The keys that identify each command packet of a set, by value: its
/// metadata, and its constant fields that a decoder can read without
/// decoding anything else. In a binary family those are integer and bool
/// fields and fixed-length strings at static positions; in a JSON family,
/// integer and string fields. Decoding a command reads exactly these.
pub(crate) fn command_identities(set: &Set, family: &Family) -> Vec<BTreeMap<KeyId, KeyValue>> {
    set.commands
        .iter()
        .map(|packet| {
            let mut keys: BTreeMap<KeyId, KeyValue> = packet
                .metadata
                .iter()
                .map(|(name, value)| (KeyId::Metadata(name.clone()), by_value(value)))
                .collect();
            // Command packets hold no struct types.
            let offsets = match family.encoding {
                Encoding::Binary => static_offsets(&packet.entries, &BTreeMap::new()),
                _ => Vec::new(),
            };
            for (index, entry) in packet.entries.iter().enumerate() {
                let Entry::Field(field) = entry else {
                    continue;
                };
                let (Some(value), None) = (&field.value, &field.when) else {
                    continue;
                };
                let key = match family.encoding {
                    Encoding::Binary => {
                        binary_key(field, value, offsets.get(index).copied().flatten())
                    }
                    _ => json_key(field, value),
                };
                if let Some((id, value)) = key {
                    keys.insert(id, value);
                }
            }
            keys
        })
        .collect()
}

/// A constant field at a static position, as what it puts in the payload:
/// an integer or bool as its raw unsigned bits (an int8 -1 and a uint8 255
/// are the same key and value, an int8 -1 and a uint8 1 differ), a
/// fixed-length string as its bytes.
fn binary_key(field: &Field, value: &Scalar, offset: Option<u64>) -> Option<(KeyId, KeyValue)> {
    let offset = offset?;
    if let (ScalarType::String, Scalar::Str(text), Some(ByteLength::Fixed { bytes })) =
        (field.ty, value, &field.length)
    {
        let encoded = text::encode(text, field.text)?;
        let fits = offset.is_multiple_of(8) && encoded.len() == *bytes as usize;
        let key = KeyId::Bytes {
            offset,
            length: *bytes,
        };
        return fits.then_some((key, KeyValue::Bytes(encoded)));
    }
    let size = field.size_bits.filter(|size| (1..=64).contains(size))?;
    if !matches!(
        field.ty,
        ScalarType::Uint | ScalarType::Int | ScalarType::Bool
    ) {
        return None;
    }
    // Two's complement in the field's size; values out of range are
    // reported by check_value, which leaves the packet out of check_distinct.
    let raw = integer_value(value)? & ((1_i128 << size) - 1);
    Some((
        KeyId::Bits {
            offset,
            size,
            signed: false,
        },
        KeyValue::Int(raw),
    ))
}

/// A constant integer or string field, by its pointer.
fn json_key(field: &Field, value: &Scalar) -> Option<(KeyId, KeyValue)> {
    let pointer = KeyId::Pointer(field.pointer.clone()?);
    match (field.ty, value) {
        (ScalarType::Uint | ScalarType::Int, _) => {
            Some((pointer, KeyValue::Int(integer_value(value)?)))
        }
        (ScalarType::String, Scalar::Str(_)) => Some((pointer, exact(value))),
        _ => None,
    }
}

/// Every two command packets of a set differ in some key both have, so a
/// payload matches at most one of them. Runs after every other check of
/// the set: a packet with problems of its own is left out, since a key it
/// lost to them would only echo them here.
pub(super) fn check_distinct(set: &Set, family: &Family, place: &Place, report: &mut Report) {
    let identities = command_identities(set, family);
    let places: Vec<Place> = set
        .commands
        .iter()
        .enumerate()
        .map(|(index, packet)| place.item("commands", index, "command", &packet.name))
        .collect();
    let clean: Vec<bool> = places
        .iter()
        .map(|place| !report.reported_under(place))
        .collect();
    for (later, keys) in identities.iter().enumerate() {
        if !clean.get(later).copied().unwrap_or(false) {
            continue;
        }
        let alike = identities[..later]
            .iter()
            .enumerate()
            .position(|(index, earlier)| {
                clean.get(index).copied().unwrap_or(false)
                    && !earlier
                        .iter()
                        .any(|(key, value)| keys.get(key).is_some_and(|other| other != value))
            });
        let (Some(earlier), Some(packet), Some(packet_place)) =
            (alike, set.commands.get(later), places.get(later))
        else {
            continue;
        };
        let first = set
            .commands
            .get(earlier)
            .map_or("", |packet| packet.name.as_str());
        report.error(
            Code::AmbiguousIdentify,
            packet_place,
            format!(
                "nothing tells command packets {first} and {} apart: give both a constant field \
                 at the same position, or metadata of the same name, with different values",
                packet.name
            ),
        );
    }
}

fn check_metadata(
    packet: &CommandPacket,
    family: Option<&Family>,
    place: &Place,
    report: &mut Report,
) {
    for (name, value) in &packet.metadata {
        let value_place = place.map_item("metadata", name, "metadata");
        if let Some(family) = family
            && !family.metadata.iter().any(|declared| declared == name)
        {
            report.error(
                Code::UnknownMetadata,
                &value_place,
                format!("family {} doesn't declare metadata {name}", family.name),
            );
        }
        check_scalar(value, "metadata value", &value_place, report);
        check_integer_spelling(value, "metadata value", &value_place, report);
    }
}

/// What a command packet holds, and where each field's value comes from.
/// `arguments` is `None` when the command is unknown (reported already).
fn check_entries(
    packet: &CommandPacket,
    arguments: Option<&[Argument]>,
    place: &Place,
    report: &mut Report,
) {
    // Fields a later entry takes a length from get their value from the
    // data; one data field per length field, so the length has one answer.
    let mut computed: BTreeSet<&str> = BTreeSet::new();
    for (index, entry) in packet.entries.iter().enumerate() {
        if let Entry::Field(Field {
            name,
            length: Some(ByteLength::Field(from)),
            ..
        }) = entry
            && !computed.insert(from.field.as_str())
        {
            report.error(
                Code::CommandShape,
                &place
                    .item("entries", index, "field", name)
                    .key("length")
                    .key("field"),
                format!(
                    "another field already takes its length from {}; in a command packet a \
                     length field gives the length of one field",
                    from.field
                ),
            );
        }
    }
    let mut encoded: BTreeMap<&str, usize> = BTreeMap::new();
    let mut target: Option<usize> = None;
    for (index, entry) in packet.entries.iter().enumerate() {
        let condition = match entry {
            Entry::Field(field) => field
                .when
                .is_some()
                .then_some(("field", field.name.as_str())),
            Entry::Padding(padding) => padding.when.is_some().then_some(("padding", "")),
            _ => None,
        };
        if let Some((label, name)) = condition {
            report.error(
                Code::CommandShape,
                &place.item("entries", index, label, name).key("when"),
                "every entry of a command packet is written; a condition could silently drop an \
                 argument",
            );
        }
        let field = match entry {
            Entry::Field(field) => field,
            Entry::Padding(_) | Entry::Checksum(_) => continue,
            Entry::Array(array) => {
                let entry_place = place.item("entries", index, "array", &array.name);
                report.error(
                    Code::CommandShape,
                    &entry_place,
                    "a command packet holds fields, padding and checksums",
                );
                continue;
            }
            Entry::Struct(inner) => {
                let entry_place = place.item("entries", index, "struct", &inner.name);
                report.error(
                    Code::CommandShape,
                    &entry_place,
                    "a command packet holds fields, padding and checksums",
                );
                continue;
            }
        };
        let field_place = place.item("entries", index, "field", &field.name);
        check_telemetry_keys(field, &field_place, report);
        let sources = [
            field.value.is_some(),
            field.argument.is_some(),
            field.target == Some(true),
            computed.contains(field.name.as_str()),
        ];
        match sources.iter().filter(|&&source| source).count() {
            0 => report.error(
                Code::NoValue,
                &field_place,
                format!(
                    "field {} gets no value: give it a value, an argument or the target, or let \
                     a later entry's length come from it",
                    field.name
                ),
            ),
            1 => {}
            _ => report.error(
                Code::CommandShape,
                &field_place,
                format!(
                    "field {} gets its value from more than one place (a value, an argument, \
                     the target, a later entry's length)",
                    field.name
                ),
            ),
        }
        if let Some(value) = &field.value {
            check_value(field, value, &field_place.key("value"), report);
        }
        if let Some(is_target) = field.target {
            check_target(field, is_target, &field_place, report);
        }
        // One target field, so a payload can't hold two ids that disagree.
        if field.target == Some(true) {
            match target {
                Some(first) => report.error(
                    Code::CommandShape,
                    &field_place.key("target"),
                    format!(
                        "entries/{first} already holds the target's native id; a command packet \
                         holds it once"
                    ),
                ),
                None => target = Some(index),
            }
        }
        let needs_inverse = field
            .conversion
            .as_ref()
            .is_some_and(|c| !inverts_exactly(c));
        if field.inverse.is_some() && !needs_inverse {
            report.error(
                Code::ConversionType,
                &field_place.key("inverse"),
                "an inverse is for a conversion that doesn't invert exactly (a polynomial, a \
                 spline, a step table, a table whose values rise and fall); this field's does, or \
                 it has none",
            );
        }
        if let Some(inverse) = &field.inverse
            && !matches!(
                inverse,
                Conversion::Linear { .. } | Conversion::Poly { .. } | Conversion::Table { .. }
            )
        {
            report.error(
                Code::ConversionType,
                &field_place.key("inverse"),
                "an inverse is a linear, polynomial or table conversion, engineering value to raw",
            );
        }
        let (Some(name), Some(arguments)) = (&field.argument, arguments) else {
            if field.argument.is_none() && field.inverse.is_some() {
                report.error(
                    Code::ConversionType,
                    &field_place.key("inverse"),
                    "an inverse converts an argument, and this field encodes none",
                );
            }
            continue;
        };
        let argument_place = field_place.key("argument");
        let Some(argument) = arguments.iter().find(|argument| &argument.name == name) else {
            report.error(
                Code::UnknownArgument,
                &argument_place,
                format!("command {} has no argument {name}", packet.canonical),
            );
            continue;
        };
        if let Some(first) = encoded.get(name.as_str()) {
            report.error(
                Code::DuplicateMapping,
                &argument_place,
                format!("argument {name} is already encoded by entries/{first}"),
            );
        } else {
            encoded.insert(name, index);
        }
        check_argument_field(field, argument, &field_place, report);
    }
    check_pointers(packet, place, report);
    // A set may leave out an argument with a default (an older layout, from
    // before the argument was added); the encoder then accepts only the
    // default for it.
    for argument in arguments.into_iter().flatten() {
        if !encoded.contains_key(argument.name.as_str()) && argument.default.is_none() {
            report.error(
                Code::UnmappedArgument,
                &place.key("canonical"),
                format!(
                    "no field encodes argument {} of command {}",
                    argument.name, packet.canonical
                ),
            );
        }
    }
}

/// Each field of a JSON command packet is written at its own place inside
/// the payload's object: no pointer is empty (the whole document), and none
/// equals another or leads into it (`/a` and `/a/b`).
fn check_pointers(packet: &CommandPacket, place: &Place, report: &mut Report) {
    let mut written: Vec<(Vec<String>, &str)> = Vec::new();
    for (index, entry) in packet.entries.iter().enumerate() {
        let Entry::Field(field) = entry else {
            continue;
        };
        let Some(tokens) = field.pointer.as_deref().and_then(pointer_tokens) else {
            continue;
        };
        if tokens.is_empty() {
            report.error(
                Code::CommandShape,
                &place
                    .item("entries", index, "field", &field.name)
                    .key("pointer"),
                format!(
                    "a command payload is a JSON object, so field {} is written under a key, not \
                     at the empty pointer",
                    field.name
                ),
            );
            continue;
        }
        if let Some((_, other)) = written
            .iter()
            .find(|(earlier, _)| earlier.starts_with(&tokens) || tokens.starts_with(earlier))
        {
            report.error(
                Code::Overlap,
                &place
                    .item("entries", index, "field", &field.name)
                    .key("pointer"),
                format!(
                    "{} and {other} would be written to the same place: one pointer equals or \
                     leads into the other",
                    field.name
                ),
            );
            continue;
        }
        written.push((tokens, &field.name));
    }
}

/// Keys that only mean something for telemetry.
fn check_telemetry_keys(field: &Field, place: &Place, report: &mut Report) {
    if field.canonical.is_some() {
        report.error(
            Code::CommandShape,
            &place.key("canonical"),
            "canonical maps telemetry to a param; a command field takes an argument or a value",
        );
    }
    if !field.invalid.is_empty() {
        report.error(
            Code::CommandShape,
            &place.key("invalid"),
            "sentinels mark telemetry that has no reading; a command field always has a value",
        );
    }
    if field.optional.is_some() {
        report.error(
            Code::CommandShape,
            &place.key("optional"),
            "optional is for telemetry members that may be missing; a command field is always \
             written",
        );
    }
}

/// A target field holds the asset's native id as it is: an integer or
/// text, unconverted.
fn check_target(field: &Field, target: bool, place: &Place, report: &mut Report) {
    if !target {
        report.error(
            Code::ExplicitDefault,
            &place.key("target"),
            "false is the default for target; omit it",
        );
        return;
    }
    if !matches!(
        field.ty,
        ScalarType::Uint | ScalarType::Int | ScalarType::String
    ) {
        report.error(
            Code::CommandShape,
            &place.key("target"),
            format!(
                "a native id is an integer or text, so target field {} is a uint, int or string \
                 field",
                field.name
            ),
        );
    }
    for (present, key) in [
        (field.conversion.is_some(), "conversion"),
        (field.inverse.is_some(), "inverse"),
        (field.unit.is_some(), "unit"),
    ] {
        if present {
            report.error(
                Code::CommandShape,
                &place.key(key),
                format!(
                    "target field {} writes the native id as it is, with no {key}",
                    field.name
                ),
            );
        }
    }
}

/// A constant fits its field: an integer or bool field holds the integer,
/// a string field holds the text.
fn check_value(field: &Field, value: &Scalar, place: &Place, report: &mut Report) {
    match (field.ty, value) {
        (ScalarType::String, Scalar::Str(text)) => check_text_value(field, text, place, report),
        (ScalarType::String, Scalar::Int(_)) => report.error(
            Code::ReferenceType,
            place,
            format!("{} is a string field, so its value is a string", field.name),
        ),
        (ScalarType::Bytes | ScalarType::Float, _) => report.error(
            Code::ReferenceType,
            place,
            format!(
                "a constant value is an integer, bool or string; {} can't have one",
                field.name
            ),
        ),
        _ => {
            check_scalar(value, "value", place, report);
            check_integer_spelling(value, "value", place, report);
            let Some(n) = integer_value(value) else {
                report.error(
                    Code::ReferenceType,
                    place,
                    format!("{} holds integers, so its value is one", field.name),
                );
                return;
            };
            if let Some((low, high)) = integer_range(field)
                && !(low..=high).contains(&n)
            {
                report.error(
                    Code::ValueRange,
                    place,
                    format!(
                        "{n} doesn't fit {}, which holds {low} to {high}",
                        field.name
                    ),
                );
            }
        }
    }
}

/// A string constant can be written in its field: in its text encoding, at
/// exactly a fixed length, without a terminator byte inside it.
fn check_text_value(field: &Field, value: &str, place: &Place, report: &mut Report) {
    let Some(encoded) = text::encode(value, field.text) else {
        report.error(
            Code::ReferenceType,
            place,
            format!(
                "{value:?} can't be written as {}, field {}'s text encoding",
                text::name(field.text),
                field.name
            ),
        );
        return;
    };
    match &field.length {
        Some(ByteLength::Fixed { bytes }) if encoded.len() != *bytes as usize => report.error(
            Code::ValueRange,
            place,
            format!(
                "{value:?} is {} bytes, and field {} is exactly {bytes}",
                encoded.len(),
                field.name
            ),
        ),
        Some(ByteLength::Terminator { byte }) if encoded.contains(byte) => report.error(
            Code::ValueRange,
            place,
            format!(
                "{value:?} contains field {}'s terminator byte {byte:#04x}",
                field.name
            ),
        ),
        _ => {}
    }
}

/// The integers a binary integer or bool field holds; `None` without a
/// size (a JSON field, or a size reported elsewhere).
fn integer_range(field: &Field) -> Option<(i128, i128)> {
    match field.ty {
        ScalarType::Bool => Some((0, 1)),
        ScalarType::Int => {
            let size = field.size_bits.filter(|s| (1..=64).contains(s))?;
            Some((-(1_i128 << (size - 1)), (1_i128 << (size - 1)) - 1))
        }
        ScalarType::Uint => {
            let size = field.size_bits.filter(|s| (1..=64).contains(s))?;
            Some((0, (1_i128 << size) - 1))
        }
        _ => None,
    }
}

/// An argument's type fits the field that encodes it, through the
/// field's conversion, in the field's unit.
fn check_argument_field(field: &Field, argument: &Argument, place: &Place, report: &mut Report) {
    let states = matches!(field.conversion, Some(Conversion::States { .. }));
    let plain = field.conversion.is_none();
    let numeric = matches!(
        field.ty,
        ScalarType::Uint | ScalarType::Int | ScalarType::Float
    );
    let fits = match argument.ty {
        ParamType::F64 => numeric && !states,
        ParamType::I64 => plain && matches!(field.ty, ScalarType::Int | ScalarType::Uint),
        ParamType::U64 => plain && field.ty == ScalarType::Uint,
        ParamType::Bool => {
            plain
                && matches!(
                    field.ty,
                    ScalarType::Bool | ScalarType::Uint | ScalarType::Int
                )
        }
        ParamType::Str => plain && field.ty == ScalarType::String,
        ParamType::Bytes => plain && field.ty == ScalarType::Bytes,
        ParamType::Enum => {
            (states && matches!(field.ty, ScalarType::Uint | ScalarType::Int))
                || (plain && field.ty == ScalarType::String)
        }
        // Reported on the argument.
        ParamType::Vec3 | ParamType::Quat => return,
    };
    if !fits {
        report.error(
            Code::CanonicalType,
            &place.key("argument"),
            format!(
                "field {} can't encode argument {}, which is {}",
                field.name,
                argument.name,
                requirement(argument.ty)
            ),
        );
        return;
    }
    if let Some(conversion) = &field.conversion
        && !inverts_exactly(conversion)
        && field.inverse.is_none()
    {
        report.error(
            Code::ConversionType,
            &place.key("conversion"),
            format!(
                "{} doesn't invert exactly, so a field that encodes an argument through it needs \
                 an inverse (engineering value to raw)",
                conversion_name(conversion)
            ),
        );
    }
    // Coverage is checked at the range's ends, which bound the raw values
    // only once the inverse is known to be monotonic over the range.
    let monotonic = check_inverse(field, argument, place, report);
    if let Some(Conversion::Linear { scale, .. }) = field.conversion
        && scale == 0.0
    {
        report.error(
            Code::ConversionType,
            &place.key("conversion").key("scale"),
            "a linear conversion with scale 0 has no inverse, so it can't encode an argument",
        );
    }
    if let (Some(Conversion::States { states }), false) =
        (&field.conversion, argument.states.is_empty())
    {
        let mut seen = BTreeSet::new();
        let mut misspelled = false;
        for (index, state) in states.iter().enumerate() {
            let state_place = place.key("conversion").at("states", index);
            if !argument.states.contains(&state.name) {
                misspelled = true;
                report.error(
                    Code::CanonicalType,
                    &state_place,
                    format!(
                        "state {:?} isn't one of argument {}'s states",
                        state.name, argument.name
                    ),
                );
            }
            if !seen.insert(&state.name) {
                report.error(
                    Code::DuplicateValue,
                    &state_place,
                    format!(
                        "state {:?} is listed twice, so encoding it would have two raw values",
                        state.name
                    ),
                );
            }
        }
        // A name that isn't a state is most likely a misspelling of a
        // missing one; report it alone.
        for state in argument.states.iter().filter(|_| !misspelled) {
            if !states.iter().any(|listed| &listed.name == state) {
                report.error(
                    Code::CanonicalType,
                    &place.key("conversion"),
                    format!(
                        "argument {}'s state {state:?} has no raw value here, so it couldn't be \
                         sent",
                        argument.name
                    ),
                );
            }
        }
    }
    if monotonic {
        check_coverage(field, argument, place, report);
    }
    if matches!(
        argument.ty,
        ParamType::F64 | ParamType::I64 | ParamType::U64
    ) {
        check_unit(
            field.unit.as_deref(),
            argument.unit.as_deref(),
            &argument.name,
            argument.ty == ParamType::F64,
            &place.key("unit"),
            report,
        );
    }
}

/// Evenly spaced points at which a polynomial inverse is checked over its
/// argument's range (and the ends), as `spec/encoding.md` §3.1 says.
const INVERSE_SAMPLES: u32 = 4096;

fn conversion_name(conversion: &Conversion) -> &'static str {
    match conversion {
        Conversion::Linear { .. } => "a linear conversion",
        Conversion::Poly { .. } => "a polynomial",
        Conversion::States { .. } => "a states conversion",
        Conversion::Spline { .. } => "a spline",
        Conversion::Table {
            interpolation: Some(crate::v2::ir::Interpolation::Step),
            ..
        } => "a step table",
        Conversion::Table { .. } => "a table whose values rise and fall",
    }
}

/// Through a conversion other than linear or states, an argument needs a
/// range with both ends to check its inverse over: the inverse must reach
/// both ends (inside a table's or spline's points, unless it extrapolates),
/// and an explicit inverse must only rise or only fall across the range,
/// which decoding relies on. Whether coverage can then be checked at the
/// ends.
fn check_inverse(field: &Field, argument: &Argument, place: &Place, report: &mut Report) -> bool {
    let Some(conversion) = &field.conversion else {
        return true;
    };
    if matches!(
        conversion,
        Conversion::Linear { .. } | Conversion::States { .. }
    ) {
        return true;
    }
    if !inverts_exactly(conversion) && field.inverse.is_none() {
        return false;
    }
    let range = argument.range.unwrap_or_default();
    let (Some(min), Some(max)) = (range.min, range.max) else {
        // A range with no ends is reported on the catalog.
        if argument
            .range
            .is_some_and(|r| r.min.is_none() && r.max.is_none())
        {
            return false;
        }
        report.error(
            Code::ValueRange,
            &place.key("argument"),
            format!(
                "argument {} is encoded through {}, so it needs a range with both ends to check \
                 the conversion over",
                argument.name,
                conversion_name(conversion)
            ),
        );
        return false;
    };
    // The range in the field's unit; units that don't convert are reported
    // on their own.
    let to_field = |x: f64| convert_between(x, argument.unit.as_deref(), field.unit.as_deref());
    let (Some(field_min), Some(field_max)) = (to_field(min), to_field(max)) else {
        return false;
    };
    for (end, field_end) in [(min, field_min), (max, field_max)] {
        if inverse(field, field_end) == Converted::Outside {
            report.error(
                Code::ValueRange,
                &place.key("argument"),
                format!(
                    "argument {}'s range reaches {end}, past the points field {} encodes it \
                     through; keep the range inside them, or let the conversion extrapolate",
                    argument.name, field.name
                ),
            );
            return false;
        }
    }
    // A table that inverts exactly is monotonic by construction.
    if let Some(explicit) = &field.inverse
        && !monotonic_over(
            explicit, field_min, field_max, field, argument, place, report,
        )
    {
        return false;
    }
    // The raw values the ends encode to, rounded as the encoder rounds
    // them, lie inside the conversion's points when it doesn't extrapolate,
    // so decoding can read them back; the inverse being monotonic,
    // everything between does too.
    if let Some((first, last)) = calibrated_span(conversion) {
        // A field has a size only in a binary payload.
        let binary = field.size_bits.is_some();
        for end in [min, max] {
            let Some(raw) = raw_unchecked(field, end, argument.unit.as_deref(), binary) else {
                return false;
            };
            if !(first <= raw && raw <= last) {
                report.error(
                    Code::ValueRange,
                    &place.key("argument"),
                    format!(
                        "argument {}'s range end {end} encodes to raw {raw}, outside field {}'s \
                         points ({first} to {last}), so decoding couldn't read it back; narrow \
                         the range, widen the points, or let the conversion extrapolate",
                        argument.name, field.name
                    ),
                );
                return false;
            }
        }
    }
    true
}

/// The raw values a table's or spline's points span, when it doesn't
/// extrapolate past them.
fn calibrated_span(conversion: &Conversion) -> Option<(f64, f64)> {
    let (Conversion::Table {
        points,
        extrapolate,
        ..
    }
    | Conversion::Spline {
        points,
        extrapolate,
    }) = conversion
    else {
        return None;
    };
    if *extrapolate == Some(true) {
        return None;
    }
    Some((points.first()?.raw, points.last()?.raw))
}

/// An explicit inverse only rises or only falls over `min` to `max`. A
/// linear one always does. A table, being straight or flat between its
/// points, is checked exactly: its value at `min`, at every point strictly
/// inside, and at `max`. A polynomial is checked at the 4097 points the
/// spec names.
fn monotonic_over(
    explicit: &Conversion,
    min: f64,
    max: f64,
    field: &Field,
    argument: &Argument,
    place: &Place,
    report: &mut Report,
) -> bool {
    let inputs: Vec<f64> = match explicit {
        Conversion::Linear { .. } => return true,
        Conversion::Table { points, .. } => std::iter::once(min)
            .chain(
                points
                    .iter()
                    .map(|point| point.raw)
                    .filter(|raw| min < *raw && *raw < max),
            )
            .chain(std::iter::once(max))
            .collect(),
        _ => (0..=INVERSE_SAMPLES)
            .map(|k| min + f64::from(k) * (max - min) / f64::from(INVERSE_SAMPLES))
            .collect(),
    };
    let mut previous: Option<f64> = None;
    let (mut rising, mut falling) = (false, false);
    for x in inputs {
        let Some(y) = convert(explicit, x).number() else {
            return false;
        };
        if let Some(previous) = previous {
            rising |= y > previous;
            falling |= y < previous;
        }
        previous = Some(y);
        if rising && falling {
            report.error(
                Code::ConversionType,
                &place.key("inverse"),
                format!(
                    "field {}'s inverse rises and falls over argument {}'s range (it turns near \
                     {x}), so a raw value can't be traced back; give an inverse that only rises \
                     or only falls from {min} to {max}",
                    field.name, argument.name
                ),
            );
            return false;
        }
    }
    true
}

/// Every value the catalog lets a caller give, through the field's
/// conversion, fits the field: checked for integer fields with a size, at
/// the range's ends through the encoder's inverse (which bound every value
/// in range: `check_inverse` makes sure the inverse is monotonic). An
/// argument without a range promises every value of its type.
fn check_coverage(field: &Field, argument: &Argument, place: &Place, report: &mut Report) {
    let Some((field_low, field_high)) =
        integer_range(field).filter(|_| field.ty != ScalarType::Bool)
    else {
        return;
    };
    let (type_low, type_high) = match argument.ty {
        ParamType::U64 => (0.0, u64::MAX as f64),
        ParamType::I64 => (i64::MIN as f64, i64::MAX as f64),
        ParamType::F64 => (f64::NEG_INFINITY, f64::INFINITY),
        _ => return,
    };
    let range = argument.range.unwrap_or_default();
    let low = range.min.unwrap_or(type_low);
    let high = range.max.unwrap_or(type_high);
    let (raw_low, raw_high) = match &field.conversion {
        None => (low, high),
        // Reported on its own: no inverse.
        Some(Conversion::Linear { scale, .. }) if *scale == 0.0 => return,
        Some(Conversion::States { .. }) => return,
        Some(_) => {
            let through = |x: f64| {
                convert_between(x, argument.unit.as_deref(), field.unit.as_deref())
                    .and_then(|x| inverse(field, x).number())
            };
            let (Some(a), Some(b)) = (through(low), through(high)) else {
                return;
            };
            (a.min(b), a.max(b))
        }
    };
    // The encoder rounds to the nearest integer, ties away from zero. Then
    // compare exactly: u64::MAX as f64 is 2^64, so comparing doubles would
    // let raw 2^64 through. An infinite end saturates past every field.
    let exact = |raw: f64| raw.is_finite().then(|| raw.round() as i128);
    let fits = exact(raw_low).is_some_and(|low| low >= field_low)
        && exact(raw_high).is_some_and(|high| high <= field_high);
    if fits {
        return;
    }
    let promise = match (range.min, range.max) {
        (None, None) => format!("has no range, so it can be any {}", type_label(argument.ty)),
        _ => format!("can be {low} to {high}"),
    };
    report.error(
        Code::ValueRange,
        &place.key("argument"),
        format!(
            "argument {} {promise}, but field {} holds raw {field_low} to {field_high}; give the \
             argument a range the field can hold",
            argument.name, field.name
        ),
    );
}

fn type_label(ty: ParamType) -> &'static str {
    match ty {
        ParamType::U64 => "u64",
        ParamType::I64 => "i64",
        _ => "number",
    }
}

fn requirement(ty: ParamType) -> &'static str {
    match ty {
        ParamType::F64 => "an f64: a number field, raw or through a numeric conversion",
        ParamType::I64 => "an i64: an int or uint field, unconverted",
        ParamType::U64 => "a u64: a uint field, unconverted",
        ParamType::Bool => "a bool: a bool or integer field, unconverted",
        ParamType::Str => "a str: a string field, unconverted",
        ParamType::Bytes => "bytes: a bytes field, unconverted",
        ParamType::Enum => "an enum: an integer field with a states conversion, or a string field",
        ParamType::Vec3 | ParamType::Quat => "a vector",
    }
}

/// Telemetry fields don't take command-only keys: a field in a packet or
/// struct type has no constant value, argument, inverse or target.
pub(super) fn check_telemetry_fields(entries: &[Entry], place: &Place, report: &mut Report) {
    for (index, entry) in entries.iter().enumerate() {
        let Entry::Field(field) = entry else {
            continue;
        };
        let field_place = place.item("entries", index, "field", &field.name);
        for (present, key) in [
            (field.value.is_some(), "value"),
            (field.argument.is_some(), "argument"),
            (field.inverse.is_some(), "inverse"),
            (field.target.is_some(), "target"),
        ] {
            if present {
                report.error(
                    Code::CommandOnly,
                    &field_place.key(key),
                    format!(
                        "{key} belongs to command packets; a telemetry field's value is read \
                         from the message"
                    ),
                );
            }
        }
    }
}
