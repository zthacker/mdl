//! Command packet checks: each encodes one catalog command, every field
//! gets its value from exactly one place, and a set's command packets can
//! be told apart.

use std::collections::{BTreeMap, BTreeSet};

use super::catalog::{check_argument, check_argument_default, check_range};
use super::layout::static_offsets;
use super::mapping::check_unit;
use super::rules::{
    check_integer_spelling, check_scalar, check_unit_code, did_you_mean, integer_value,
    is_identifier, pointer_tokens,
};
use super::set::{KeyId, KeyValue, by_value, exact, key_order, used_types};
use crate::bits::IntForm;
use crate::canonical::format_double;
use crate::convert::{Converted, convert, inverse, inverts_exactly};
use crate::diagnostics::{Code, Place, Report};
use crate::encode::{
    FieldProblem, encoded_arguments, raw_unchecked, show_end, show_literal, write_problem,
};
use crate::ir::{
    Argument, ArgumentRange, ArrayEntry, ByteLength, Catalog, Command, CommandPacket, Conversion,
    Count, Element, Encoding, Entry, Family, Field, Literal, NarrowError, PacketArgument,
    ParamType, RangeEnds, Scalar, ScalarElement, ScalarType, Set, StructType,
};
use crate::text;
use crate::units::convert_between;

/// Each command packet's arguments as it takes them, in the set's order:
/// its catalog command's, each narrowed by the packet's own `arguments`
/// (IR 2.2, [`Argument::narrowed`]) where they're valid. `None` for a
/// packet whose command the catalog lacks, which [`check_packets`]
/// reports.
pub(super) fn narrowed_arguments(
    set: &Set,
    catalog: &Catalog,
    place: &Place,
    report: &mut Report,
) -> Vec<PacketArguments> {
    set.commands
        .iter()
        .enumerate()
        .map(|(index, packet)| {
            let mut unsettled = BTreeSet::new();
            let mut unsettled_states = BTreeSet::new();
            let Some(command) = catalog.commands.get(&packet.canonical) else {
                return PacketArguments {
                    arguments: None,
                    unsettled,
                    unsettled_states,
                };
            };
            let packet_place = place.item("commands", index, "command", &packet.name);
            let encoded = encoded_arguments(packet, &set.types);
            let mut arguments = command.arguments.clone();
            for (name, limits) in &packet.arguments {
                let at = packet_place.map_item("arguments", name, "argument");
                let Some(argument) = arguments.iter_mut().find(|a| &a.name == name) else {
                    unknown_argument(command, packet, name, &at, report);
                    continue;
                };
                let encoded = encoded.contains(name.as_str());
                match check_packet_argument(argument, limits, encoded, &at, report) {
                    Limited::Narrowed(narrowed) => *argument = *narrowed,
                    Limited::AsCataloged => {}
                    Limited::Unsettled {
                        states,
                        states_sound,
                    } => {
                        if let Some(narrowed) = states {
                            *argument = *narrowed;
                        }
                        unsettled.insert(name.clone());
                        if !states_sound {
                            unsettled_states.insert(name.clone());
                        }
                    }
                }
                // A packet that requires an argument it encodes (IR 2.3)
                // sends no default, whatever else its limits get wrong:
                // where they have problems, a required list's catalog
                // default isn't held to one length with the lists it
                // shares a count with. One it doesn't encode is reported,
                // and keeps the default the set would send.
                if encoded && limits.required == Some(true) && limits.default.is_none() {
                    argument.default = None;
                }
            }
            PacketArguments {
                arguments: Some(arguments),
                unsettled,
                unsettled_states,
            }
        })
        .collect()
}

/// A command packet's arguments as it takes them ([`narrowed_arguments`]).
pub(super) struct PacketArguments {
    /// Its command's, each narrowed by the packet's limits; `None` when
    /// the catalog lacks the command.
    arguments: Option<Vec<Argument>>,
    /// Those whose limits here have problems, reported: the fields that
    /// encode them aren't checked against the catalog's range, which would
    /// only echo them.
    unsettled: BTreeSet<String>,
    /// Those whose states here don't say which they are (reported): their
    /// fields aren't checked for a raw value for each state it accepts. A
    /// field is checked for every other argument, whatever else its limits
    /// get wrong.
    unsettled_states: BTreeSet<String>,
}

/// What a packet's limits make of one of its arguments.
enum Limited {
    Narrowed(Box<Argument>),
    /// The argument as the catalog has it: its problems, reported there,
    /// leave nothing to narrow.
    AsCataloged,
    /// The limits have problems, reported here. `states` is the argument
    /// with only the states it accepts narrowed, when it lists sound ones;
    /// `states_sound` is false when its list doesn't say which they are.
    Unsettled {
        states: Option<Box<Argument>>,
        states_sound: bool,
    },
}

/// Limits with problems: the argument with the states the packet accepts,
/// when those are sound, so its field is still checked for a raw value for
/// each.
fn unsettled(argument: &Argument, limits: &PacketArgument, states_sound: bool) -> Limited {
    let states = match (&limits.states, states_sound) {
        (Some(_), true) => states_only(argument, limits).map(Box::new),
        _ => None,
    };
    Limited::Unsettled {
        states,
        states_sound,
    }
}

/// `argument` with only the states `limits` accepts narrowed (IR 2.3): its
/// range and default as the catalog has them.
fn states_only(argument: &Argument, limits: &PacketArgument) -> Option<Argument> {
    let mut states = PacketArgument::new();
    states.states = limits.states.clone();
    argument.narrowed(&states).ok()
}

fn unknown_argument(
    command: &Command,
    packet: &CommandPacket,
    name: &str,
    place: &Place,
    report: &mut Report,
) {
    let close = did_you_mean(name, command.arguments.iter().map(|a| &a.name));
    report.error(
        Code::UnknownArgument,
        place,
        format!("command {} has no argument {name}{close}", packet.canonical),
    );
}

/// What a packet says of one of its command's arguments, in its own terms
/// (its unit, its range, its default), then against the catalog's: the
/// argument narrowed, when it's all valid.
fn check_packet_argument(
    argument: &Argument,
    limits: &PacketArgument,
    encoded: bool,
    place: &Place,
    report: &mut Report,
) -> Limited {
    let name = &argument.name;
    let mut found = Report::default();
    if !encoded {
        found.error(
            Code::UnmappedArgument,
            place,
            format!(
                "no field or array of this packet encodes argument {name}, so it has nothing \
                 here to limit"
            ),
        );
    }
    let values = limits.range.is_some() || limits.default.is_some();
    let empty = !values && limits.states.is_none() && limits.required.is_none();
    if empty {
        found.error(
            Code::Empty,
            place,
            "a packet's argument gives a range, a default, the states it accepts or required; \
             omit it otherwise",
        );
    }
    match (limits.required, &limits.default) {
        (Some(false), _) => found.error(
            Code::ExplicitDefault,
            &place.key("required"),
            "required: false is the default; omit it",
        ),
        (Some(true), Some(_)) => found.error(
            Code::RequiredDefault,
            &place.key("default"),
            format!(
                "argument {name} is required here, so this default would never be sent; give a \
                 default or require the argument, not both"
            ),
        ),
        _ => {}
    }
    if let Some(states) = &limits.states {
        check_packet_states(argument, states, place, &mut found);
    }
    // The states it accepts are known unless its list is empty, names a
    // state the catalog lacks, or is for an argument that isn't an enum: a
    // list out of order or with a repeat still says which they are.
    let states_sound = limits.states.is_none() || states_only(argument, limits).is_some();
    if let Some(unit) = &limits.unit {
        let at = place.key("unit");
        if values {
            check_unit_code(unit, &at, &mut found);
            let argument_name = format!("argument {name}");
            let converts = argument.ty == ParamType::F64;
            check_unit(
                Some(unit),
                argument.unit.as_deref(),
                &argument_name,
                converts,
                &at,
                &mut found,
            );
        } else if !empty {
            // States and required have no unit, so it's only checked for
            // being here at all: the argument's own is its default written
            // out, and any other has nothing to apply to.
            let code = if argument.unit.as_deref() == Some(unit.as_str()) {
                Code::ExplicitDefault
            } else {
                Code::CommandShape
            };
            found.error(
                code,
                &at,
                "a packet's unit is the one its range and default are in, and this argument \
                 gives neither; omit unit",
            );
        }
    }
    let mut own = argument.clone();
    own.range = limits.range.clone();
    own.default = limits.default.clone();
    if let Some(range) = &limits.range {
        check_range(&own, range, &place.key("range"), &mut found);
    }
    check_argument_default(&own, place, &mut found);
    let clean = found.is_empty();
    report.absorb(found);
    if !clean {
        return unsettled(argument, limits, states_sound);
    }
    // An argument with problems of its own, reported on the catalog, isn't
    // one to compare with. The states the packet accepts, checked above
    // against the catalog's, still say which its field must send.
    let mut catalog = Report::default();
    check_argument(argument, place, &mut catalog);
    if !catalog.is_empty() {
        return match (&limits.states, states_only(argument, limits)) {
            (Some(_), Some(narrowed)) => Limited::Narrowed(Box::new(narrowed)),
            _ => Limited::AsCataloged,
        };
    }
    let unit = argument.unit.as_deref().unwrap_or_default();
    let (code, key, message) = match argument.narrowed(limits) {
        Ok(narrowed) => return check_narrowed_default(argument, narrowed, limits, place, report),
        Err(NarrowError::Disjoint) => (
            Code::ValueRange,
            "range",
            format!(
                "argument {name} can be {} in the catalog and {} here, so no value is both",
                range_text(argument.range.as_ref()),
                range_text(limits.range.as_ref())
            ),
        ),
        Err(NarrowError::EndNotFinite) => (
            Code::ValueRange,
            "range",
            format!("in argument {name}'s unit, {unit:?}, an end here is past what a double holds"),
        ),
        Err(NarrowError::DefaultNotFinite) => (
            Code::DefaultValue,
            "default",
            format!(
                "in argument {name}'s unit, {unit:?}, the default here is past what a double \
                 holds"
            ),
        ),
        // The checks above refuse the rest first.
        Err(problem) => (
            Code::ValueRange,
            "range",
            format!("these limits can't narrow argument {name}: {problem}"),
        ),
    };
    report.error(code, &place.key(key), message);
    unsettled(argument, limits, true)
}

/// The states a packet accepts (IR 2.3): an enum argument's, at least
/// one, each once, in the catalog's order. The list means a set, so one
/// order keeps one set one digest.
fn check_packet_states(argument: &Argument, states: &[String], place: &Place, report: &mut Report) {
    let name = &argument.name;
    let at = place.key("states");
    if argument.ty != ParamType::Enum {
        report.error(
            Code::ParamShape,
            &at,
            format!("only an enum argument has states, and argument {name} isn't one"),
        );
        return;
    }
    if states.is_empty() {
        report.error(
            Code::Empty,
            &at,
            "a packet's states list at least one state; leave states out to accept every state \
             the catalog lists",
        );
        return;
    }
    // An enum the catalog lists no states for is reported there; each of
    // these would only echo it.
    if argument.states.is_empty() {
        return;
    }
    let mut seen = BTreeSet::new();
    // The furthest catalog position listed so far.
    let mut furthest: Option<usize> = None;
    for (index, state) in states.iter().enumerate() {
        let state_place = place.at("states", index);
        let Some(position) = argument.states.iter().position(|known| known == state) else {
            let close = did_you_mean(state, argument.states.iter());
            report.error(
                Code::UnknownState,
                &state_place,
                format!("argument {name} has no state {state:?}{close}"),
            );
            continue;
        };
        if !seen.insert(state) {
            report.error(
                Code::DuplicateName,
                &state_place,
                format!("state {state:?} is listed twice"),
            );
            continue;
        }
        if let Some(furthest) = furthest
            && position < furthest
            && let Some(later) = argument.states.get(furthest)
        {
            report.error(
                Code::NotSorted,
                &state_place,
                format!(
                    "states are listed in the catalog's order, where {state:?} comes before \
                     {later:?}, so one set of states has one spelling"
                ),
            );
        }
        furthest = furthest.max(Some(position));
    }
}

/// What a packet's limits leave of `argument`, `narrowed`, if the default
/// it sends when a request leaves the argument out is one both ranges
/// allow, and one of the states it accepts.
fn check_narrowed_default(
    argument: &Argument,
    narrowed: Argument,
    limits: &PacketArgument,
    place: &Place,
    report: &mut Report,
) -> Limited {
    let name = &argument.name;
    if let Some(state) = unaccepted_default(&narrowed) {
        let (at, message) = match &limits.default {
            Some(_) => (
                place.key("default"),
                format!("the default here, {state:?}, isn't one of the states this packet accepts"),
            ),
            None => (
                place.key("states"),
                format!(
                    "argument {name}'s default in the catalog, {state:?}, isn't one of the states \
                     here; give a default here"
                ),
            ),
        };
        report.error(Code::DefaultValue, &at, message);
        return unsettled(argument, limits, true);
    }
    let mut defaults = Report::default();
    check_argument_default(&narrowed, place, &mut defaults);
    if !defaults.is_empty() {
        let (at, message) = match &limits.default {
            Some(_) => (
                place.key("default"),
                format!(
                    "the default here is outside argument {name}'s range in the catalog, {}",
                    range_text(argument.range.as_ref())
                ),
            ),
            None => (
                place.key("range"),
                format!(
                    "argument {name}'s default in the catalog is outside the range here, {}; \
                     give a default here",
                    range_text(limits.range.as_ref())
                ),
            ),
        };
        report.error(Code::DefaultValue, &at, message);
        return unsettled(argument, limits, true);
    }
    Limited::Narrowed(Box::new(narrowed))
}

/// The first state of a narrowed enum argument's default, a single value
/// or a list's, that isn't one of the states it's narrowed to. Its own and
/// the catalog's defaults are catalog states by then.
fn unaccepted_default(narrowed: &Argument) -> Option<&str> {
    if narrowed.ty != ParamType::Enum {
        return None;
    }
    let values = match narrowed.default.as_ref()? {
        Literal::List(values) => values.as_slice(),
        value => std::slice::from_ref(value),
    };
    values.iter().find_map(|value| match value {
        Literal::Str(state) if !narrowed.states.contains(state) => Some(state.as_str()),
        _ => None,
    })
}

/// A range in words: `0 to 10`, `at least 0`, `any value`.
fn range_text(range: Option<&ArgumentRange>) -> String {
    let (min, max) = range.map_or((None, None), |range| {
        (range.min.as_ref(), range.max.as_ref())
    });
    match (min, max) {
        (Some(min), Some(max)) => format!("{} to {}", show_end(min), show_end(max)),
        (Some(min), None) => format!("at least {}", show_end(min)),
        (None, Some(max)) => format!("at most {}", show_end(max)),
        (None, None) => "any value".to_string(),
    }
}

/// Every command packet of a set: names, the command it encodes, link
/// metadata, and its fields, against its arguments as
/// [`narrowed_arguments`] gives them. Positions and references are checked
/// with the set's layout, and what each field writes with
/// [`check_writes`], which takes the writes this returns.
pub(super) fn check_packets<'a>(
    set: &'a Set,
    family: Option<&Family>,
    catalog: &'a Catalog,
    narrowed: &'a [PacketArguments],
    place: &'a Place,
    report: &mut Report,
) -> Vec<Write<'a>> {
    let mut writes = Vec::new();
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
            let close = report.suggestion("command", &packet.canonical, || {
                did_you_mean(&packet.canonical, catalog.commands.keys())
            });
            report.error(
                Code::UnknownCommand,
                &packet_place.key("canonical"),
                format!("the catalog has no command {}{close}", packet.canonical),
            );
        }
        let taken = narrowed.get(index);
        let mut sources = Sources {
            packet,
            command,
            arguments: taken.and_then(|taken| taken.arguments.as_deref()),
            unsettled: taken.map(|taken| &taken.unsettled),
            unsettled_states: taken.map(|taken| &taken.unsettled_states),
            types: &set.types,
            set_place: place,
            computed: computed_fields(packet, &packet_place, report),
            encoded: BTreeMap::new(),
            writes: Vec::new(),
        };
        sources.check_entries(&packet_place, report);
        writes.append(&mut sources.writes);
    }
    writes
}

/// A field, or an array's element taken as a field, that writes an
/// argument's values in one command packet, and whose checks against the
/// argument found nothing.
pub(super) struct Write<'a> {
    packet: &'a CommandPacket,
    /// Owned, since an element is taken as a field.
    field: Field,
    argument: &'a Argument,
    /// How messages name it: `field x`, `array x's elements`.
    what: String,
    /// Where it names the argument.
    at: Place,
    /// Where its own keys are.
    own: Place,
}

/// The keys of a field, or an element, that decide whether it can write a
/// value.
const WRITE_KEYS: [&str; 7] = [
    "type",
    "size_bits",
    "length",
    "text",
    "conversion",
    "inverse",
    "unit",
];

/// What the catalog gives each write, written as encoding would write it:
/// the name of every state of an enum argument a string field writes, then
/// the argument's default. Runs after every other check of the set, so a
/// write whose field has a problem of its own, which would only echo here,
/// is left out.
pub(super) fn check_writes(writes: &[Write<'_>], family: &Family, report: &mut Report) {
    let binary = family.encoding == Encoding::Binary;
    for write in writes {
        let echoes = WRITE_KEYS
            .iter()
            .any(|key| report.reported_under(&write.own.key(key)));
        // A default that is a state already reported is the same problem.
        if !echoes && write.check_states(binary, report) {
            write.check_default(binary, report);
        }
    }
}

impl Write<'_> {
    /// Each state of an enum argument that a string field writes is text
    /// the field can hold, so every state the catalog lists can be sent.
    /// Whether they all are.
    fn check_states(&self, binary: bool, report: &mut Report) -> bool {
        if self.argument.ty != ParamType::Enum || self.field.ty != ScalarType::String {
            return true;
        }
        let mut written = true;
        for state in &self.argument.states {
            let Some(problem) = self.problem(&Literal::Str(state.clone()), binary) else {
                continue;
            };
            written = false;
            report.error(
                Code::ValueRange,
                &self.at,
                format!(
                    "argument {}'s state {state:?} can't be written by {}, so it can't be sent: \
                     {problem}",
                    self.argument.name, self.what
                ),
            );
        }
        written
    }

    /// An argument's default encodes where the packet writes it: a single
    /// value through its field, each value of a list through the array's
    /// element or record field. Otherwise a request that leaves it out
    /// could never be sent with this set.
    fn check_default(&self, binary: bool, report: &mut Report) {
        let argument = self.argument;
        // A default of the wrong shape is reported on the catalog.
        let values: Vec<(Option<usize>, &Literal)> = match (&argument.default, argument.list) {
            (Some(Literal::List(items)), Some(_)) => items
                .iter()
                .enumerate()
                .map(|(index, item)| (Some(index), item))
                .collect(),
            (Some(Literal::List(_)), None) | (Some(_), Some(_)) | (None, _) => return,
            (Some(value), None) => vec![(None, value)],
        };
        for (index, value) in values {
            let Some(problem) = self.problem(value, binary) else {
                continue;
            };
            let which = match index {
                Some(index) => format!("value {index} of argument {}'s default", argument.name),
                None => format!("argument {}'s default", argument.name),
            };
            report.error(
                Code::DefaultValue,
                &self.at,
                format!(
                    "{which}, {}, can't be written by {}, so a request that leaves {} out can't \
                     be sent: {problem}",
                    show_literal(value),
                    self.what,
                    argument.name
                ),
            );
        }
    }

    /// Why the field can't write `value`, as encoding would find it; `None`
    /// when it can, or for a problem with the layout, which is reported
    /// with the layout.
    fn problem(&self, value: &Literal, binary: bool) -> Option<FieldProblem> {
        write_problem(self.packet, &self.field, self.argument, value, binary)
            .filter(|problem| !matches!(problem, FieldProblem::Layout(_)))
    }
}

/// The keys that identify each command packet of a set, by value: its
/// metadata, and its constant fields that a decoder can read without
/// decoding anything else. In a binary family those are integer and bool
/// fields and fixed-length strings at static positions; in a JSON family,
/// integer and string fields. Decoding a command reads exactly these.
pub(crate) fn command_identities(
    set: &Set,
    family: &Family,
    type_sizes: &BTreeMap<&str, u64>,
) -> Vec<BTreeMap<KeyId, KeyValue>> {
    set.commands
        .iter()
        .map(|packet| {
            let mut keys: BTreeMap<KeyId, KeyValue> = packet
                .metadata
                .iter()
                .map(|(name, value)| (KeyId::Metadata(name.clone()), by_value(value)))
                .collect();
            let offsets = match family.encoding {
                Encoding::Binary => static_offsets(&packet.entries, type_sizes),
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
                        let offset = offsets.get(index).copied().flatten();
                        binary_key(field, family, value, offset)
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
/// fixed-length string as its bytes. In a terminated buffer only the value
/// and the terminator after it are certain (the padding can be anything),
/// so those are the key: `"PING"` in a `char[8]` is `PING\0`.
fn binary_key(
    field: &Field,
    family: &Family,
    value: &Scalar,
    offset: Option<u64>,
) -> Option<(KeyId, KeyValue)> {
    let offset = offset?;
    if let (ScalarType::String, Scalar::Str(text), Some(ByteLength::Fixed { bytes, terminator })) =
        (field.ty, value, &field.length)
    {
        let mut certain = text::encode(text, field.text)?;
        let size = *bytes as usize;
        // A value that doesn't fit is reported by check_text_value.
        let fits = match terminator {
            None => certain.len() == size,
            Some(byte) => certain.len() <= size && !certain.contains(byte),
        };
        if !fits || !offset.is_multiple_of(8) {
            return None;
        }
        if let Some(byte) = terminator
            && certain.len() < size
        {
            certain.push(*byte);
        }
        let key = KeyId::Bytes {
            offset,
            length: u32::try_from(certain.len()).ok()?,
        };
        return Some((key, KeyValue::Bytes(certain)));
    }
    let size = field.size_bits.filter(|size| (1..=64).contains(size))?;
    if !matches!(
        field.ty,
        ScalarType::Uint | ScalarType::Int | ScalarType::Bool
    ) {
        return None;
    }
    // The bits the constant writes, compared as bits; values out of range
    // are reported by check_value, which leaves the packet out of
    // check_distinct.
    let form = IntForm::of(field.ty, field.signed)?;
    let raw = form.write(integer_value(value)?, size)?;
    Some((
        KeyId::Bits {
            offset,
            size,
            form: IntForm::Unsigned,
            order: key_order(field, family, offset, size),
        },
        KeyValue::Int(i128::from(raw)),
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
pub(super) fn check_distinct(
    set: &Set,
    family: &Family,
    type_sizes: &BTreeMap<&str, u64>,
    place: &Place,
    report: &mut Report,
) {
    let identities = command_identities(set, family, type_sizes);
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

/// Fields whose value comes from the data: each gives one later field's
/// length, or the count of later arrays (several, which then share it).
fn computed_fields<'p>(
    packet: &'p CommandPacket,
    place: &Place,
    report: &mut Report,
) -> BTreeSet<&'p str> {
    let mut lengths: BTreeSet<&str> = BTreeSet::new();
    let mut counts: BTreeSet<&str> = BTreeSet::new();
    for (index, entry) in packet.entries.iter().enumerate() {
        let (from, at, is_length) = match entry {
            Entry::Field(Field {
                name,
                length: Some(ByteLength::Field(from)),
                ..
            }) => (
                from,
                place.item("entries", index, "field", name).key("length"),
                true,
            ),
            Entry::Array(ArrayEntry {
                name,
                count: Some(Count::Field(from)),
                ..
            }) => (
                from,
                place.item("entries", index, "array", name).key("count"),
                false,
            ),
            _ => continue,
        };
        let field = from.field.as_str();
        let problem = match (is_length, lengths.contains(field), counts.contains(field)) {
            (true, true, _) => Some(format!(
                "another field already takes its length from {field}; in a command packet a \
                 length field gives the length of one field"
            )),
            (true, _, true) | (false, true, _) => Some(format!(
                "{field} gives {}; in a command packet a field gives one length, or the count \
                 of arrays",
                if is_length {
                    "the count of an array"
                } else {
                    "a length"
                }
            )),
            _ => None,
        };
        match problem {
            Some(problem) => report.error(Code::CommandShape, &at.key("field"), problem),
            None if is_length => {
                lengths.insert(field);
            }
            None => {
                counts.insert(field);
            }
        }
    }
    lengths.union(&counts).copied().collect()
}

/// Where a command packet's values come from, as its entries are checked.
struct Sources<'a> {
    packet: &'a CommandPacket,
    /// `None` when the command is unknown (reported already).
    command: Option<&'a Command>,
    /// The command's arguments as this packet takes them; `None` when the
    /// command is unknown.
    arguments: Option<&'a [Argument]>,
    /// Arguments whose limits in this packet have problems (reported
    /// already), whose fields aren't checked against their ranges.
    unsettled: Option<&'a BTreeSet<String>>,
    /// Arguments whose states in this packet have problems (reported
    /// already), whose fields aren't checked for a raw value for each.
    unsettled_states: Option<&'a BTreeSet<String>>,
    types: &'a BTreeMap<String, StructType>,
    set_place: &'a Place,
    /// Fields whose value comes from the data: a length or a count.
    computed: BTreeSet<&'a str>,
    /// Each argument encoded so far, and what encodes it.
    encoded: BTreeMap<&'a str, String>,
    /// What [`check_writes`] checks once the set's other checks have run.
    writes: Vec<Write<'a>>,
}

/// Where a field is: at the top of the packet, or in the records of an
/// array.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Top,
    Record,
}

impl<'a> Sources<'a> {
    /// What the packet holds, and where each value comes from.
    fn check_entries(&mut self, place: &Place, report: &mut Report) {
        let packet = self.packet;
        let mut target: Option<usize> = None;
        for (index, entry) in packet.entries.iter().enumerate() {
            check_written(entry, &entry_place(place, index, entry), report);
            match entry {
                Entry::Padding(_) | Entry::Checksum(_) => {}
                Entry::Struct(inner) => report.error(
                    Code::CommandShape,
                    &place.item("entries", index, "struct", &inner.name),
                    "a command packet holds fields, arrays, padding and checksums",
                ),
                Entry::Array(array) => {
                    let array_place = place.item("entries", index, "array", &array.name);
                    self.check_array(array, &array_place, report);
                }
                Entry::Field(field) => {
                    let field_place = place.item("entries", index, "field", &field.name);
                    let at = format!("entries/{index}");
                    self.check_field(field, Level::Top, at, false, &field_place, report);
                    // One target field, so a payload can't hold two ids that
                    // disagree.
                    if field.target == Some(true) {
                        match target {
                            Some(first) => report.error(
                                Code::CommandShape,
                                &field_place.key("target"),
                                format!(
                                    "entries/{first} already holds the target's native id; a \
                                     command packet holds it once"
                                ),
                            ),
                            None => target = Some(index),
                        }
                    }
                }
            }
        }
        check_pointers(&packet.entries, place, report);
        // A set may leave out an argument with a default (an older layout,
        // from before the argument was added); the encoder then accepts only
        // the default for it.
        for argument in self.arguments.into_iter().flatten() {
            if !self.encoded.contains_key(argument.name.as_str()) && argument.default.is_none() {
                report.error(
                    Code::UnmappedArgument,
                    &place.key("canonical"),
                    format!(
                        "no field or array encodes argument {} of command {}",
                        argument.name, packet.canonical
                    ),
                );
            }
        }
        self.check_default_lengths(place, report);
    }

    /// Lists that must have one length have defaults that do: those that
    /// fill one array's records, and those of arrays that share a count
    /// field, whose defaults give it one count, each a count it can hold
    /// (`(length - add) / multiply`, whole and not negative). So the
    /// defaults of a command's lists encode together.
    fn check_default_lengths(&self, place: &Place, report: &mut Report) {
        let default_length = |name: &str| {
            self.arguments
                .into_iter()
                .flatten()
                .find(|argument| argument.name == name)
                .and_then(|argument| match &argument.default {
                    Some(Literal::List(values)) => Some(values.len()),
                    _ => None,
                })
        };
        // Each count field: the count the first defaulted list gives it, and
        // that list.
        let mut counts: BTreeMap<&str, (i128, &str)> = BTreeMap::new();
        for (index, entry) in self.packet.entries.iter().enumerate() {
            let Entry::Array(array) = entry else {
                continue;
            };
            let at = place.item("entries", index, "array", &array.name);
            let lists: Vec<&str> = match &array.element {
                Element::Scalar(_) => array.argument.as_deref().into_iter().collect(),
                Element::Struct { name } => self
                    .types
                    .get(name)
                    .map(|record| {
                        record
                            .entries
                            .iter()
                            .filter_map(|entry| match entry {
                                Entry::Field(field) => field.argument.as_deref(),
                                _ => None,
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            };
            let defaulted: Vec<(&str, usize)> = lists
                .into_iter()
                .filter_map(|name| default_length(name).map(|length| (name, length)))
                .collect();
            let Some(&(first, length)) = defaulted.first() else {
                continue;
            };
            if let Some((other, other_length)) = defaulted.iter().find(|(_, l)| *l != length) {
                report.error(
                    Code::DefaultValue,
                    &at,
                    format!(
                        "the defaults of lists {first} and {other}, which fill array {}'s \
                         records, have {length} and {other_length} values; lists that fill the \
                         same records have the same length",
                        array.name
                    ),
                );
                continue;
            }
            let Some(Count::Field(from)) = &array.count else {
                continue;
            };
            let multiply = i128::from(from.multiply.unwrap_or(1));
            let add = i128::from(from.add.unwrap_or(0));
            let held = i128::try_from(length).unwrap_or(i128::MAX) - add;
            if held < 0 || held % multiply != 0 {
                report.error(
                    Code::DefaultValue,
                    &at,
                    format!(
                        "list {first}'s default has {length} values, which {} can't count: it \
                         holds (count - {add}) / {multiply}",
                        from.field
                    ),
                );
                continue;
            }
            let count = held / multiply;
            match counts.get(from.field.as_str()) {
                Some(&(earlier, other)) if earlier != count => report.error(
                    Code::DefaultValue,
                    &at,
                    format!(
                        "{} counts the arrays of lists {other} and {first}, whose defaults give \
                         it {earlier} and {count}; arrays that share a count hold as many values",
                        from.field
                    ),
                ),
                Some(_) => {}
                None => {
                    counts.insert(&from.field, (count, first));
                }
            }
        }
    }

    /// A field's one source of value, and the argument it encodes. `at`
    /// names it for a later field that encodes the same argument;
    /// `measured` says another field of its record takes a length from it,
    /// which is refused there.
    fn check_field(
        &mut self,
        field: &'a Field,
        level: Level,
        at: String,
        measured: bool,
        place: &Place,
        report: &mut Report,
    ) {
        check_telemetry_keys(field, place, report);
        if level == Level::Record {
            if field.target.is_some() {
                report.error(
                    Code::CommandShape,
                    &place.key("target"),
                    "the target's native id is written once, at the top of a command packet, \
                     not in each record of an array",
                );
            }
            if let Some(ByteLength::Field(_)) = field.length {
                report.error(
                    Code::CommandShape,
                    &place.key("length"),
                    "a field of a command record doesn't take its length from another field",
                );
            }
        }
        let computed = level == Level::Top && self.computed.contains(field.name.as_str());
        // A record's target, or a length taken from the field, is reported
        // already; it isn't echoed as no value.
        let echo = level == Level::Record && (field.target.is_some() || measured);
        let sources = [
            field.value.is_some(),
            field.argument.is_some(),
            field.target == Some(true) && level == Level::Top,
            computed,
        ];
        match sources.iter().filter(|&&source| source).count() {
            0 if !echo => report.error(
                Code::NoValue,
                place,
                match level {
                    Level::Top => format!(
                        "field {} gets no value: give it a value, an argument or the target, or \
                         let a later entry's length or count come from it",
                        field.name
                    ),
                    Level::Record => format!(
                        "field {} gets no value: give it a value or a list argument",
                        field.name
                    ),
                },
            ),
            0 | 1 => {}
            _ => report.error(
                Code::CommandShape,
                place,
                format!(
                    "field {} gets its value from more than one place (a value, an argument, \
                     the target, a later entry's length or count)",
                    field.name
                ),
            ),
        }
        if let Some(value) = &field.value {
            check_value(field, value, &place.key("value"), report);
        }
        if let (Some(is_target), Level::Top) = (field.target, level) {
            check_target(field, is_target, place, report);
        }
        check_inverse_keys(field, place, report);
        if field.argument.is_none() && field.inverse.is_some() {
            report.error(
                Code::ConversionType,
                &place.key("inverse"),
                "an inverse converts an argument, and this field encodes none",
            );
        }
        if let Some(name) = &field.argument {
            let encoder = Encoder {
                field,
                what: format!("field {}", field.name),
                argument: place.key("argument"),
                own: place.clone(),
            };
            self.check_encoded(name, &encoder, level == Level::Record, at, report);
        }
    }

    /// An array of a list argument's values, or of records whose fields
    /// each take a list argument's values.
    fn check_array(&mut self, array: &'a ArrayEntry, place: &Place, report: &mut Report) {
        for (present, key, why) in [
            (
                array.canonical.is_some(),
                "canonical",
                "canonical maps telemetry to a param; a command array encodes a list argument",
            ),
            (
                array.sample_interval_ms.is_some(),
                "sample_interval_ms",
                "sample_interval_ms spaces out telemetry samples; a command array has none",
            ),
            (
                array.optional.is_some(),
                "optional",
                "optional is for telemetry members that may be missing; a command array is \
                 always written",
            ),
        ] {
            if present {
                report.error(Code::CommandShape, &place.key(key), why);
            }
        }
        match &array.element {
            Element::Scalar(element) => {
                let element_place = place.key("element");
                if !element.invalid.is_empty() {
                    report.error(
                        Code::CommandShape,
                        &element_place.key("invalid"),
                        "sentinels mark telemetry that has no reading; a command element always \
                         has a value",
                    );
                }
                let field = element.as_field(&array.name);
                check_inverse_keys(&field, &element_place, report);
                let Some(name) = &array.argument else {
                    report.error(
                        Code::NoValue,
                        place,
                        format!(
                            "array {} gets no values: give it a list argument",
                            array.name
                        ),
                    );
                    return;
                };
                let encoder = Encoder {
                    field: &field,
                    what: format!("array {}'s elements", array.name),
                    argument: place.key("argument"),
                    own: element_place,
                };
                self.check_encoded(
                    name,
                    &encoder,
                    true,
                    format!("array {}", array.name),
                    report,
                );
                self.check_list_fits(array, name, place, report);
            }
            Element::Struct { name } => {
                if array.argument.is_some() {
                    report.error(
                        Code::CommandShape,
                        &place.key("argument"),
                        format!(
                            "array {}'s elements are records, whose fields name their own list \
                             arguments; the array names none",
                            array.name
                        ),
                    );
                }
                // An unknown type is reported with the set's references.
                let Some(record) = self.types.get(name) else {
                    return;
                };
                let takes_list = record
                    .entries
                    .iter()
                    .any(|entry| matches!(entry, Entry::Field(field) if field.argument.is_some()));
                if !takes_list {
                    report.error(
                        Code::NoValue,
                        place,
                        format!(
                            "array {} gets no values: a field of its records takes a list \
                             argument, whose length is the number of records",
                            array.name
                        ),
                    );
                }
                let type_place = self.set_place.map_item("types", name, "type");
                let measured: BTreeSet<&str> = record
                    .entries
                    .iter()
                    .filter_map(|entry| match entry {
                        Entry::Field(Field {
                            length: Some(ByteLength::Field(from)),
                            ..
                        }) => Some(from.field.as_str()),
                        _ => None,
                    })
                    .collect();
                for (index, entry) in record.entries.iter().enumerate() {
                    let entry_place = entry_place(&type_place, index, entry);
                    check_written(entry, &entry_place, report);
                    match entry {
                        Entry::Padding(_) => {}
                        Entry::Field(field) => {
                            let at = format!("struct type {name}'s field {}", field.name);
                            let from = measured.contains(field.name.as_str());
                            self.check_field(field, Level::Record, at, from, &entry_place, report);
                        }
                        Entry::Array(_) | Entry::Struct(_) | Entry::Checksum(_) => report.error(
                            Code::CommandShape,
                            &entry_place,
                            format!(
                                "struct type {name} holds the records of command array {}, and a \
                                 command record holds fields and padding",
                                array.name
                            ),
                        ),
                    }
                }
                check_pointers(&record.entries, &type_place, report);
                for entry in &record.entries {
                    if let Entry::Field(Field {
                        argument: Some(name),
                        ..
                    }) = entry
                    {
                        self.check_list_fits(array, name, place, report);
                    }
                }
            }
        }
    }

    /// Every list the catalog lets a caller give fits the array's count:
    /// exactly its fixed count, or no more than its count field holds and
    /// no fewer than the field's `add` (below which the field would be
    /// negative). A `multiply` that needs a length it divides is left to
    /// encoding.
    fn check_list_fits(&self, array: &ArrayEntry, name: &str, place: &Place, report: &mut Report) {
        let Some(argument) = self
            .arguments
            .into_iter()
            .flatten()
            .find(|argument| argument.name == name)
        else {
            return;
        };
        let Some(list) = argument.list else {
            return;
        };
        let least = list.min.unwrap_or(0);
        let problem = match &array.count {
            Some(Count::Fixed { count }) if least != *count || list.max != Some(*count) => {
                Some(format!("array {} holds exactly {count}", array.name))
            }
            Some(Count::Field(from)) => {
                // A count field that isn't an unsigned integer with a size
                // is reported with the layout.
                let Some(max_raw) = self.count_field_max(&from.field) else {
                    return;
                };
                // Past u64, the field holds any count a list can have.
                let most = from.max_value(max_raw);
                let add = from.add.unwrap_or(0);
                match (list.max, most) {
                    (None, Some(most)) => Some(format!("{} counts at most {most}", from.field)),
                    (Some(max), Some(most)) if u64::from(max) > most => {
                        Some(format!("{} counts at most {most}", from.field))
                    }
                    _ if add > 0 && i64::from(least) < add => Some(format!(
                        "{} holds the count less {add}, so array {} holds at least {add}",
                        from.field, array.name
                    )),
                    _ => None,
                }
            }
            _ => None,
        };
        let Some(problem) = problem else {
            return;
        };
        let promise = match (list.min, list.max) {
            (Some(min), Some(max)) if min == max => format!("exactly {min} values"),
            (Some(min), Some(max)) => format!("{min} to {max} values"),
            (Some(min), None) => format!("at least {min} values"),
            (None, Some(max)) => format!("up to {max} values"),
            (None, None) => "any number of values".to_string(),
        };
        report.error(
            Code::ValueRange,
            &place.key("count"),
            format!(
                "argument {name} takes {promise}, but {problem}; give the list bounds the array \
                 can hold"
            ),
        );
    }

    /// The largest raw value of the packet's field `name`, if it's an
    /// unsigned integer with a size.
    fn count_field_max(&self, name: &str) -> Option<u64> {
        self.packet.entries.iter().find_map(|entry| match entry {
            Entry::Field(field) if field.name == name && field.ty == ScalarType::Uint => {
                let size = field.size_bits.filter(|size| (1..=64).contains(size))?;
                Some(if size == 64 {
                    u64::MAX
                } else {
                    (1 << size) - 1
                })
            }
            _ => None,
        })
    }

    /// The argument an encoder names: the command has it, and it fits the
    /// encoder ([`Sources::check_argument`]). An encoder that fits is kept
    /// for [`check_writes`].
    fn check_encoded(
        &mut self,
        name: &'a str,
        encoder: &Encoder<'_>,
        in_array: bool,
        at: String,
        report: &mut Report,
    ) {
        let Some(arguments) = self.arguments else {
            return;
        };
        let Some(argument) = arguments.iter().find(|argument| argument.name == name) else {
            report.error(
                Code::UnknownArgument,
                &encoder.argument,
                format!("command {} has no argument {name}", self.packet.canonical),
            );
            return;
        };
        // What this packet's checks of the encoder find, apart: its values
        // are checked only when they find nothing, since those values would
        // only echo it. A struct type's field is checked for each packet
        // whose arrays hold its records, against that packet's command, so
        // another packet's findings there don't count.
        let mut found = Report::default();
        self.check_argument(name, argument, encoder, in_array, at, &mut found);
        if found.is_empty() && !self.unsettled(name) {
            self.writes.push(Write {
                packet: self.packet,
                field: encoder.field.clone(),
                argument,
                what: encoder.what.clone(),
                at: encoder.argument.clone(),
                own: encoder.own.clone(),
            });
        }
        report.absorb(found);
    }

    /// The argument is encoded once, a list exactly by an array, and its
    /// type fits the encoder.
    fn check_argument(
        &mut self,
        name: &'a str,
        argument: &Argument,
        encoder: &Encoder<'_>,
        in_array: bool,
        at: String,
        report: &mut Report,
    ) {
        if let Some(first) = self.encoded.get(name) {
            report.error(
                Code::DuplicateMapping,
                &encoder.argument,
                format!("argument {name} is already encoded by {first}"),
            );
        } else {
            self.encoded.insert(name, at);
        }
        let problem = match (argument.list.is_some(), in_array) {
            (true, false) => Some("a list, so an array encodes it, not a field"),
            (false, true) => {
                Some("a single value, so a field at the top of the packet encodes it, not an array")
            }
            _ => None,
        };
        if let Some(problem) = problem {
            report.error(
                Code::CanonicalType,
                &encoder.argument,
                format!("argument {name} is {problem}"),
            );
            return;
        }
        let settled = !self.unsettled(name);
        let states_known = !self
            .unsettled_states
            .is_some_and(|unsettled| unsettled.contains(name));
        // The states the catalog lists, which a field may give raw values
        // to although this packet accepts fewer of them.
        let cataloged = self
            .command
            .and_then(|command| command.arguments.iter().find(|a| a.name == name))
            .map_or(argument.states.as_slice(), |a| a.states.as_slice());
        let checks = FieldChecks {
            cataloged,
            settled,
            states_known,
        };
        check_argument_field(encoder, argument, &checks, report);
    }

    /// Whether this packet's limits for the argument have problems.
    fn unsettled(&self, name: &str) -> bool {
        self.unsettled
            .is_some_and(|unsettled| unsettled.contains(name))
    }
}

/// Every entry of a command packet is written, so none has a condition.
fn check_written(entry: &Entry, place: &Place, report: &mut Report) {
    let when = match entry {
        Entry::Field(field) => field.when.is_some(),
        Entry::Array(array) => array.when.is_some(),
        Entry::Padding(padding) => padding.when.is_some(),
        Entry::Struct(inner) => inner.when.is_some(),
        Entry::Checksum(_) => false,
    };
    if when {
        report.error(
            Code::CommandShape,
            &place.key("when"),
            "every entry of a command packet is written; a condition could silently drop an \
             argument",
        );
    }
}

/// An inverse is for a field that encodes an argument through a conversion
/// that doesn't invert exactly, and is itself a linear, polynomial or table
/// conversion.
fn check_inverse_keys(field: &Field, place: &Place, report: &mut Report) {
    let Some(explicit) = &field.inverse else {
        return;
    };
    let needs_inverse = field
        .conversion
        .as_ref()
        .is_some_and(|c| !inverts_exactly(c));
    if !needs_inverse {
        report.error(
            Code::ConversionType,
            &place.key("inverse"),
            "an inverse is for a conversion that doesn't invert exactly (a polynomial, a \
             spline, a step table, a table whose values rise and fall); this one does, or there \
             is none",
        );
    }
    if !matches!(
        explicit,
        Conversion::Linear { .. } | Conversion::Poly { .. } | Conversion::Table { .. }
    ) {
        report.error(
            Code::ConversionType,
            &place.key("inverse"),
            "an inverse is a linear, polynomial or table conversion, engineering value to raw",
        );
    }
}

fn entry_place(container: &Place, index: usize, entry: &Entry) -> Place {
    let (label, name) = match entry {
        Entry::Field(field) => ("field", field.name.as_str()),
        Entry::Array(array) => ("array", array.name.as_str()),
        Entry::Struct(inner) => ("struct", inner.name.as_str()),
        Entry::Checksum(checksum) => ("checksum", checksum.name.as_str()),
        Entry::Padding(_) => ("padding", ""),
    };
    container.item("entries", index, label, name)
}

/// Each field and array of a JSON command packet, or of a record, is
/// written at its own place inside its object: none equals another or
/// leads into it (`/a` and `/a/b`). That none is empty, the whole object,
/// holds for every field and array (`check_member_pointer`).
fn check_pointers(entries: &[Entry], place: &Place, report: &mut Report) {
    let mut written: Vec<(Vec<String>, &str)> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let (label, name, pointer) = match entry {
            Entry::Field(field) => ("field", &field.name, &field.pointer),
            Entry::Array(array) => ("array", &array.name, &array.pointer),
            _ => continue,
        };
        let Some(tokens) = pointer.as_deref().and_then(pointer_tokens) else {
            continue;
        };
        let pointer_place = place.item("entries", index, label, name).key("pointer");
        if tokens.is_empty() {
            continue;
        }
        if let Some((_, other)) = written
            .iter()
            .find(|(earlier, _)| earlier.starts_with(&tokens) || tokens.starts_with(earlier))
        {
            report.error(
                Code::Overlap,
                &pointer_place,
                format!(
                    "{name} and {other} would be written to the same place: one pointer equals \
                     or leads into the other"
                ),
            );
            continue;
        }
        written.push((tokens, name));
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
                text::name(text::encoding_of(field.text)),
                field.name
            ),
        );
        return;
    };
    match &field.length {
        Some(ByteLength::Fixed {
            bytes,
            terminator: None,
        }) if encoded.len() != *bytes as usize => report.error(
            Code::ValueRange,
            place,
            format!(
                "{value:?} is {} bytes, and field {} is exactly {bytes}",
                encoded.len(),
                field.name
            ),
        ),
        Some(ByteLength::Fixed {
            bytes,
            terminator: Some(_),
        }) if encoded.len() > *bytes as usize => report.error(
            Code::ValueRange,
            place,
            format!(
                "{value:?} is {} bytes, and field {} holds at most {bytes}",
                encoded.len(),
                field.name
            ),
        ),
        Some(
            ByteLength::Terminator { byte }
            | ByteLength::Fixed {
                terminator: Some(byte),
                ..
            },
        ) if encoded.contains(byte) => report.error(
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

/// The integers an integer or bool field holds: by its size in a binary
/// payload, 64 bits in a JSON one, where fields have no size (a binary
/// field without one is reported with the layout). `None` for a size out
/// of range, reported elsewhere.
fn integer_range(field: &Field) -> Option<(i128, i128)> {
    let size = match field.size_bits {
        None => 64,
        Some(size) if (1..=64).contains(&size) => size,
        Some(_) => return None,
    };
    match field.ty {
        ScalarType::Bool => Some((0, 1)),
        ScalarType::Uint | ScalarType::Int => {
            IntForm::of(field.ty, field.signed).map(|form| form.range(size))
        }
        _ => None,
    }
}

/// A field, or an array's element taken as a field, that encodes an
/// argument, with where its keys are reported.
struct Encoder<'a> {
    field: &'a Field,
    /// How messages name it: `field x`, `array x's elements`.
    what: String,
    /// Where it names the argument.
    argument: Place,
    /// Where its own keys are: conversion, inverse, unit.
    own: Place,
}

/// An argument's type fits the field that encodes it, through the
/// field's conversion, in the field's unit.
/// What [`check_argument_field`] checks a field against, besides the
/// argument as the packet takes it.
struct FieldChecks<'a> {
    /// The catalog's states for an enum argument: a states conversion
    /// names only those, and gives a raw value to each the packet accepts.
    cataloged: &'a [String],
    /// False for an argument whose limits in this packet have problems
    /// (reported already): the checks that read its range (an inverse's
    /// direction, coverage) would only echo them, so they wait until the
    /// limits are fixed.
    settled: bool,
    /// False for an argument whose states in this packet have problems
    /// (reported already), so the states it accepts aren't known.
    states_known: bool,
}

/// The field can encode the argument, as `checks` says.
fn check_argument_field(
    encoder: &Encoder<'_>,
    argument: &Argument,
    checks: &FieldChecks<'_>,
    report: &mut Report,
) {
    let field = encoder.field;
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
            &encoder.argument,
            format!(
                "{} can't encode argument {}, which is {}",
                encoder.what,
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
            &encoder.own.key("conversion"),
            format!(
                "{} doesn't invert exactly, so a field that encodes an argument through it needs \
                 an inverse (engineering value to raw)",
                conversion_name(conversion)
            ),
        );
    }
    // Coverage is checked at the range's ends, which bound the raw values
    // only once the inverse is known to be monotonic over the range.
    let monotonic =
        checks.settled && readable_range(argument) && check_inverse(encoder, argument, report);
    if let Some(Conversion::Linear { scale, .. }) = field.conversion
        && scale == 0.0
    {
        report.error(
            Code::ConversionType,
            &encoder.own.key("conversion").key("scale"),
            "a linear conversion with scale 0 has no inverse, so it can't encode an argument",
        );
    }
    if let (Some(Conversion::States { states }), false) =
        (&field.conversion, argument.states.is_empty())
    {
        let mut seen = BTreeSet::new();
        let mut misspelled = false;
        for (index, state) in states.iter().enumerate() {
            let state_place = encoder.own.key("conversion").at("states", index);
            if !checks.cataloged.contains(&state.name) {
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
        for state in argument
            .states
            .iter()
            .filter(|_| !misspelled && checks.states_known)
        {
            if !states.iter().any(|listed| &listed.name == state) {
                report.error(
                    Code::CanonicalType,
                    &encoder.own.key("conversion"),
                    format!(
                        "argument {}'s state {state:?} has no raw value here, so it couldn't be \
                         sent; give it one, or list the states this packet accepts without it \
                         (states, IR 2.3)",
                        argument.name
                    ),
                );
            }
        }
    }
    if monotonic {
        check_coverage(encoder, argument, report);
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
            &encoder.own.key("unit"),
            report,
        );
    }
}

/// Whether an argument's range ends are values of its type (numbers for an
/// f64 argument, integers of the type for a u64 or i64 one). One that isn't
/// is reported on the catalog, so the checks that need the range leave it
/// out.
fn readable_range(argument: &Argument) -> bool {
    argument.range.is_none() || argument.range_ends().is_some()
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
            interpolation: Some(crate::ir::Interpolation::Step),
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
fn check_inverse(encoder: &Encoder<'_>, argument: &Argument, report: &mut Report) -> bool {
    let field = encoder.field;
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
    // A malformed inverse is reported with the set's conversions.
    if field
        .inverse
        .as_ref()
        .is_some_and(|explicit| !well_formed(explicit))
    {
        return false;
    }
    let ends = match argument.range_ends() {
        Some(RangeEnds::Numbers(min, max)) => (min, max),
        _ => (None, None),
    };
    let (Some(min), Some(max)) = ends else {
        // A range with no ends is reported on the catalog.
        if argument
            .range
            .as_ref()
            .is_some_and(|r| r.min.is_none() && r.max.is_none())
        {
            return false;
        }
        report.error(
            Code::ValueRange,
            &encoder.argument,
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
                &encoder.argument,
                format!(
                    "argument {}'s range reaches {end}, past the points of {}; keep the range \
                     inside them, or let the conversion extrapolate",
                    argument.name, encoder.what
                ),
            );
            return false;
        }
    }
    // A table that inverts exactly is monotonic by construction.
    if let Some(explicit) = &field.inverse
        && !monotonic_over(explicit, field_min, field_max, encoder, argument, report)
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
                    &encoder.argument,
                    format!(
                        "argument {}'s range end {end} encodes to raw {raw}, outside the points \
                         of {} ({first} to {last}), so decoding couldn't read it back; narrow \
                         the range, widen the points, or let the conversion extrapolate",
                        argument.name, encoder.what
                    ),
                );
                return false;
            }
        }
    }
    true
}

/// A conversion that can be evaluated: a polynomial with a coefficient, a
/// table with at least two points in strictly increasing raw order.
fn well_formed(conversion: &Conversion) -> bool {
    match conversion {
        Conversion::Poly { coefficients } => !coefficients.is_empty(),
        Conversion::Table { points, .. } | Conversion::Spline { points, .. } => {
            points.len() >= 2 && points.windows(2).all(|pair| pair[0].raw < pair[1].raw)
        }
        Conversion::Linear { .. } | Conversion::States { .. } => true,
    }
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
    encoder: &Encoder<'_>,
    argument: &Argument,
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
                &encoder.own.key("inverse"),
                format!(
                    "the inverse of {} rises and falls over argument {}'s range (it turns near \
                     {x}), so a raw value can't be traced back; give an inverse that only rises \
                     or only falls from {min} to {max}",
                    encoder.what, argument.name
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
/// argument without a range promises every value of its type, and a bool
/// is 0 or 1. A float field is checked by [`check_float_coverage`].
fn check_coverage(encoder: &Encoder<'_>, argument: &Argument, report: &mut Report) {
    let field = encoder.field;
    if field.ty == ScalarType::Float {
        check_float_coverage(encoder, argument, report);
        return;
    }
    let Some((field_low, field_high)) =
        integer_range(field).filter(|_| field.ty != ScalarType::Bool)
    else {
        return;
    };
    // A bool is written as 0 or 1, which only a 1-bit int can't hold.
    if argument.ty == ParamType::Bool {
        if field_high < 1 {
            report.error(
                Code::ValueRange,
                &encoder.argument,
                format!(
                    "argument {} is a bool, written as 0 or 1, but {} holds raw {field_low} to \
                     {field_high}",
                    argument.name, encoder.what
                ),
            );
        }
        return;
    }
    let (type_low, type_high) = match argument.ty {
        ParamType::U64 => (0.0, u64::MAX as f64),
        ParamType::I64 => (i64::MIN as f64, i64::MAX as f64),
        ParamType::F64 => (f64::NEG_INFINITY, f64::INFINITY),
        _ => return,
    };
    let open = ArgumentRange::default();
    let range = argument.range.as_ref().unwrap_or(&open);
    // Readable, or the catalog reports it and coverage isn't checked.
    let (numbers, integers) = match argument.range_ends() {
        Some(RangeEnds::Numbers(min, max)) => ((min, max), (None, None)),
        Some(RangeEnds::Integers(min, max)) => {
            ((min.map(|n| n as f64), max.map(|n| n as f64)), (min, max))
        }
        None => return,
    };
    let low = numbers.0.unwrap_or(type_low);
    let high = numbers.1.unwrap_or(type_high);
    let (raw_low, raw_high) = match &field.conversion {
        // An f64 argument is first converted into the field's unit (an
        // integer argument is in it already).
        None => {
            let into_field =
                |x: f64| convert_between(x, argument.unit.as_deref(), field.unit.as_deref());
            let (Some(a), Some(b)) = (into_field(low), into_field(high)) else {
                return;
            };
            (a.min(b), a.max(b))
        }
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
    // An unconverted integer argument's missing end is its type's own,
    // which is an integer: a u64 fills a 64-bit field exactly.
    let own_ends = match (&field.conversion, argument.ty) {
        (None, ParamType::U64) => Some((0, i128::from(u64::MAX))),
        (None, ParamType::I64) => Some((i128::from(i64::MIN), i128::from(i64::MAX))),
        _ => None,
    };
    let (low_raw, high_raw) = match own_ends {
        // Its ends are integers, compared exactly.
        Some((own_low, own_high)) => (
            Some(integers.0.unwrap_or(own_low)),
            Some(integers.1.unwrap_or(own_high)),
        ),
        None => (exact(raw_low), exact(raw_high)),
    };
    let fits = low_raw.is_some_and(|low| low >= field_low)
        && high_raw.is_some_and(|high| high <= field_high);
    if fits {
        return;
    }
    let shown = |end: &Option<Literal>, own: String| end.as_ref().map_or(own, show_end);
    let (own_low, own_high) = match own_ends {
        Some((own_low, own_high)) => (own_low.to_string(), own_high.to_string()),
        None => (low.to_string(), high.to_string()),
    };
    let promise = match (&range.min, &range.max) {
        (None, None) => format!("has no range, so it can be any {}", type_label(argument.ty)),
        (min, max) => format!("can be {} to {}", shown(min, own_low), shown(max, own_high)),
    };
    report.error(
        Code::ValueRange,
        &encoder.argument,
        format!(
            "argument {} {promise}, but {} holds raw {field_low} to {field_high}; give the \
             argument, or this packet's arguments, a range it can hold",
            argument.name, encoder.what
        ),
    );
}

/// Each end an f64 argument's range gives encodes to a finite raw value in
/// a float field, rounded to binary32 for a 32-bit one, so every value in
/// range does. An end left out isn't checked: most f64 arguments have no
/// range, and refusing them all for a binary32 field would refuse what
/// most definitions mean.
fn check_float_coverage(encoder: &Encoder<'_>, argument: &Argument, report: &mut Report) {
    let field = encoder.field;
    let Some(RangeEnds::Numbers(min, max)) = argument.range_ends() else {
        return;
    };
    // A field has a size only in a binary payload.
    let binary = field.size_bits.is_some();
    let single = binary && field.size_bits == Some(32);
    for end in [min, max].into_iter().flatten() {
        // A unit that doesn't convert is reported on its own.
        let Some(raw) = raw_unchecked(field, end, argument.unit.as_deref(), binary) else {
            continue;
        };
        if raw.is_finite() {
            continue;
        }
        let holds = if single {
            format!(
                "finite binary32 numbers, up to {}",
                format_double(f64::from(f32::MAX))
            )
        } else {
            "finite numbers".to_string()
        };
        report.error(
            Code::ValueRange,
            &encoder.argument,
            format!(
                "argument {}'s range reaches {}, which {} can't write: it holds {holds}; narrow \
                 the range",
                argument.name,
                format_double(end),
                encoder.what
            ),
        );
        return;
    }
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
    let command_only = |key: &str, at: &Place, report: &mut Report| {
        report.error(
            Code::CommandOnly,
            at,
            format!(
                "{key} belongs to command packets; a telemetry entry's value is read from the \
                 message"
            ),
        );
    };
    for (index, entry) in entries.iter().enumerate() {
        if let Entry::Array(array) = entry {
            let array_place = place.item("entries", index, "array", &array.name);
            if array.argument.is_some() {
                command_only("argument", &array_place.key("argument"), report);
            }
            if let Element::Scalar(ScalarElement {
                inverse: Some(_), ..
            }) = &array.element
            {
                command_only(
                    "inverse",
                    &array_place.key("element").key("inverse"),
                    report,
                );
            }
        }
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
                command_only(key, &field_place.key(key), report);
            }
        }
    }
}

/// The struct types command arrays hold records of.
pub(super) fn record_types(set: &Set) -> BTreeSet<&str> {
    set.commands
        .iter()
        .flat_map(|packet| &packet.entries)
        .filter_map(|entry| match entry {
            Entry::Array(ArrayEntry {
                element: Element::Struct { name },
                ..
            }) => Some(name.as_str()),
            _ => None,
        })
        .collect()
}

/// A struct type holds command records or telemetry, not both: a record's
/// fields take arguments and constants, which telemetry can't.
pub(super) fn check_record_uses(set: &Set, place: &Place, report: &mut Report) {
    let records = record_types(set);
    let packets = set
        .packets
        .iter()
        .map(|packet| (format!("telemetry packet {}", packet.name), &packet.entries));
    let types = set
        .types
        .iter()
        .map(|(name, struct_type)| (format!("struct type {name}"), &struct_type.entries));
    let mut reported = BTreeSet::new();
    for (user, entries) in packets.chain(types) {
        for used in used_types(entries) {
            if records.contains(used) && reported.insert(used) {
                report.error(
                    Code::CommandShape,
                    &place.map_item("types", used, "type"),
                    format!(
                        "struct type {used} holds the records of a command array, and {user} \
                         uses it too; a struct type holds command records or telemetry, not both"
                    ),
                );
            }
        }
    }
}
