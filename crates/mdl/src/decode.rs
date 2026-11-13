//! Decoding: a message into the values its packet defines.
//!
//! [`FamilySets::decode`] routes a message (see [`crate::route`]),
//! identifies its packet within the chosen set, and extracts every entry
//! into a [`Record`] of raw values: numbers as read, before any
//! conversion, and text and bytes as they are. A message decodes
//! completely or not at all: any failure is a [`DecodeError`], with the
//! path of the entry that failed. A JSON payload is parsed once, whether
//! routing or decoding needs it first.
//!
//! # Identification
//!
//! Packets are grouped by the keys they read, and groups are tried from
//! most keys to fewest; the first packet whose keys all match is the
//! message's. Validation guarantees that packets matching one message have
//! strictly nested keys, so this finds the most specific one. A key that
//! can't be read (missing metadata, a payload too short, nothing at a
//! pointer) doesn't match, so a more specific packet that can't be read
//! falls back to a less specific one. A group's keys are read in order,
//! metadata and pointers first, and the group is skipped as soon as no
//! packet in it has the values read so far. A field key without a static
//! position is read by decoding its packet up to that field: a failure
//! before it means no match, and a failure after it doesn't affect
//! identification (decoding the packet then reports it).
//!
//! # Rules
//!
//! - **Positions.** An entry with `offset_bits` starts there, relative to
//!   its container; one without starts where the previous entry ended. An
//!   absent conditional entry ends where it starts. A struct's size is the
//!   furthest end of its present entries.
//! - **References.** Conditions, counts and lengths use the raw values of
//!   earlier, unconditional fields of the same container. A count or
//!   length from a field is `raw * multiply + add`; a negative result is
//!   an error.
//! - **Lengths.** A packet with an exact length decodes only that many
//!   bytes, so a longer payload is accepted (layouts may grow at the end),
//!   and a shorter one is an error, as is one shorter than an `at_least`
//!   length. Entries that read to the end read to the packet's end.
//! - **Safety.** A count is checked against the bits left in the packet
//!   before anything is allocated, and every array element takes at least
//!   one bit, so a payload can't ask for more values than it has bits.
//!   Strings and bytes are checked against the bits left before they're
//!   copied. Nesting depth is bounded by the definition. A record can
//!   still be large: a `Value` takes 32 bytes, so a payload of one-bit
//!   values decodes to about 256 times its own size.
//! - **Text** is strict: UTF-8 must be valid, ASCII must be below 0x80,
//!   and Latin-1 maps each byte to the code point of the same value. A
//!   fixed length keeps every byte, padding included.
//! - **JSON values.** A uint or int field takes a number whose value is an
//!   exact integer in range (`42.0` is 42); a float field takes any
//!   number; bool and string fields take their JSON type. Nothing at a
//!   present field's pointer is an error.

mod clock;
mod command;
mod extract;
mod identify;
mod meaning;

use std::collections::BTreeMap;
use std::fmt;

use serde_json::Value as Json;
use thiserror::Error;

use crate::bindings::Resolver;
use crate::bits::{read_bits, sign_extend};
use crate::checksum::{Algorithm, Compiled};
use crate::ir::{
    ByteOrder, Catalog, CommandPacket, Entry, Family, Length, Packet, Scalar, ScalarType, Set,
    TextEncoding,
};
use crate::message::{Message, MetaValue};
use crate::route::{FamilySets, Readable, Route, RouteError, json_integer};
use crate::text;
use crate::validate::{
    KeyId, KeyValue, by_value, command_identities, decoding_facts, exact, static_offsets,
};

pub use command::{CommandDecodeError, DecodedCommand};
use extract::{BinaryWalker, FieldNames, JsonWalker, PacketChecksums, Segment, Tables};
use identify::Index;
use meaning::{CompiledComposite, sample_value, visit};

pub use clock::{Correlator, EventTime};
pub use meaning::{Engineering, Quality, Sample, SampleValue};

/// A decoded message.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Decoded<'a> {
    /// Who sent it and which set decoded it.
    pub route: Route<'a>,
    pub packet: &'a Packet,
    /// When the values were taken, by the set's time source.
    pub event_time: EventTime,
    /// Every entry, raw.
    pub record: Record<'a>,
    /// The canonical values: mapped fields and array elements in record
    /// order, then composites by canonical name, as set artifacts list
    /// them.
    pub samples: Vec<Sample<'a>>,
}

impl<'a> Decoded<'a> {
    /// The engineering value of the field or scalar element at `path`
    /// (`bus_counts`, `pos.x`, `samples[3]`), mapped or not: its raw value
    /// through its conversion. `None` if nothing scalar is there.
    pub fn engineering(&self, path: &str) -> Option<Engineering> {
        meaning::engineering_at(path, self.packet, &self.route.set.types, &self.record)
    }

    /// The engineering value of every field and scalar element, by path,
    /// in record order. Computed on each call, not during decoding.
    pub fn engineering_values(&self) -> Vec<(String, Engineering)> {
        let mut values = Vec::new();
        let walked = visit::<std::convert::Infallible>(
            &self.packet.entries,
            &self.record,
            &self.route.set.types,
            "",
            None,
            &mut |visit| {
                if let Some(engineering) = meaning::engineering(visit.meaning, visit.value) {
                    values.push((visit.path, engineering));
                }
                Ok(())
            },
        );
        match walked {
            Ok(()) => values,
            Err(never) => match never {},
        }
    }
}

/// The decoded entries of a packet or struct, in definition order. Absent
/// conditional entries and padding aren't listed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Record<'a> {
    /// Each value with its entry's index in the container and its name,
    /// in index order.
    entries: Vec<(usize, &'a str, Value<'a>)>,
}

impl<'a> Record<'a> {
    pub fn get(&self, name: &str) -> Option<&Value<'a>> {
        self.entries
            .iter()
            .find(|(_, entry, _)| *entry == name)
            .map(|(_, _, value)| value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&'a str, &Value<'a>)> {
        self.entries.iter().map(|(_, name, value)| (*name, value))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn push(&mut self, index: usize, name: &'a str, value: Value<'a>) {
        self.entries.push((index, name, value));
    }

    /// The value of the container's entry at `index`, if it's present.
    fn at_entry(&self, index: usize) -> Option<&Value<'a>> {
        self.entries
            .binary_search_by_key(&index, |(entry, _, _)| *entry)
            .ok()
            .map(|position| &self.entries[position].2)
    }
}

/// One decoded value, raw: before any conversion.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value<'a> {
    Uint(u64),
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Bytes(Vec<u8>),
    Array(Vec<Value<'a>>),
    Record(Record<'a>),
    /// No reading: a `null` element of a JSON array, which keeps its
    /// position.
    Null,
}

/// Why a message can't be decoded.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum DecodeError {
    #[error(transparent)]
    Route(#[from] RouteError),
    #[error("no packet of set {set} (family {family}) matches the message")]
    NoPacket { family: String, set: String },
    /// The payload has `given` bytes, fewer than the packet's length of
    /// `needed`, exactly or at least as `exact` says (`decoding.md` §5.1).
    #[error("{}", too_short(packet, *needed, *given, *exact))]
    TooShort {
        packet: String,
        needed: u64,
        given: u64,
        exact: bool,
    },
    #[error("packet {packet}: its event time can't be read: {reason}")]
    Time { packet: String, reason: String },
    /// An entry couldn't be decoded. `path` names it the way records do,
    /// like `cells[2].volts`; it's empty for the packet itself.
    #[error("{}", entry_failure(packet, path, problem))]
    Entry {
        packet: String,
        path: String,
        problem: Problem,
    },
}

impl DecodeError {
    /// A stable code for the error, for counting and dead-letter queues.
    pub fn code(&self) -> &'static str {
        match self {
            DecodeError::Route(err) => err.code(),
            DecodeError::NoPacket { .. } => "no_packet",
            DecodeError::TooShort { .. } => "too_short",
            DecodeError::Time { .. } => "invalid_time",
            DecodeError::Entry { problem, .. } => problem.code(),
        }
    }
}

fn too_short(packet: &str, needed: u64, given: u64, exact: bool) -> String {
    let least = if exact { "" } else { " at least" };
    format!("packet {packet} is{least} {needed} bytes, but the payload has {given}")
}

fn entry_failure(packet: &str, path: &str, problem: &Problem) -> String {
    if path.is_empty() {
        format!("packet {packet}: {problem}")
    } else {
        format!("packet {packet}, {path}: {problem}")
    }
}

/// What went wrong with one entry.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum Problem {
    #[error("it needs the packet's bits up to {needed_bits}, but the packet has {available_bits}")]
    PastEnd {
        needed_bits: u64,
        available_bits: u64,
    },
    /// Padding that runs past the end. Padding has no path, so the error
    /// names its container and says it's padding (`decoding.md` §5.2).
    #[error(
        "padding needs the packet's bits up to {needed_bits}, but the packet has {available_bits}"
    )]
    PaddingPastEnd {
        needed_bits: u64,
        available_bits: u64,
    },
    #[error("no {byte:#04x} byte ends it before the packet ends")]
    NoTerminator { byte: u8 },
    /// Bytes that aren't text in the field's encoding: the one it names,
    /// or UTF-8.
    #[error("its bytes aren't valid {}", text::name(*.encoding))]
    InvalidText { encoding: TextEncoding },
    #[error("an array element took no bits; every element takes at least one")]
    EmptyElement,
    #[error("a length or count taken from a field is {value}")]
    BadLength { value: i128 },
    #[error("no value at {pointer}")]
    Missing { pointer: String },
    /// A JSON value of another type than the entry takes: a number for a
    /// `uint`, `int` or `float` field, an array for an array, an object
    /// for a struct or a record element (`decoding.md` §5.3).
    #[error("expected {}, found {}", .expected.phrase(), .found.phrase())]
    WrongType { expected: JsonType, found: JsonType },
    /// A number that isn't an integer (§2.3) in the range of `expected`,
    /// the field's type: `uint` or `int`. `value` is the number as text,
    /// since no Rust integer holds every JSON number: an integer literal
    /// within 64 bits as its digits, any other number as its double's
    /// canonical JSON text (`1.50` is `1.5`, `1E20` is
    /// `100000000000000000000`, `1e21` is `1e+21`).
    #[error("{value} doesn't fit {}", type_name(*.expected))]
    OutOfRange { value: String, expected: ScalarType },
    /// A checksum that doesn't match the bytes it covers: the message was
    /// damaged on the way.
    #[error("it reads {stored:#x}, but the bytes it covers give {computed:#x}")]
    BadChecksum { stored: u64, computed: u64 },
    /// Only for a definition that didn't pass validation.
    #[error("the definition can't be decoded: {0}")]
    Layout(&'static str),
}

impl Problem {
    pub fn code(&self) -> &'static str {
        match self {
            Problem::PastEnd { .. } | Problem::PaddingPastEnd { .. } => "past_end",
            Problem::NoTerminator { .. } => "no_terminator",
            Problem::InvalidText { .. } => "invalid_text",
            Problem::EmptyElement => "empty_element",
            Problem::BadLength { .. } => "bad_length",
            Problem::Missing { .. } => "missing_value",
            Problem::WrongType { .. } => "wrong_type",
            Problem::OutOfRange { .. } => "out_of_range",
            Problem::BadChecksum { .. } => "bad_checksum",
            Problem::Layout(_) => "invalid_layout",
        }
    }
}

/// A field's type, as an `out_of_range` message names it.
fn type_name(ty: ScalarType) -> &'static str {
    match ty {
        ScalarType::Uint => "an unsigned 64-bit integer",
        ScalarType::Int => "a signed 64-bit integer",
        ScalarType::Float => "a float",
        ScalarType::Bool => "a boolean",
        ScalarType::String => "a string",
        ScalarType::Bytes => "bytes",
    }
}

/// One of the six types of JSON value (RFC 8259 §1). JSON has no others,
/// so matches on it needn't allow for more. It displays as RFC 8259
/// names it: `null`, `boolean`, `array`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JsonType {
    Null,
    Boolean,
    Number,
    String,
    Array,
    Object,
}

impl JsonType {
    /// The type of `value`.
    pub(crate) fn of(value: &Json) -> JsonType {
        match value {
            Json::Null => JsonType::Null,
            Json::Bool(_) => JsonType::Boolean,
            Json::Number(_) => JsonType::Number,
            Json::String(_) => JsonType::String,
            Json::Array(_) => JsonType::Array,
            Json::Object(_) => JsonType::Object,
        }
    }

    /// As a message names it: `null`, `a boolean`, `an array`.
    pub(crate) fn phrase(self) -> &'static str {
        match self {
            JsonType::Null => "null",
            JsonType::Boolean => "a boolean",
            JsonType::Number => "a number",
            JsonType::String => "a string",
            JsonType::Array => "an array",
            JsonType::Object => "an object",
        }
    }
}

impl fmt::Display for JsonType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            JsonType::Null => "null",
            JsonType::Boolean => "boolean",
            JsonType::Number => "number",
            JsonType::String => "string",
            JsonType::Array => "array",
            JsonType::Object => "object",
        })
    }
}

impl FamilySets {
    /// Route `message`, identify its packet and decode it.
    pub fn decode<R: Resolver + ?Sized>(
        &self,
        message: &Message<'_>,
        resolver: &R,
    ) -> Result<Decoded<'_>, DecodeError> {
        let readable = self.readable(message)?;
        let (route, loaded) = self.route_parts(message, &readable, resolver)?;
        let program = &loaded.program;
        let (index, packet, record) =
            program.decode(&loaded.set, &self.family().name, message, &readable)?;
        let json = readable.json(&self.family().name)?;
        let event_time = clock::event_time(
            &loaded.set,
            packet,
            &record,
            &readable,
            json,
            message.ground_time,
        )
        .map_err(|reason| DecodeError::Time {
            packet: packet.name.clone(),
            reason,
        })?;
        let samples = program.samples(index, packet, &loaded.set, &loaded.catalog, &record)?;
        Ok(Decoded {
            route,
            packet,
            event_time,
            record,
            samples,
        })
    }
}

/// What decoding precomputes for a set.
#[derive(Debug, Clone)]
pub(crate) struct Program {
    index: Index,
    /// The exact size of each struct type that has one.
    type_sizes: BTreeMap<String, u64>,
    /// Field names of each packet, by packet index, and of each type.
    packet_names: Vec<FieldNames>,
    type_names: BTreeMap<String, FieldNames>,
    /// Each packet's composites, by packet index.
    composites: Vec<Vec<CompiledComposite>>,
    /// Each packet's checksums, by packet index.
    checksums: Vec<PacketChecksums>,
    commands: CommandProgram,
}

/// What decoding a set's command packets precomputes, by command index.
#[derive(Debug, Clone)]
struct CommandProgram {
    index: Index,
    names: Vec<FieldNames>,
    checksums: Vec<PacketChecksums>,
}

/// A packet's checksums, compiled, with those at static positions.
fn packet_checksums(entries: &[Entry], type_sizes: &BTreeMap<&str, u64>) -> PacketChecksums {
    let offsets = static_offsets(entries, type_sizes);
    let mut compiled = PacketChecksums::default();
    for (index, entry) in entries.iter().enumerate() {
        let Entry::Checksum(checksum) = entry else {
            continue;
        };
        let Some(algorithm) = Algorithm::resolve(&checksum.algorithm) else {
            continue;
        };
        compiled.compiled.insert(index, Compiled::new(algorithm));
        if let Some(Some(at)) = offsets.get(index) {
            compiled.fixed.push((index, *at));
        }
    }
    // Verified in order of position (decoding.md §5.6), which offsets can
    // make other than the order they're listed in.
    compiled.fixed.sort_by_key(|&(_, at)| at);
    compiled
}

/// A packet's payload, ready to walk.
enum Walked<'p> {
    Binary(&'p [u8], ByteOrder),
    Json(&'p Json),
}

impl Program {
    /// For a validated set and its family.
    pub(crate) fn compile(set: &Set, family: &Family) -> Program {
        let (identities, type_sizes) = decoding_facts(set, family);
        let checksums = set
            .packets
            .iter()
            .map(|packet| packet_checksums(&packet.entries, &type_sizes))
            .collect();
        let commands = CommandProgram {
            index: Index::new(command_identities(set, family, &type_sizes)),
            names: set
                .commands
                .iter()
                .map(|packet| field_names(&packet.entries))
                .collect(),
            checksums: set
                .commands
                .iter()
                .map(|packet| packet_checksums(&packet.entries, &type_sizes))
                .collect(),
        };
        Program {
            checksums,
            commands,
            index: Index::new(identities),
            type_sizes: type_sizes
                .into_iter()
                .map(|(name, size)| (name.to_string(), size))
                .collect(),
            packet_names: set
                .packets
                .iter()
                .map(|packet| field_names(&packet.entries))
                .collect(),
            type_names: set
                .types
                .iter()
                .map(|(name, struct_type)| (name.clone(), field_names(&struct_type.entries)))
                .collect(),
            composites: set
                .packets
                .iter()
                .map(|packet| {
                    packet
                        .composites
                        .iter()
                        .filter_map(|composite| {
                            CompiledComposite::compile(composite, packet, &set.types)
                        })
                        .collect()
                })
                .collect(),
        }
    }

    /// The canonical samples of a decoded packet.
    fn samples<'a>(
        &'a self,
        index: usize,
        packet: &'a Packet,
        set: &'a Set,
        catalog: &Catalog,
        record: &Record<'a>,
    ) -> Result<Vec<Sample<'a>>, DecodeError> {
        let failed = |path: String, problem: Problem| DecodeError::Entry {
            packet: packet.name.clone(),
            path,
            problem,
        };
        let mut samples = Vec::new();
        let mut failed_at = String::new();
        let walked = visit(
            &packet.entries,
            record,
            &set.types,
            "",
            None,
            &mut |visit| {
                let Some((param_name, param)) = visit
                    .canonical
                    .and_then(|name| Some((name, catalog.params.get(name)?)))
                else {
                    return Ok(());
                };
                match sample_value(param, visit.meaning, visit.value) {
                    Ok((value, quality)) => {
                        samples.push(Sample {
                            param: param_name,
                            value,
                            path: visit.path,
                            index: visit.index,
                            time_offset_ns: visit.time_offset_ns,
                            quality,
                        });
                        Ok(())
                    }
                    Err(problem) => {
                        failed_at = visit.path;
                        Err(problem)
                    }
                }
            },
        );
        if let Err(problem) = walked {
            return Err(failed(failed_at, problem));
        }
        for composite in self.composites.get(index).into_iter().flatten() {
            let sample = composite
                .sample(catalog, record)
                .map_err(|problem| failed(String::new(), problem))?;
            samples.extend(sample);
        }
        Ok(samples)
    }

    fn tables<'a>(&'a self, set: &'a Set) -> Tables<'a> {
        Tables {
            types: &set.types,
            type_sizes: &self.type_sizes,
            type_names: &self.type_names,
        }
    }

    fn decode<'a>(
        &'a self,
        set: &'a Set,
        family: &str,
        message: &Message<'_>,
        readable: &Readable<'_>,
    ) -> Result<(usize, &'a Packet, Record<'a>), DecodeError> {
        let json = readable.json(family)?;
        let mut evaluate = |key: &KeyId| self.key(set, key, message, readable, json);
        let found = self.index.find(&mut evaluate);
        let found = found.and_then(|index| Some((index, set.packets.get(index)?)));
        let Some((index, packet)) = found else {
            return Err(DecodeError::NoPacket {
                family: family.to_string(),
                set: set.name.clone(),
            });
        };
        let record = self.extract(set, index, packet, readable, json)?;
        Ok((index, packet, record))
    }

    /// The command packet a payload is, by index, and its record; `None` if
    /// no command packet of the set matches.
    pub(super) fn decode_command<'a>(
        &'a self,
        set: &'a Set,
        family: &str,
        message: &Message<'_>,
        readable: &Readable<'_>,
    ) -> Result<Option<(&'a CommandPacket, Record<'a>)>, DecodeError> {
        let json = readable.json(family)?;
        let mut evaluate = |key: &KeyId| self.key(set, key, message, readable, json);
        let found = self.commands.index.find(&mut evaluate);
        let Some((index, packet)) = found.and_then(|index| Some((index, set.commands.get(index)?)))
        else {
            return Ok(None);
        };
        let (Some(names), checksums) = (
            self.commands.names.get(index),
            self.commands.checksums.get(index),
        ) else {
            return Err(entry_error(
                &packet.name,
                &[],
                Problem::Layout("a command packet not compiled"),
            ));
        };
        // A command packet has no length: the payload is the packet, and
        // anything after its last entry is ignored, as layouts may grow.
        let payload = match (readable, json) {
            (Readable::Binary { bytes, order }, _) => Walked::Binary(bytes, *order),
            (Readable::Json { .. }, Some(document)) => Walked::Json(document),
            (Readable::Json { .. }, None) => {
                return Err(entry_error(
                    &packet.name,
                    &[],
                    Problem::Layout("a JSON payload that wasn't parsed"),
                ));
            }
        };
        let record = self.walk(
            set,
            &packet.name,
            &packet.entries,
            names,
            checksums,
            payload,
        )?;
        Ok(Some((packet, record)))
    }

    /// A key's value in a message, or `None` if it can't be read.
    fn key(
        &self,
        set: &Set,
        key: &KeyId,
        message: &Message<'_>,
        readable: &Readable<'_>,
        json: Option<&Json>,
    ) -> Option<KeyValue> {
        match key {
            KeyId::Metadata(name) => metadata_key(message, name),
            KeyId::Pointer(pointer) => json.and_then(|doc| doc.pointer(pointer)).and_then(json_key),
            KeyId::Bits {
                offset,
                size,
                signed,
                order,
            } => bits_key(readable, *offset, *size, *signed, *order),
            KeyId::Bytes { offset, length } => bytes_key(readable, *offset, *length),
            KeyId::Unplaced { packet, name } => self.unplaced_key(set, *packet, name, readable),
        }
    }

    /// A field key without a static position: the packet decoded up to
    /// that field. Only binary packets have field keys.
    fn unplaced_key(
        &self,
        set: &Set,
        index: usize,
        name: &str,
        readable: &Readable<'_>,
    ) -> Option<KeyValue> {
        let Readable::Binary { bytes, order } = readable else {
            return None;
        };
        let packet = set.packets.get(index)?;
        let names = self.packet_names.get(index)?;
        // The key is read whether or not the payload is long enough for
        // the whole packet; a length is checked once the packet is chosen.
        let bytes = match packet.length {
            Some(Length::Exact { bytes: exact }) => {
                let exact = usize::try_from(exact).unwrap_or(usize::MAX);
                &bytes[..bytes.len().min(exact)]
            }
            _ => bytes,
        };
        let mut walker = BinaryWalker::new(self.tables(set), bytes, *order)
            .with_checksums(self.checksums.get(index));
        let (record, _) = walker.entries(&packet.entries, names, 0, Some(name)).ok()?;
        record_key(record.get(name)?)
    }

    fn extract<'a>(
        &'a self,
        set: &'a Set,
        index: usize,
        packet: &'a Packet,
        readable: &Readable<'_>,
        json: Option<&Json>,
    ) -> Result<Record<'a>, DecodeError> {
        let Some(names) = self.packet_names.get(index) else {
            return Err(entry_error(
                &packet.name,
                &[],
                Problem::Layout("a packet not compiled"),
            ));
        };
        let payload = match (readable, json) {
            (Readable::Binary { bytes, order }, _) => {
                Walked::Binary(within_length(packet, bytes)?, *order)
            }
            (Readable::Json { .. }, Some(document)) => Walked::Json(document),
            (Readable::Json { .. }, None) => {
                return Err(entry_error(
                    &packet.name,
                    &[],
                    Problem::Layout("a JSON payload that wasn't parsed"),
                ));
            }
        };
        let checksums = self.checksums.get(index);
        self.walk(
            set,
            &packet.name,
            &packet.entries,
            names,
            checksums,
            payload,
        )
    }

    /// A packet's entries read from its payload; a binary packet's checksums
    /// at static positions are verified first.
    fn walk<'a>(
        &'a self,
        set: &'a Set,
        name: &str,
        entries: &'a [Entry],
        names: &'a FieldNames,
        checksums: Option<&'a PacketChecksums>,
        payload: Walked<'_>,
    ) -> Result<Record<'a>, DecodeError> {
        match payload {
            Walked::Binary(bytes, order) => {
                let mut walker =
                    BinaryWalker::new(self.tables(set), bytes, order).with_checksums(checksums);
                let result = walker
                    .verify_fixed(entries)
                    .and_then(|()| walker.entries(entries, names, 0, None));
                result
                    .map(|(record, _)| record)
                    .map_err(|problem| entry_error(name, walker.path(), problem))
            }
            Walked::Json(document) => {
                let mut walker = JsonWalker::new(self.tables(set));
                let result = walker.packet(entries, names, document);
                result.map_err(|problem| entry_error(name, walker.path(), problem))
            }
        }
    }
}

/// Each field of a container by name, with its entry index.
fn field_names(entries: &[Entry]) -> FieldNames {
    entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| match entry {
            Entry::Field(field) => Some((field.name.clone(), index)),
            _ => None,
        })
        .collect()
}

/// The packet's bytes: the payload, cut to an exact length.
fn within_length<'p>(packet: &Packet, payload: &'p [u8]) -> Result<&'p [u8], DecodeError> {
    let (needed, exact) = match packet.length {
        Some(Length::Exact { bytes }) => (bytes, true),
        Some(Length::AtLeast { bytes }) => (bytes, false),
        None => return Ok(payload),
    };
    match usize::try_from(needed) {
        Ok(len) if len <= payload.len() => Ok(if exact { &payload[..len] } else { payload }),
        // Shorter than a length that's at most u32::MAX, so the payload's
        // length is exact as a u64.
        _ => Err(DecodeError::TooShort {
            packet: packet.name.clone(),
            needed: u64::from(needed),
            given: u64::try_from(payload.len()).unwrap_or(u64::MAX),
            exact,
        }),
    }
}

fn entry_error(packet: &str, path: &[Segment<'_>], problem: Problem) -> DecodeError {
    let mut text = String::new();
    for segment in path {
        match segment {
            Segment::Name(name) => {
                if !text.is_empty() {
                    text.push('.');
                }
                text.push_str(name);
            }
            Segment::Index(index) => {
                text.push('[');
                text.push_str(&index.to_string());
                text.push(']');
            }
        }
    }
    DecodeError::Entry {
        packet: packet.to_string(),
        path: text,
        problem,
    }
}

/// A metadata identify key: integers by value, as validation compares them.
fn metadata_key(message: &Message<'_>, name: &str) -> Option<KeyValue> {
    let mut values = message
        .metadata
        .iter()
        .filter(|(key, _)| *key == name)
        .map(|(_, value)| value);
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    Some(match value {
        MetaValue::Uint(n) => KeyValue::Int(i128::from(*n)),
        MetaValue::Int(n) => KeyValue::Int(i128::from(*n)),
        MetaValue::Text(text) => by_value(&Scalar::Str((*text).to_string())),
    })
}

/// A JSON Pointer identify key: the JSON value exactly.
fn json_key(value: &Json) -> Option<KeyValue> {
    match value {
        Json::String(text) => Some(exact(&Scalar::Str(text.clone()))),
        Json::Number(number) => json_integer(number).map(KeyValue::Int),
        _ => None,
    }
}

fn bits_key(
    readable: &Readable<'_>,
    offset: u64,
    size: u32,
    signed: bool,
    order: ByteOrder,
) -> Option<KeyValue> {
    let Readable::Binary { bytes, .. } = readable else {
        return None;
    };
    let raw = read_bits(bytes, offset, size, order).ok()?;
    Some(KeyValue::Int(if signed {
        i128::from(sign_extend(raw, size))
    } else {
        i128::from(raw)
    }))
}

fn bytes_key(readable: &Readable<'_>, offset: u64, length: u32) -> Option<KeyValue> {
    let Readable::Binary { bytes, .. } = readable else {
        return None;
    };
    let first = usize::try_from(offset / 8)
        .ok()
        .filter(|_| offset.is_multiple_of(8))?;
    let stop = first.checked_add(usize::try_from(length).ok()?)?;
    bytes
        .get(first..stop)
        .map(|run| KeyValue::Bytes(run.to_vec()))
}

/// A field key read from a decoded record.
fn record_key(value: &Value<'_>) -> Option<KeyValue> {
    match value {
        Value::Uint(n) => Some(KeyValue::Int(i128::from(*n))),
        Value::Int(n) => Some(KeyValue::Int(i128::from(*n))),
        Value::Bool(b) => Some(KeyValue::Int(i128::from(*b))),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
