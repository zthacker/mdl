//! Command packet checks: each encodes one catalog command, and every field
//! gets its value from exactly one place.

use std::collections::{BTreeMap, BTreeSet};

use super::mapping::check_unit;
use super::rules::{check_integer_spelling, check_scalar, integer_value, is_identifier};
use crate::v2::diagnostics::{Code, Place, Report};
use crate::v2::ir::{
    Argument, ByteLength, Catalog, CommandPacket, Conversion, Entry, Family, Field, ParamType,
    Scalar, ScalarType, Set,
};

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
            report.error(
                Code::DuplicateName,
                &packet_place,
                format!(
                    "command packet {} is already defined at commands/{first}",
                    packet.name
                ),
            );
        } else {
            names.insert(&packet.name, index);
        }
        check_metadata(packet, family, &packet_place, report);
        // The encoder looks a packet up by command, so there's one.
        if let Some(first) = commands.get(packet.canonical.as_str()) {
            report.error(
                Code::DuplicateMapping,
                &packet_place.key("canonical"),
                format!(
                    "command {} is already encoded by commands/{first}; a set encodes each \
                     command once",
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
            Entry::Padding(_) => continue,
            Entry::Array(array) => {
                let entry_place = place.item("entries", index, "array", &array.name);
                report.error(
                    Code::CommandShape,
                    &entry_place,
                    "a command packet holds fields and padding",
                );
                continue;
            }
            Entry::Struct(inner) => {
                let entry_place = place.item("entries", index, "struct", &inner.name);
                report.error(
                    Code::CommandShape,
                    &entry_place,
                    "a command packet holds fields and padding",
                );
                continue;
            }
        };
        let field_place = place.item("entries", index, "field", &field.name);
        check_telemetry_keys(field, &field_place, report);
        let sources = [
            field.value.is_some(),
            field.argument.is_some(),
            computed.contains(field.name.as_str()),
        ];
        match sources.iter().filter(|&&source| source).count() {
            0 => report.error(
                Code::NoValue,
                &field_place,
                format!(
                    "field {} gets no value: give it a value or an argument, or let a later \
                     entry's length come from it",
                    field.name
                ),
            ),
            1 => {}
            _ => report.error(
                Code::CommandShape,
                &field_place,
                format!(
                    "field {} gets its value from more than one place (a value, an argument, a \
                     later entry's length)",
                    field.name
                ),
            ),
        }
        if let Some(value) = &field.value {
            check_value(field, value, &field_place.key("value"), report);
        }
        if field.inverse.is_some() && !matches!(field.conversion, Some(Conversion::Poly { .. })) {
            report.error(
                Code::ConversionType,
                &field_place.key("inverse"),
                "an inverse is for a polynomial conversion; linear and states conversions invert \
                 exactly",
            );
        }
        if let Some(inverse) = &field.inverse
            && !matches!(inverse, Conversion::Linear { .. } | Conversion::Poly { .. })
        {
            report.error(
                Code::ConversionType,
                &field_place.key("inverse"),
                "an inverse is a linear or polynomial conversion, engineering value to raw",
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

/// A constant fits its field: an integer or bool field holds the integer,
/// a string field holds the text.
fn check_value(field: &Field, value: &Scalar, place: &Place, report: &mut Report) {
    match (field.ty, value) {
        (ScalarType::String, Scalar::Str(_)) => {}
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
    let poly = matches!(field.conversion, Some(Conversion::Poly { .. }));
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
    if poly && field.inverse.is_none() {
        report.error(
            Code::ConversionType,
            &place.key("conversion"),
            "a polynomial conversion on an argument needs an inverse (engineering value to raw)",
        );
    }
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
    check_coverage(field, argument, place, report);
    if matches!(
        argument.ty,
        ParamType::F64 | ParamType::I64 | ParamType::U64
    ) {
        check_unit(
            field.unit.as_deref(),
            argument.unit.as_deref(),
            &argument.name,
            &place.key("unit"),
            report,
        );
    }
}

/// Every value the catalog lets a caller give, through the field's
/// conversion, fits the field: checked for integer fields with a size,
/// unconverted or through a linear conversion (a polynomial's inverse is
/// checked per message). An argument without a range promises every value
/// of its type.
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
    let (raw_low, raw_high) = match field.conversion {
        None => (low, high),
        Some(Conversion::Linear { scale, offset }) if scale != 0.0 => {
            let ends = ((low - offset) / scale, (high - offset) / scale);
            (ends.0.min(ends.1), ends.0.max(ends.1))
        }
        _ => return,
    };
    // The encoder rounds to the nearest integer, ties away from zero.
    let (raw_low, raw_high) = (raw_low.round(), raw_high.round());
    if raw_low >= field_low as f64 && raw_high <= field_high as f64 {
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
        ParamType::F64 => "an f64: a number field, raw or through linear or poly",
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
/// struct type has no constant value, argument or inverse.
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
