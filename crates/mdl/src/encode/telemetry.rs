//! Encoding telemetry: values for one of a set's packets, as the payload
//! that decoding reads back as those values. `spec/encoding.md` §9 is the
//! normative description.
//!
//! Encoding runs in four steps. Each entry's raw value is worked out in
//! the order decoding reads them: from the request's raw values and its
//! params' values, and from what the definitions fix (the family's
//! locators, the packet's identify keys, counts and lengths from what they
//! measure). The packet is laid out where decoding finds each entry, as
//! bits or as a JSON document, with checksums written last. Locators and
//! identify keys that read metadata become the metadata the link frames it
//! with. Then the payload is decoded, and it's refused unless it decodes as
//! this packet, from this native id, with these values.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use thiserror::Error;

use self::clock::{EVENT_TIME, Reading};
use super::write::{Lookup, Node, json_text, write_field};
use super::{FieldProblem, Raw, float_raw, integer_raw, rounded, run_bytes, show_double};
use crate::bindings::{Binding, NativeId, Resolver};
use crate::bits::{IntForm, write_bits};
use crate::canonical::write_string;
use crate::checksum::Algorithm;
use crate::convert::{Converted, inverse};
use crate::decode::{
    DecodeError, EventTime, Problem, Record, SampleValue, Value, boot_instant, good_value, holds,
    instant, is_sentinel, when,
};
use crate::ir::{
    ArrayEntry, ByteLength, ByteOrder, Catalog, Conversion, Count, Digest, Element, Encoding,
    Entry, Family, Field, Frame, FromField, Length, Locator, Packet, Param, ParamType, Scalar,
    ScalarType, Set, StructType, Time,
};
use crate::message::{Message, MetaValue, Payload};
use crate::route::FamilySets;
use crate::time::Timestamp;
use crate::units::convert_between;
use crate::validate::rules::{integer_value, pointer_tokens};
use crate::validate::static_offsets;

/// A raw value for one entry, as decoding's record holds it: before any
/// conversion.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum RawValue {
    Integer(i128),
    Float(f64),
    Bool(bool),
    Text(String),
    Bytes(Vec<u8>),
    /// An array's elements.
    List(Vec<RawValue>),
    /// A struct's entries, or a record element's, by name.
    Record(Vec<(String, RawValue)>),
    /// No reading, at a position of a JSON array.
    Null,
}

/// One of a set's packets to encode: the set and the packet, the native
/// id of the asset that sends it, its params' values, raw values for its
/// entries, and when its values were taken.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct TelemetryRequest {
    pub set: String,
    pub packet: String,
    pub native_id: NativeId,
    /// Each param's values, in its unit, by name: one for each sample
    /// decoding gives it from the message, in record order.
    pub params: Vec<(String, Vec<SampleValue>)>,
    /// Raw values by entry name: a struct's or record's as a record, an
    /// array's as a list.
    pub raw: Vec<(String, RawValue)>,
    /// The event time: when the values were taken, which the packet's
    /// clocks read.
    pub at: Option<Timestamp>,
    /// When the asset booted, for clocks that count time since boot.
    pub booted: Option<Timestamp>,
}

impl TelemetryRequest {
    pub fn new(
        set: impl Into<String>,
        packet: impl Into<String>,
        native_id: impl Into<NativeId>,
    ) -> Self {
        TelemetryRequest {
            set: set.into(),
            packet: packet.into(),
            native_id: native_id.into(),
            params: Vec::new(),
            raw: Vec::new(),
            at: None,
            booted: None,
        }
    }

    /// A param the message holds once.
    pub fn with_param(self, name: impl Into<String>, value: SampleValue) -> Self {
        self.with_params(name, vec![value])
    }

    /// A param the message holds several times (an array's elements), its
    /// values in record order.
    pub fn with_params(mut self, name: impl Into<String>, values: Vec<SampleValue>) -> Self {
        self.params.push((name.into(), values));
        self
    }

    pub fn with_raw(mut self, name: impl Into<String>, value: RawValue) -> Self {
        self.raw.push((name.into(), value));
        self
    }

    /// When the values were taken, which the packet's clocks read.
    pub fn with_at(mut self, at: Timestamp) -> Self {
        self.at = Some(at);
        self
    }

    /// When the asset booted, which clocks that count time since boot
    /// read with the event time.
    pub fn with_booted(mut self, booted: Timestamp) -> Self {
        self.booted = Some(booted);
        self
    }
}

/// A metadata value an encoded packet is framed with.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MetadataValue {
    Uint(u64),
    Int(i64),
    Text(String),
}

impl MetadataValue {
    /// As a link delivers it to a decoder.
    pub fn as_meta(&self) -> MetaValue<'_> {
        match self {
            MetadataValue::Uint(n) => MetaValue::Uint(*n),
            MetadataValue::Int(n) => MetaValue::Int(*n),
            MetadataValue::Text(text) => MetaValue::Text(text),
        }
    }

    fn of_scalar(value: &Scalar) -> MetadataValue {
        match value {
            Scalar::Int(n) => MetadataValue::Int(*n),
            Scalar::Str(text) => MetadataValue::Text(text.clone()),
        }
    }

    /// A native id is an integer from -2^63 to 2^64 - 1, or text.
    fn of_native_id(id: &NativeId) -> MetadataValue {
        match id.as_int() {
            Some(n) => match (u64::try_from(n), i64::try_from(n)) {
                (Ok(n), _) => MetadataValue::Uint(n),
                (_, Ok(n)) => MetadataValue::Int(n),
                _ => MetadataValue::Text(n.to_string()),
            },
            None => MetadataValue::Text(id.to_string()),
        }
    }

    /// Compared as decoding compares metadata: by value, so text in an
    /// integer's one decimal spelling is that integer.
    fn same(&self, other: &MetadataValue) -> bool {
        self.as_native_id() == other.as_native_id()
    }

    fn as_native_id(&self) -> NativeId {
        match self {
            MetadataValue::Uint(n) => NativeId::from(*n),
            MetadataValue::Int(n) => NativeId::from(*n),
            MetadataValue::Text(text) => NativeId::from_text(text.clone()),
        }
    }
}

impl fmt::Display for MetadataValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MetadataValue::Uint(n) => write!(f, "{n}"),
            MetadataValue::Int(n) => write!(f, "{n}"),
            MetadataValue::Text(text) => write!(f, "{text:?}"),
        }
    }
}

/// An encoded telemetry packet, ready for its link to frame with its
/// metadata.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct EncodedTelemetry<'a> {
    pub set: &'a Set,
    /// The set artifact's digest, which decoding the payload stamps.
    pub def_version: &'a Digest,
    pub packet: &'a Packet,
    /// The payload. A JSON payload is UTF-8 text.
    pub payload: Vec<u8>,
    /// What the link frames the payload with, by name, in name order.
    pub metadata: Vec<(String, MetadataValue)>,
    /// Every entry's raw value, as decoding the payload gives them.
    pub record: Record<'a>,
    /// The event time decoding the payload gives: at or before the
    /// request's, by less than one tick of the packet's clock.
    pub event_time: EventTime,
    /// When the asset booted, as decoding the payload teaches it, for the
    /// set's correlation packet.
    pub booted: Option<Timestamp>,
}

/// Why a packet can't be encoded.
#[derive(Debug, Clone, PartialEq, Error)]
#[non_exhaustive]
pub enum TelemetryError {
    #[error("family {family} has no set {set}")]
    UnknownSet { family: String, set: String },
    #[error("set {set} has no packet {packet}")]
    UnknownPacket { set: String, packet: String },
    #[error("param {param}: {problem}")]
    Param {
        param: String,
        problem: ParamProblem,
    },
    #[error("packet {packet}, {}: {problem}", entry_words(path))]
    Entry {
        packet: String,
        /// The entry, as a record path (`hdr.len`, `samples[3]`).
        path: String,
        problem: EntryProblem,
    },
    #[error("native id {native_id}: {reason}")]
    NativeId { native_id: String, reason: String },
    #[error("packet {packet}: {detail}")]
    DecodesOtherwise { packet: String, detail: String },
}

fn entry_words(path: &str) -> String {
    if path.is_empty() {
        "the whole packet".to_string()
    } else {
        path.to_string()
    }
}

impl TelemetryError {
    /// The stable error code, as `spec/encoding.md` lists them.
    pub fn code(&self) -> &'static str {
        match self {
            TelemetryError::UnknownSet { .. } => "unknown_set",
            TelemetryError::UnknownPacket { .. } => "unknown_packet",
            TelemetryError::Param { problem, .. } => problem.code(),
            TelemetryError::Entry { problem, .. } => problem.code(),
            TelemetryError::NativeId { .. } => "native_id",
            TelemetryError::DecodesOtherwise { .. } => "decodes_otherwise",
        }
    }
}

/// What's wrong with a param of a request.
#[derive(Debug, Clone, PartialEq, Error)]
#[non_exhaustive]
pub enum ParamProblem {
    #[error("packet {packet} maps no such param")]
    Unknown { packet: String },
    #[error("it's given more than once")]
    Duplicate,
    /// The entries that take its values, in record order, took `holds` of
    /// the `given`: an entry given a raw value or a composite's member
    /// takes none.
    #[error("it's given {}, and {}", values_of(*given), entries_taking(*holds))]
    Count { given: usize, holds: usize },
}

impl ParamProblem {
    pub fn code(&self) -> &'static str {
        match self {
            ParamProblem::Unknown { .. } => "unknown_param",
            ParamProblem::Duplicate => "duplicate_value",
            ParamProblem::Count { .. } => "bad_count",
        }
    }
}

fn entries_taking(n: usize) -> String {
    match n {
        1 => "1 entry takes its value from it".to_string(),
        _ => format!("{n} entries take their value from it"),
    }
}

fn values_of(n: usize) -> String {
    match n {
        1 => "1 value".to_string(),
        _ => format!("{n} values"),
    }
}

/// What's wrong with the value of one entry.
#[derive(Debug, Clone, PartialEq, Error)]
#[non_exhaustive]
pub enum EntryProblem {
    #[error("{0}")]
    Field(FieldProblem),
    #[error("the message holds it, and no value is given for it, raw or by param")]
    Missing,
    #[error(
        "the message doesn't hold it here, since its condition doesn't hold, so it takes no value"
    )]
    Absent,
    #[error("the packet has no such entry")]
    Unknown,
    #[error("it's given more than one value: {sources}")]
    Duplicate { sources: String },
    #[error("{given} isn't {expected}")]
    WrongType {
        given: String,
        expected: &'static str,
    },
    #[error("{given} isn't a finite number")]
    NonFinite { given: String },
    #[error("{given:?} isn't {among}")]
    UnknownState { given: String, among: String },
    #[error(
        "its conversion doesn't invert exactly and the field has no inverse, so encoding takes \
         its raw value, not param {param}'s"
    )]
    NoInverse { param: String },
    #[error("{given} is outside what its conversion covers")]
    Outside { given: String },
    /// A param's value that becomes one of the field's invalid raw values,
    /// which decoding reads as no reading.
    #[error("its value becomes raw value {raw}, which the field lists as no reading")]
    InvalidRaw { raw: String },
    #[error("{why} gives it {holds}, and it's given {given}")]
    WrongConstant {
        why: String,
        holds: String,
        given: String,
    },
    /// Two definitions fix one value differently.
    #[error("{detail}")]
    Conflict { detail: String },
    /// An array of records that hold only padding, counted by a count
    /// field's value with no raw list.
    #[error(
        "its records hold only padding and take no values, so give them as a raw list of empty \
         records"
    )]
    EmptyRecords,
    /// An array in a repeated record whose count only its param's values
    /// could give, which don't say which of them are this record's.
    #[error(
        "nothing says how many elements it has here: param {param}'s values don't say which \
         are this record's, so give its count, or its elements as raw values"
    )]
    Uncounted { param: String },
    #[error(
        "the set's time source reads the packet's time where no entry holds it, so it needs the \
         event time"
    )]
    NoEventTime,
    /// A clock the request's instants don't set, given no value.
    #[error(
        "it's one of the packet's clocks, and it's given no value: give {give}, or a value for it"
    )]
    UnsetClock { give: &'static str },
    /// The event time as a reading the clock doesn't hold.
    #[error("the event time makes it {raw}, and it holds {holds}")]
    EventTimeRange { raw: String, holds: String },
    /// A clock's value that decoding can't read as a time.
    #[error("decoding can't read an event time from it: {reason}")]
    InvalidTime { reason: String },
}

impl EntryProblem {
    pub fn code(&self) -> &'static str {
        match self {
            EntryProblem::Field(problem) => problem.code(),
            EntryProblem::Missing => "missing_value",
            EntryProblem::Absent => "absent_entry",
            EntryProblem::Unknown => "unknown_entry",
            EntryProblem::Duplicate { .. } => "duplicate_value",
            EntryProblem::WrongType { .. } => "wrong_type",
            EntryProblem::NonFinite { .. } => "non_finite",
            EntryProblem::UnknownState { .. } => "unknown_state",
            EntryProblem::NoInverse { .. } => "no_inverse",
            EntryProblem::Outside { .. } => "out_of_range",
            EntryProblem::InvalidRaw { .. } => "invalid_raw",
            EntryProblem::WrongConstant { .. } | EntryProblem::Conflict { .. } => "wrong_constant",
            EntryProblem::EventTimeRange { .. } => "raw_range",
            EntryProblem::InvalidTime { .. } => "invalid_time",
            EntryProblem::NoEventTime
            | EntryProblem::UnsetClock { .. }
            | EntryProblem::Uncounted { .. }
            | EntryProblem::EmptyRecords => "missing_value",
        }
    }
}

impl From<FieldProblem> for EntryProblem {
    fn from(problem: FieldProblem) -> Self {
        EntryProblem::Field(problem)
    }
}

impl FamilySets {
    /// Encodes one of a set's packets as telemetry the request's native id
    /// sends (`spec/encoding.md` §9).
    pub fn encode_telemetry(
        &self,
        request: &TelemetryRequest,
    ) -> Result<EncodedTelemetry<'_>, TelemetryError> {
        let loaded = self
            .loaded(&request.set)
            .ok_or_else(|| TelemetryError::UnknownSet {
                family: self.family().name.clone(),
                set: request.set.clone(),
            })?;
        let set = &loaded.set;
        let packet = set
            .packets
            .iter()
            .find(|packet| packet.name == request.packet)
            .ok_or_else(|| TelemetryError::UnknownPacket {
                set: set.name.clone(),
                packet: request.packet.clone(),
            })?;
        let family = self.family();
        let order = self.byte_order();
        let type_sizes = loaded.program.type_sizes();
        let fixed = Fixed::of(family, set, packet, request, &type_sizes)?;
        let mut planner = Planner::new(set, packet, &loaded.catalog, order.is_some(), request)?;
        let planned = planner.container(&packet.entries, "", &request.raw, Some(&fixed))?;
        planner.finish()?;
        let fail = |path: String, problem: FieldProblem| TelemetryError::Entry {
            packet: packet.name.clone(),
            path,
            problem: EntryProblem::Field(problem),
        };
        let (payload, checksums) = match order {
            Some(order) => {
                let mut out = BinaryOut::new(&set.types, order);
                out.lay(packet, &planned, &fixed)
                    .map_err(|(path, problem)| fail(path, problem))?;
                (out.payload, out.computed)
            }
            None => (
                json_payload(packet, &set.types, &planned, &fixed)?,
                BTreeMap::new(),
            ),
        };
        for (index, plan) in planned.iter().enumerate() {
            if let (Planned::Checksum(Some(given)), Some(computed), Some(entry)) =
                (plan, checksums.get(&index), packet.entries.get(index))
                && given != computed
            {
                return Err(TelemetryError::Entry {
                    packet: packet.name.clone(),
                    path: entry_name(entry).unwrap_or_default().to_string(),
                    problem: EntryProblem::WrongConstant {
                        why: "the bytes it covers".to_string(),
                        holds: computed.to_string(),
                        given: given.to_string(),
                    },
                });
            }
        }
        let record = record_of(
            &packet.entries,
            &set.types,
            planned,
            &checksums,
            order.is_some(),
        );
        let metadata: Vec<(String, MetadataValue)> = fixed
            .metadata
            .into_iter()
            .map(|(name, (value, _))| (name, value))
            .collect();
        let (event_time, booted) =
            check_round_trip(self, set, packet, request, &payload, &metadata, &record)?;
        Ok(EncodedTelemetry {
            set,
            def_version: &loaded.def_version,
            packet,
            payload,
            metadata,
            record,
            event_time,
            booted,
        })
    }
}

// ---- What the definitions fix ----------------------------------------------

/// A value the definitions fix: the native id, the set's version, or an
/// identify key's value.
#[derive(Debug, Clone, PartialEq)]
enum Fix {
    Int(i128),
    /// A float clock's reading.
    Float(f64),
    Text(String),
}

impl Fix {
    fn of_scalar(value: &Scalar) -> Fix {
        match value {
            Scalar::Int(n) => Fix::Int(i128::from(*n)),
            Scalar::Str(text) => Fix::Text(text.clone()),
        }
    }

    fn of_native_id(id: &NativeId) -> Fix {
        match id.as_int() {
            Some(n) => Fix::Int(n),
            None => Fix::Text(id.to_string()),
        }
    }

    /// As a JSON value: a number or a string.
    fn json(&self) -> String {
        match self {
            Fix::Int(n) => n.to_string(),
            Fix::Float(x) => show_double(*x),
            Fix::Text(text) => {
                let mut out = String::new();
                write_string(&mut out, text);
                out
            }
        }
    }

    fn show(&self) -> String {
        match self {
            Fix::Int(n) => n.to_string(),
            Fix::Float(x) => show_double(*x),
            Fix::Text(text) => format!("{text:?}"),
        }
    }
}

/// What the definitions and the request's instants fix before any value
/// is given: the metadata the link frames the packet with, raw values of
/// top-level fields that the locators, identify keys and clocks read, and
/// what's written over the laid-out payload where no field holds it.
#[derive(Debug, Default)]
struct Fixed {
    /// By name: the value, and what fixes it.
    metadata: BTreeMap<String, (MetadataValue, String)>,
    /// Top-level fields by name: the value, and what fixes it.
    fields: BTreeMap<String, (Fix, String)>,
    /// What fixes a value from the native id, for a conflict's code.
    alias: String,
    /// A binary payload's bits that locators and a CUC time code read.
    bits: Vec<BitRun>,
    /// A JSON payload's values at pointers no field holds: the pointer's
    /// tokens, the value, what.
    pointers: Vec<(String, Fix, String)>,
    /// Clock fields the request's instants don't set, by name: what would.
    unset: BTreeMap<String, &'static str>,
}

/// Bits written over a laid-out binary payload.
#[derive(Debug)]
struct BitRun {
    offset: u64,
    size: u32,
    value: u64,
    /// The byte order they're read in: `None` for the family's.
    order: Option<ByteOrder>,
    /// What fixes them.
    what: String,
}

impl Fixed {
    fn of(
        family: &Family,
        set: &Set,
        packet: &Packet,
        request: &TelemetryRequest,
        type_sizes: &BTreeMap<&str, u64>,
    ) -> Result<Fixed, TelemetryError> {
        let native_id = &request.native_id;
        let refuse = |reason: String| TelemetryError::NativeId {
            native_id: native_id.to_string(),
            reason,
        };
        if native_id.as_text().is_some_and(str::is_empty) {
            return Err(refuse(
                "a native id is never empty, so empty text names no asset".to_string(),
            ));
        }
        let alias = format!("family {}'s alias", family.name);
        let mut fixed = Fixed {
            alias: alias.clone(),
            ..Fixed::default()
        };
        let offsets = static_offsets(&packet.entries, type_sizes);
        let order = family.byte_order;
        match &family.alias {
            Locator::Bits {
                offset_bits,
                size_bits,
            } => {
                let value = native_id
                    .as_int()
                    .and_then(|n| fits_bits(n, *size_bits))
                    .ok_or_else(|| {
                        refuse(format!(
                            "{alias} is {size_bits} bits, which hold an integer from 0 to {}",
                            bits_max(*size_bits)
                        ))
                    })?;
                let holder = bits_holder(packet, &offsets, order, order, *offset_bits, *size_bits);
                fixed.locator_bits(
                    packet,
                    holder,
                    (*offset_bits, *size_bits, value),
                    Fix::of_native_id(native_id),
                    &alias,
                    native_id,
                )?;
            }
            Locator::Metadata { name } => {
                fixed.metadata(
                    packet,
                    name,
                    MetadataValue::of_native_id(native_id),
                    &alias,
                    native_id,
                )?;
            }
            Locator::Path { pointer } => {
                fixed.locator_path(
                    packet,
                    pointer,
                    Fix::of_native_id(native_id),
                    &alias,
                    native_id,
                )?;
            }
            Locator::Fixed { value } => {
                if NativeId::from_text(value.clone()) != *native_id {
                    return Err(refuse(format!("{alias} is always {value:?}")));
                }
            }
        }
        if let (Some(locator), Some(version)) = (&family.version, &set.version) {
            let what = format!("set {}'s version", set.name);
            match locator {
                Locator::Bits {
                    offset_bits,
                    size_bits,
                } => {
                    let value = integer_value(version)
                        .and_then(|n| fits_bits(n, *size_bits))
                        .ok_or_else(|| layout_error(packet, "a version its locator's bits hold"))?;
                    let holder =
                        bits_holder(packet, &offsets, order, order, *offset_bits, *size_bits);
                    fixed.locator_bits(
                        packet,
                        holder,
                        (*offset_bits, *size_bits, value),
                        Fix::of_scalar(version),
                        &what,
                        native_id,
                    )?;
                }
                Locator::Metadata { name } => {
                    fixed.metadata(
                        packet,
                        name,
                        MetadataValue::of_scalar(version),
                        &what,
                        native_id,
                    )?;
                }
                Locator::Path { pointer } => {
                    fixed.locator_path(
                        packet,
                        pointer,
                        Fix::of_scalar(version),
                        &what,
                        native_id,
                    )?;
                }
                Locator::Fixed { .. } => {}
            }
        }
        for (key, value) in &packet.identify {
            let what = format!("identify key {key}");
            let declared = family.metadata.iter().any(|name| name == key);
            if key.starts_with('/') {
                fixed.locator_path(packet, key, Fix::of_scalar(value), &what, native_id)?;
            } else if family.encoding == Encoding::Binary && !declared {
                fixed.field(packet, key, Fix::of_scalar(value), &what, native_id)?;
            } else {
                fixed.metadata(
                    packet,
                    key,
                    MetadataValue::of_scalar(value),
                    &what,
                    native_id,
                )?;
            }
        }
        let binary = family.encoding == Encoding::Binary;
        let clocks = clock::clocks(set, packet, request.at, request.booted, binary).map_err(
            |(path, problem)| TelemetryError::Entry {
                packet: packet.name.clone(),
                path,
                problem,
            },
        )?;
        for reading in clocks.readings {
            match reading {
                Reading::Field { name, value, why } => {
                    fixed.field(packet, &name, value, why, native_id)?;
                }
                Reading::Text { pointer, text } => {
                    fixed.locator_path(packet, &pointer, Fix::Text(text), EVENT_TIME, native_id)?;
                }
                Reading::Code {
                    offset_bits,
                    coarse_bytes,
                    coarse,
                    fine_bytes,
                    fine,
                } => {
                    let coarse_size = u32::from(coarse_bytes) * 8;
                    let fine_size = u32::from(fine_bytes) * 8;
                    let fine_at = offset_bits + coarse_size;
                    let big = Some(ByteOrder::Big);
                    let parts = [
                        (offset_bits, coarse_size, coarse),
                        (fine_at, fine_size, fine),
                    ];
                    for (at, size, part) in parts.into_iter().filter(|(_, size, _)| *size > 0) {
                        // A field that holds exactly the part, as decoding
                        // reads it, takes it.
                        if let Some(name) = bits_holder(packet, &offsets, order, big, at, size) {
                            let value = Fix::Int(i128::from(part));
                            fixed.field(packet, &name, value, EVENT_TIME, native_id)?;
                        }
                    }
                    fixed.bits.push(BitRun {
                        offset: u64::from(offset_bits),
                        size: coarse_size + fine_size,
                        value: (coarse << fine_size) | fine,
                        order: big,
                        what: EVENT_TIME.to_string(),
                    });
                }
            }
        }
        fixed.unset = clocks.unset.into_iter().collect();
        Ok(fixed)
    }

    /// Bits a locator reads: `holder`, the top-level field that holds
    /// exactly them, if one does, takes the value; they're written over the
    /// payload either way.
    fn locator_bits(
        &mut self,
        packet: &Packet,
        holder: Option<String>,
        (offset, size, bits): (u32, u32, u64),
        value: Fix,
        what: &str,
        native_id: &NativeId,
    ) -> Result<(), TelemetryError> {
        if let Some(name) = holder {
            self.field(packet, &name, value, what, native_id)?;
        }
        self.bits.push(BitRun {
            offset: u64::from(offset),
            size,
            value: bits,
            order: None,
            what: what.to_string(),
        });
        Ok(())
    }

    /// A pointer a locator or identify key reads: the top-level field at
    /// it, if one is, takes the value; otherwise it's written into the
    /// document.
    fn locator_path(
        &mut self,
        packet: &Packet,
        pointer: &str,
        value: Fix,
        what: &str,
        native_id: &NativeId,
    ) -> Result<(), TelemetryError> {
        match top_level_at(packet, pointer) {
            Some(name) => self.field(packet, &name, value, what, native_id),
            None => {
                self.pointers
                    .push((pointer.to_string(), value, what.to_string()));
                Ok(())
            }
        }
    }

    fn field(
        &mut self,
        packet: &Packet,
        name: &str,
        value: Fix,
        what: &str,
        native_id: &NativeId,
    ) -> Result<(), TelemetryError> {
        if let Some((earlier, earlier_what)) = self.fields.get(name) {
            if *earlier != value {
                let detail = format!(
                    "{earlier_what} makes {name} {}, and {what} makes it {}",
                    earlier.show(),
                    value.show()
                );
                return Err(self.conflict(packet, name, earlier_what, what, detail, native_id));
            }
            return Ok(());
        }
        self.fields
            .insert(name.to_string(), (value, what.to_string()));
        Ok(())
    }

    fn metadata(
        &mut self,
        packet: &Packet,
        name: &str,
        value: MetadataValue,
        what: &str,
        native_id: &NativeId,
    ) -> Result<(), TelemetryError> {
        if let Some((earlier, earlier_what)) = self.metadata.get(name) {
            if !earlier.same(&value) {
                let detail = format!(
                    "{earlier_what} makes metadata {name} {earlier}, and {what} makes it {value}"
                );
                return Err(self.conflict(packet, name, earlier_what, what, detail, native_id));
            }
            return Ok(());
        }
        self.metadata
            .insert(name.to_string(), (value, what.to_string()));
        Ok(())
    }

    /// Two definitions fixing one value differently: the native id can't
    /// send the packet when one of them is the alias.
    fn conflict(
        &self,
        packet: &Packet,
        name: &str,
        earlier: &str,
        later: &str,
        detail: String,
        native_id: &NativeId,
    ) -> TelemetryError {
        if earlier == self.alias || later == self.alias {
            return TelemetryError::NativeId {
                native_id: native_id.to_string(),
                reason: format!("packet {} can't be sent with it: {detail}", packet.name),
            };
        }
        TelemetryError::Entry {
            packet: packet.name.clone(),
            path: name.to_string(),
            problem: EntryProblem::Conflict { detail },
        }
    }
}

/// The top-level field that holds exactly the bits a locator or a CUC
/// time code reads, as it reads them in byte order `reads`: an unsigned
/// integer of that size at that static position, read in that order (in
/// its own or the family's, `order`), always present. A whole byte reads
/// alike in either order.
fn bits_holder(
    packet: &Packet,
    offsets: &[Option<u64>],
    order: Option<ByteOrder>,
    reads: Option<ByteOrder>,
    offset: u32,
    size: u32,
) -> Option<String> {
    let whole_byte = size == 8 && offset.is_multiple_of(8);
    packet
        .entries
        .iter()
        .zip(offsets)
        .find_map(|(entry, at)| match entry {
            Entry::Field(field)
                if field.ty == ScalarType::Uint
                    && field.size_bits == Some(size)
                    && *at == Some(u64::from(offset))
                    && field.when.is_none()
                    && (whole_byte || field.byte_order.or(order) == reads) =>
            {
                Some(field.name.clone())
            }
            _ => None,
        })
}

fn layout_error(packet: &Packet, what: &'static str) -> TelemetryError {
    TelemetryError::Entry {
        packet: packet.name.clone(),
        path: String::new(),
        problem: EntryProblem::Field(FieldProblem::Layout(what)),
    }
}

/// The name of the packet's top-level field at `pointer`, if one is there.
fn top_level_at(packet: &Packet, pointer: &str) -> Option<String> {
    packet.entries.iter().find_map(|entry| match entry {
        Entry::Field(field) if field.pointer.as_deref() == Some(pointer) => {
            Some(field.name.clone())
        }
        _ => None,
    })
}

/// `n` as bits of `size`: an integer from 0 to 2^size - 1.
fn fits_bits(n: i128, size: u32) -> Option<u64> {
    let value = u64::try_from(n).ok()?;
    (size >= 64 || value >> size == 0).then_some(value)
}

fn bits_max(size: u32) -> String {
    if size >= 64 {
        u64::MAX.to_string()
    } else {
        ((1_u128 << size) - 1).to_string()
    }
}

// ---- Values -------------------------------------------------------------------

/// What one entry holds, once worked out.
#[derive(Debug, Clone)]
enum Planned {
    /// Not in the message: its condition doesn't hold, or it's an optional
    /// JSON entry given no value.
    Absent,
    Padding,
    /// Worked out once the packet is laid out; the value given, to check.
    Checksum(Option<u64>),
    Scalar(Raw),
    /// An array's elements: scalars, records or nulls.
    Array(Vec<Planned>),
    /// A struct's or record element's entries, by entry index.
    Record(Vec<Planned>),
    /// No reading, at a position of a JSON array.
    Null,
}

/// A param's values, taken in record order.
struct Feed<'r> {
    values: &'r [SampleValue],
    next: usize,
}

impl Feed<'_> {
    fn remaining(&self) -> usize {
        self.values.len().saturating_sub(self.next)
    }
}

/// A composite member's value: a vector's component in the composite
/// param's unit (degrees for a geodetic latitude or longitude).
struct Member {
    param: String,
    value: f64,
    unit: Option<String>,
    used: bool,
}

impl<'a, 'r> Planner<'a, 'r> {
    fn marks(&self) -> Marks<'r> {
        Marks {
            feeds: self
                .feeds
                .iter()
                .map(|(name, feed)| (*name, feed.next))
                .collect(),
            unused: self
                .members
                .iter()
                .filter(|(_, member)| !member.used)
                .map(|(path, _)| path.clone())
                .collect(),
        }
    }

    fn taken_since(&self, marks: &Marks<'r>) -> Taken<'r> {
        Taken {
            feeds: marks
                .feeds
                .iter()
                .filter(|(name, next)| self.feeds.get(name).is_some_and(|feed| feed.next != *next))
                .copied()
                .collect(),
            members: marks
                .unused
                .iter()
                .filter(|path| self.members.get(*path).is_some_and(|member| member.used))
                .cloned()
                .collect(),
        }
    }

    /// Gives back what working an entry out took.
    fn give_back(&mut self, taken: Taken<'r>) {
        for (name, next) in taken.feeds {
            if let Some(feed) = self.feeds.get_mut(name) {
                feed.next = next;
            }
        }
        for path in taken.members {
            if let Some(member) = self.members.get_mut(&path) {
                member.used = false;
            }
        }
    }
}

/// Works out each entry's raw value.
struct Planner<'a, 'r> {
    set: &'a Set,
    packet: &'a Packet,
    catalog: &'a Catalog,
    binary: bool,
    feeds: BTreeMap<&'r str, Feed<'r>>,
    members: BTreeMap<String, Member>,
}

/// A container's raw values given, by entry name.
type Given<'r> = BTreeMap<&'r str, &'r RawValue>;

/// A value given for a field or element: its raw value, and the param and
/// the param's value it came from, when it's a param's.
struct Supplied<'a, 'r> {
    raw: Raw,
    by: Option<(&'a Param, &'r SampleValue)>,
}

/// What a count or length field's measuring keeps: the raw values it
/// takes, and the entries it works out ahead of their turn.
struct Ahead<'p, 'a, 'r> {
    used: &'p mut BTreeSet<&'a str>,
    plans: &'p mut BTreeMap<usize, Early<'r>>,
}

/// An entry worked out ahead of its turn: what it holds, whether it was
/// given a raw value, and what it took.
struct Early<'r> {
    plan: Planned,
    raw: bool,
    taken: Taken<'r>,
}

/// Where each param's values and each composite member stood, to see what
/// working an entry out took.
struct Marks<'r> {
    feeds: Vec<(&'r str, usize)>,
    unused: Vec<String>,
}

/// What working an entry out took: each param's values from where they
/// stood, and the composite members it used.
#[derive(Debug, Clone, Default)]
struct Taken<'r> {
    feeds: Vec<(&'r str, usize)>,
    members: Vec<String>,
}

/// What a count or length field measures from: the raw values given, and
/// whether params' values may give it, which they don't when the field is
/// given a value of its own.
#[derive(Clone, Copy)]
struct Counts<'g, 'r> {
    given: &'g Given<'r>,
    by_params: bool,
}

impl<'a, 'r> Planner<'a, 'r> {
    fn new(
        set: &'a Set,
        packet: &'a Packet,
        catalog: &'a Catalog,
        binary: bool,
        request: &'r TelemetryRequest,
    ) -> Result<Self, TelemetryError> {
        let mut mapped = BTreeSet::new();
        mapped_params(&packet.entries, &set.types, &mut mapped);
        let mut feeds = BTreeMap::new();
        for (name, values) in &request.params {
            let param_error = |problem| TelemetryError::Param {
                param: name.clone(),
                problem,
            };
            let composite = packet.composites.iter().any(|c| &c.canonical == name);
            if !composite && !mapped.contains(name.as_str()) {
                return Err(param_error(ParamProblem::Unknown {
                    packet: packet.name.clone(),
                }));
            }
            let feed = Feed {
                values: values.as_slice(),
                next: 0,
            };
            if feeds.insert(name.as_str(), feed).is_some() {
                return Err(param_error(ParamProblem::Duplicate));
            }
        }
        let mut planner = Planner {
            set,
            packet,
            catalog,
            binary,
            feeds,
            members: BTreeMap::new(),
        };
        planner.composites()?;
        Ok(planner)
    }

    /// Each composite's value, given as its param's, spread over its
    /// members.
    fn composites(&mut self) -> Result<(), TelemetryError> {
        for composite in &self.packet.composites {
            let name = composite.canonical.as_str();
            let Some(feed) = self.feeds.get_mut(name) else {
                continue;
            };
            if feed.values.len() != 1 {
                return Err(TelemetryError::Param {
                    param: name.to_string(),
                    problem: ParamProblem::Count {
                        given: feed.values.len(),
                        holds: 1,
                    },
                });
            }
            feed.next = 1;
            let value = &feed.values[0];
            let parts: Option<(Frame, Vec<f64>)> = match value {
                SampleValue::Vec3 { frame, x, y, z } if composite.members.len() == 3 => {
                    Some((*frame, vec![*x, *y, *z]))
                }
                SampleValue::Quat { frame, w, x, y, z } if composite.members.len() == 4 => {
                    Some((*frame, vec![*w, *x, *y, *z]))
                }
                _ => None,
            };
            let expected = if composite.members.len() == 3 {
                "a vec3"
            } else {
                "a quat"
            };
            let wrong = |given: String| TelemetryError::Entry {
                packet: self.packet.name.clone(),
                path: composite.members.join(","),
                problem: EntryProblem::WrongType { given, expected },
            };
            let Some((frame, components)) = parts else {
                return Err(wrong(describe(value)));
            };
            if frame != composite.frame {
                return Err(wrong(format!(
                    "{} in {}",
                    describe(value),
                    frame_name(frame)
                )));
            }
            let param_unit = self
                .catalog
                .params
                .get(name)
                .and_then(|param| param.unit.clone());
            for (index, (path, component)) in composite.members.iter().zip(components).enumerate() {
                if !component.is_finite() {
                    return Err(TelemetryError::Entry {
                        packet: self.packet.name.clone(),
                        path: path.clone(),
                        problem: EntryProblem::NonFinite {
                            given: show_double(component),
                        },
                    });
                }
                // Geodetic positions are latitude and longitude in degrees,
                // then altitude in the param's unit, as decoding reads them.
                let unit = if composite.frame == Frame::Geodetic && index < 2 {
                    Some("deg".to_string())
                } else {
                    param_unit.clone()
                };
                self.members.insert(
                    path.clone(),
                    Member {
                        param: name.to_string(),
                        value: component,
                        unit,
                        used: false,
                    },
                );
            }
        }
        Ok(())
    }

    /// Every param's values were taken, and every composite's members.
    fn finish(&self) -> Result<(), TelemetryError> {
        for (name, feed) in &self.feeds {
            if feed.remaining() > 0 {
                return Err(TelemetryError::Param {
                    param: (*name).to_string(),
                    problem: ParamProblem::Count {
                        given: feed.values.len(),
                        holds: feed.next,
                    },
                });
            }
        }
        if let Some(member) = self.members.values().find(|member| !member.used) {
            return Err(TelemetryError::Param {
                param: member.param.clone(),
                problem: ParamProblem::Count { given: 1, holds: 0 },
            });
        }
        Ok(())
    }

    fn entry_error(&self, path: &str, problem: impl Into<EntryProblem>) -> TelemetryError {
        TelemetryError::Entry {
            packet: self.packet.name.clone(),
            path: path.to_string(),
            problem: problem.into(),
        }
    }

    /// Each entry of a container, by entry index, from the raw values
    /// given for it; `fixed` at the packet's top level.
    fn container(
        &mut self,
        entries: &'a [Entry],
        prefix: &str,
        raw: &'r [(String, RawValue)],
        fixed: Option<&Fixed>,
    ) -> Result<Vec<Planned>, TelemetryError> {
        let mut given: Given<'r> = BTreeMap::new();
        for (name, value) in raw {
            if given.insert(name.as_str(), value).is_some() {
                return Err(self.entry_error(
                    &joined(prefix, name),
                    EntryProblem::Duplicate {
                        sources: "two raw values".to_string(),
                    },
                ));
            }
        }
        let mut used: BTreeSet<&str> = BTreeSet::new();
        let mut planned: Vec<Planned> = Vec::with_capacity(entries.len());
        let mut ahead: BTreeMap<usize, Early<'r>> = BTreeMap::new();
        for (index, entry) in entries.iter().enumerate() {
            let name = entry_name(entry);
            let path = name.map_or_else(|| prefix.to_string(), |name| joined(prefix, name));
            let raw_value = name.and_then(|name| given.get(name).copied());
            if let Some(name) = name
                && raw_value.is_some()
            {
                used.insert(name);
            }
            if let Some(condition) = when(entry) {
                let present = holds(condition, &|field: &str| {
                    value_of(entries, &planned, field)
                        .ok_or(Problem::Layout("a condition on a field the message holds"))
                })
                .map_err(|problem| self.entry_error(&path, decode_problem(problem)))?;
                if !present {
                    // Worked out ahead from params' values, it gives them
                    // back: they're left over, or go to a later entry.
                    let early_raw = match ahead.remove(&index) {
                        Some(early) if !early.raw => {
                            self.give_back(early.taken);
                            false
                        }
                        Some(_) => true,
                        None => false,
                    };
                    if raw_value.is_some() || early_raw {
                        return Err(self.entry_error(&path, EntryProblem::Absent));
                    }
                    planned.push(Planned::Absent);
                    continue;
                }
            }
            let plan = match entry {
                Entry::Padding(_) => Planned::Padding,
                Entry::Checksum(_) => match raw_value {
                    None => Planned::Checksum(None),
                    Some(RawValue::Integer(n)) => match u64::try_from(*n) {
                        Ok(n) => Planned::Checksum(Some(n)),
                        Err(_) => {
                            return Err(self.entry_error(
                                &path,
                                FieldProblem::RawRange {
                                    raw: n.to_string(),
                                    holds: "unsigned integers".to_string(),
                                },
                            ));
                        }
                    },
                    Some(other) => {
                        return Err(self.entry_error(
                            &path,
                            EntryProblem::WrongType {
                                given: raw_words(other),
                                expected: "an integer",
                            },
                        ));
                    }
                },
                Entry::Field(field) => {
                    if let Some(early) = ahead.remove(&index) {
                        early.plan
                    } else {
                        let fix = fixed
                            .and_then(|fixed| fixed.fields.get(&field.name))
                            .map(|(fix, what)| (fix.clone(), what.clone()));
                        // Inside an array's records, a param's values don't
                        // say which records hold an optional entry.
                        let param = field
                            .canonical
                            .as_deref()
                            .filter(|_| field.optional != Some(true) || !prefix.contains('['));
                        let own = self.given(field, &path, raw_value, param)?;
                        let measured = if self.binary {
                            let counts = Counts {
                                given: &given,
                                by_params: own.is_none(),
                            };
                            let ahead_of = Ahead {
                                used: &mut used,
                                plans: &mut ahead,
                            };
                            self.measure(entries, index, prefix, counts, ahead_of)?
                        } else {
                            None
                        };
                        let unset = fixed.and_then(|fixed| fixed.unset.get(&field.name));
                        match (self.field(field, &path, own, fix, measured), unset) {
                            (
                                Err(TelemetryError::Entry {
                                    problem: EntryProblem::Missing,
                                    ..
                                }),
                                Some(give),
                            ) => {
                                return Err(
                                    self.entry_error(&path, EntryProblem::UnsetClock { give })
                                );
                            }
                            (plan, _) => plan?,
                        }
                    }
                }
                Entry::Array(array) => {
                    if let Some(early) = ahead.remove(&index) {
                        early.plan
                    } else {
                        let counted = match &array.count {
                            Some(Count::Field(from)) if self.binary => {
                                Some(counted(entries, &planned, from).ok_or_else(|| {
                                    self.entry_error(
                                        &path,
                                        FieldProblem::Layout("a count from an earlier uint field"),
                                    )
                                })?)
                            }
                            _ => None,
                        };
                        self.array(array, &path, raw_value, counted)?
                    }
                }
                Entry::Struct(inner) => {
                    let record_type = self.struct_type(&inner.struct_type, &path)?;
                    let present = self.binary
                        || inner.optional != Some(true)
                        || raw_value.is_some()
                        || self.has_values(&record_type.entries, &path);
                    if present {
                        let inner_raw = match raw_value {
                            None => &[][..],
                            Some(RawValue::Record(entries)) => entries.as_slice(),
                            Some(other) => {
                                return Err(self.entry_error(
                                    &path,
                                    EntryProblem::WrongType {
                                        given: raw_words(other),
                                        expected: "a record",
                                    },
                                ));
                            }
                        };
                        Planned::Record(self.container(
                            &record_type.entries,
                            &path,
                            inner_raw,
                            None,
                        )?)
                    } else {
                        Planned::Absent
                    }
                }
            };
            planned.push(plan);
        }
        if let Some((name, _)) = raw.iter().find(|(name, _)| !used.contains(name.as_str())) {
            return Err(self.entry_error(&joined(prefix, name), EntryProblem::Unknown));
        }
        Ok(planned)
    }

    fn struct_type(&self, name: &str, path: &str) -> Result<&'a StructType, TelemetryError> {
        self.set.types.get(name).ok_or_else(|| {
            self.entry_error(path, FieldProblem::Layout("a struct type the set has"))
        })
    }

    /// Whether the request gives anything for an optional JSON struct's
    /// entries: a composite member among them, or, outside an array's
    /// records, a param they map that has values left. Inside an array's
    /// records a param's values don't say which records hold the struct.
    fn has_values(&self, entries: &[Entry], path: &str) -> bool {
        let mut mapped = BTreeSet::new();
        mapped_params(entries, &self.set.types, &mut mapped);
        let inside = format!("{path}.");
        let repeated = path.contains('[');
        !repeated
            && mapped.iter().any(|param| {
                self.feeds
                    .get(param)
                    .is_some_and(|feed| feed.remaining() > 0)
            })
            || self
                .members
                .keys()
                .any(|member| member.starts_with(&inside))
    }

    /// The raw value of a count or length field, from the entries it
    /// measures that the request gives values for, which are worked out
    /// here and kept for their own turn, with the params' values and
    /// composites' members they took, so an entry whose condition then
    /// doesn't hold gives them back. `None` when it measures nothing given.
    fn measure(
        &mut self,
        entries: &'a [Entry],
        index: usize,
        prefix: &str,
        counts: Counts<'_, 'r>,
        ahead_of: Ahead<'_, 'a, 'r>,
    ) -> Result<Option<(Value<'static>, String)>, TelemetryError> {
        let Ahead {
            used, plans: ahead, ..
        } = ahead_of;
        let given = counts.given;
        let Some(Entry::Field(field)) = entries.get(index) else {
            return Ok(None);
        };
        let mut found: Option<(Value<'static>, String)> = None;
        for (later, entry) in entries.iter().enumerate().skip(index + 1) {
            let (from, measured, noun) = match entry {
                Entry::Array(ArrayEntry {
                    count: Some(Count::Field(from)),
                    ..
                }) if from.field == field.name => (from, entry, "count"),
                Entry::Field(Field {
                    length: Some(ByteLength::Field(from)),
                    ..
                }) if from.field == field.name => (from, entry, "length"),
                _ => continue,
            };
            let Some(name) = entry_name(measured) else {
                continue;
            };
            let path = joined(prefix, name);
            let raw_value = given.get(name).copied();
            let marks = self.marks();
            let (plan, n) = match measured {
                Entry::Array(array) => {
                    let raw_list = match raw_value {
                        Some(RawValue::List(items)) => Some(items.len()),
                        _ => None,
                    };
                    let by_params = || {
                        if counts.by_params {
                            self.data_count(array, &path)
                        } else {
                            None
                        }
                    };
                    let Some(n) = raw_list.or_else(by_params) else {
                        continue;
                    };
                    (self.array(array, &path, raw_value, Some(n))?, n)
                }
                Entry::Field(measured_field) => {
                    let param = measured_field
                        .canonical
                        .as_deref()
                        .filter(|_| counts.by_params || raw_value.is_some());
                    let Some(raw) = self.given_value(measured_field, &path, raw_value, param)?
                    else {
                        continue;
                    };
                    let n = raw.bytes.len();
                    (Planned::Scalar(raw), n)
                }
                _ => continue,
            };
            if raw_value.is_some() {
                used.insert(name);
            }
            let taken = self.taken_since(&marks);
            ahead.insert(
                later,
                Early {
                    plan,
                    raw: raw_value.is_some(),
                    taken,
                },
            );
            let value = measure_raw(field, from, n, noun)
                .map_err(|problem| self.entry_error(&path, problem))?;
            let why = format!("the {noun} of {name}");
            match &found {
                Some((earlier, earlier_why)) if !same(earlier, &value) => {
                    return Err(self.entry_error(
                        &path,
                        EntryProblem::WrongConstant {
                            why: earlier_why.clone(),
                            holds: show_value(earlier),
                            given: format!("{} from its {noun}", show_value(&value)),
                        },
                    ));
                }
                Some(_) => {}
                None => found = Some((value, why)),
            }
        }
        Ok(found)
    }

    /// How many elements the request gives an array by its params, when
    /// the request gives none as raw values: the values its param has
    /// left, or for records, the values left of the first param a field of
    /// theirs maps.
    fn data_count(&self, array: &ArrayEntry, path: &str) -> Option<usize> {
        let feed = |name: &str| self.feeds.get(name).map(Feed::remaining);
        // Members of given composites at element `suffix` of the array,
        // whose params take nothing there.
        let members = |suffix: &str| {
            let elements = format!("{path}[");
            self.members
                .keys()
                .filter_map(|member| member.strip_prefix(&elements))
                .filter(|rest| {
                    rest.split_once(']').is_some_and(|(index, after)| {
                        !index.is_empty()
                            && index.bytes().all(|b| b.is_ascii_digit())
                            && after == suffix
                    })
                })
                .count()
        };
        // Inside a repeated record a param's values don't say which are
        // this record's.
        let repeated = path.contains('[');
        let given = |param: Option<&str>, suffix: &str| {
            let members = members(suffix);
            match param.filter(|_| !repeated).and_then(feed) {
                Some(remaining) => Some(remaining + members),
                None if members > 0 => Some(members),
                None => None,
            }
        };
        match &array.element {
            Element::Scalar(_) => given(array.canonical.as_deref(), ""),
            Element::Struct { name } => self.set.types.get(name).and_then(|record| {
                record.entries.iter().find_map(|entry| match entry {
                    Entry::Field(field) => {
                        given(field.canonical.as_deref(), &format!(".{}", field.name))
                    }
                    _ => None,
                })
            }),
        }
    }

    /// The param an array's count could come from: its elements', or the
    /// first its records' fields map.
    fn counting_param(&self, array: &ArrayEntry) -> Option<String> {
        match &array.element {
            Element::Scalar(_) => array.canonical.clone(),
            Element::Struct { name } => self.set.types.get(name).and_then(|record| {
                record.entries.iter().find_map(|entry| match entry {
                    Entry::Field(field) => field.canonical.clone(),
                    _ => None,
                })
            }),
        }
    }

    /// A field's raw value: given (raw, by param or as a composite's
    /// member), or fixed by the definitions, or measured from what it
    /// counts; given and fixed must agree.
    fn field(
        &mut self,
        field: &'a Field,
        path: &str,
        given: Option<Supplied<'a, 'r>>,
        fix: Option<(Fix, String)>,
        measured: Option<(Value<'static>, String)>,
    ) -> Result<Planned, TelemetryError> {
        let fixed = match fix {
            Some((fix, why)) => Some((
                fix_value(field, &fix, self.binary)
                    .map_err(|problem| self.entry_error(path, problem))?,
                why,
            )),
            None => None,
        };
        let determined = match (fixed, measured) {
            (Some((a, why_a)), Some((b, why_b))) => {
                if !same(&a, &b) {
                    return Err(self.entry_error(
                        path,
                        EntryProblem::WrongConstant {
                            why: why_a,
                            holds: show_value(&a),
                            given: format!("{} as {why_b}", show_value(&b)),
                        },
                    ));
                }
                Some((a, why_a))
            }
            (one, other) => one.or(other),
        };
        match (given, determined) {
            (Some(supplied), Some((value, _))) if same(&supplied.raw.value, &value) => {
                Ok(Planned::Scalar(supplied.raw))
            }
            // A param's value agrees with a raw value fixed otherwise when
            // decoding that raw value gives it: a bool param reads any
            // integer but 0 as true.
            (
                Some(Supplied {
                    by: Some((param, sample)),
                    ..
                }),
                Some((value, _)),
            ) if good_value(param, field, &value).as_ref() == Some(sample) => {
                let bytes = self.bytes_of(field, &value, path)?;
                Ok(Planned::Scalar(Raw { value, bytes }))
            }
            (Some(supplied), Some((value, why))) => {
                let given = match supplied.by {
                    Some((_, sample)) => format!("{} by param", describe(sample)),
                    None => show_value(&supplied.raw.value),
                };
                Err(self.entry_error(
                    path,
                    EntryProblem::WrongConstant {
                        why,
                        holds: show_value(&value),
                        given,
                    },
                ))
            }
            (Some(supplied), None) => Ok(Planned::Scalar(supplied.raw)),
            (None, Some((value, _))) => {
                let bytes = self.bytes_of(field, &value, path)?;
                Ok(Planned::Scalar(Raw { value, bytes }))
            }
            (None, None) if !self.binary && field.optional == Some(true) => Ok(Planned::Absent),
            (None, None) => Err(self.entry_error(path, EntryProblem::Missing)),
        }
    }

    fn bytes_of(
        &self,
        field: &Field,
        value: &Value<'_>,
        path: &str,
    ) -> Result<Vec<u8>, TelemetryError> {
        if self.binary {
            run_bytes(field, value).map_err(|problem| self.entry_error(path, problem))
        } else {
            Ok(Vec::new())
        }
    }

    /// The value the request gives a field or scalar element: raw, as a
    /// member of a composite the request gives, or by the param it maps,
    /// which gives its next value only to an entry given neither of the
    /// others. A raw value and a composite's member is one too many.
    fn given_value(
        &mut self,
        field: &Field,
        path: &str,
        raw: Option<&RawValue>,
        param: Option<&str>,
    ) -> Result<Option<Raw>, TelemetryError> {
        Ok(self
            .given(field, path, raw, param)?
            .map(|supplied| supplied.raw))
    }

    /// As [`given_value`](Self::given_value), with the param and its value
    /// when the value is a param's.
    fn given(
        &mut self,
        field: &Field,
        path: &str,
        raw: Option<&RawValue>,
        param: Option<&str>,
    ) -> Result<Option<Supplied<'a, 'r>>, TelemetryError> {
        let member = self.members.get_mut(path).map(|member| {
            member.used = true;
            (member.param.clone(), member.value, member.unit.clone())
        });
        // A param's values go, in record order, to the entries that map it
        // and take no other value.
        let by_param: Option<(&str, &'r SampleValue)> = match (raw, &member) {
            (Some(_), _) | (_, Some(_)) => None,
            (None, None) => param.and_then(|name| {
                let feed = self.feeds.get_mut(name)?;
                let value = feed.values.get(feed.next)?;
                feed.next += 1;
                Some((name, value))
            }),
        };
        let sources: Vec<String> = [
            raw.map(|_| "a raw value".to_string()),
            by_param.map(|(name, _)| format!("param {name}'s")),
            member
                .as_ref()
                .map(|(name, _, _)| format!("composite {name}'s member")),
        ]
        .into_iter()
        .flatten()
        .collect();
        if sources.len() > 1 {
            return Err(self.entry_error(
                path,
                EntryProblem::Duplicate {
                    sources: sources.join(" and "),
                },
            ));
        }
        let mut by = None;
        let value = if let Some(raw) = raw {
            raw_field(field, raw, self.binary)
        } else if let Some((name, sample)) = by_param {
            let param = self.catalog.params.get(name).ok_or_else(|| {
                self.entry_error(path, FieldProblem::Layout("a param the catalog has"))
            })?;
            by = Some((param, sample));
            param_raw(field, name, param, sample, self.binary)
        } else if let Some((name, value, unit)) = member {
            number_raw(field, &name, value, unit.as_deref(), self.binary)
        } else {
            return Ok(None);
        }
        .map_err(|problem| self.entry_error(path, problem))?;
        // A raw value given may be a sentinel, which is how a sender says
        // it has no reading; a param's value may not become one.
        if raw.is_none() && is_sentinel(&field.invalid, &value) {
            return Err(self.entry_error(
                path,
                EntryProblem::InvalidRaw {
                    raw: show_value(&value),
                },
            ));
        }
        let bytes = self.bytes_of(field, &value, path)?;
        Ok(Some(Supplied {
            raw: Raw { value, bytes },
            by,
        }))
    }

    /// An array's elements: as many as its count says, or as the request
    /// gives.
    fn array(
        &mut self,
        array: &'a ArrayEntry,
        path: &str,
        raw: Option<&'r RawValue>,
        counted: Option<usize>,
    ) -> Result<Planned, TelemetryError> {
        let items = match raw {
            None => None,
            Some(RawValue::List(items)) => Some(items.as_slice()),
            Some(other) => {
                return Err(self.entry_error(
                    path,
                    EntryProblem::WrongType {
                        given: raw_words(other),
                        expected: "a list",
                    },
                ));
            }
        };
        let fixed = match &array.count {
            Some(Count::Fixed { count }) if self.binary => usize::try_from(*count).ok(),
            _ => None,
        };
        let known = counted.or(fixed);
        // Records of padding alone take no values, so only a raw list, whose
        // length the request holds, or the definitions say how many there
        // are: a count's value alone would make as many as it says.
        if let Element::Struct { name } = &array.element
            && items.is_none()
            && fixed.is_none()
            && counted.is_some_and(|n| n > 0)
            && holds_no_values(&self.struct_type(name, path)?.entries, &self.set.types)
        {
            return Err(self.entry_error(path, EntryProblem::EmptyRecords));
        }
        let count = match (known, items) {
            (Some(n), Some(items)) if items.len() != n => {
                return Err(self.entry_error(
                    path,
                    FieldProblem::BadCount {
                        count: items.len(),
                        reason: format!("the message holds {} here", values_of(n)),
                    },
                ));
            }
            (Some(n), _) => n,
            (None, Some(items)) => items.len(),
            (None, None) => match self.data_count(array, path) {
                Some(n) => n,
                None if !self.binary && array.optional == Some(true) => {
                    return Ok(Planned::Absent);
                }
                None => {
                    let fed = self.counting_param(array).filter(|param| {
                        self.feeds
                            .get(param.as_str())
                            .is_some_and(|feed| feed.remaining() > 0)
                    });
                    let problem = match fed {
                        Some(param) => EntryProblem::Uncounted { param },
                        None => EntryProblem::Missing,
                    };
                    return Err(self.entry_error(path, problem));
                }
            },
        };
        let mut elements = Vec::with_capacity(count.min(4096));
        match &array.element {
            Element::Scalar(element) => {
                let field = element.as_field(&array.name);
                for index in 0..count {
                    let item_path = format!("{path}[{index}]");
                    let item = items.and_then(|items| items.get(index));
                    if let Some(RawValue::Null) = item {
                        if self.binary {
                            return Err(self.entry_error(
                                &item_path,
                                EntryProblem::WrongType {
                                    given: "null".to_string(),
                                    expected: "a value, as a binary payload holds every element",
                                },
                            ));
                        }
                        elements.push(Planned::Null);
                        continue;
                    }
                    match self.given_value(&field, &item_path, item, array.canonical.as_deref())? {
                        Some(raw) => elements.push(Planned::Scalar(raw)),
                        None => return Err(self.entry_error(&item_path, EntryProblem::Missing)),
                    }
                }
            }
            Element::Struct { name } => {
                let record_type = self.struct_type(name, path)?;
                for index in 0..count {
                    let item_path = format!("{path}[{index}]");
                    let inner_raw = match items.and_then(|items| items.get(index)) {
                        None => &[][..],
                        Some(RawValue::Record(entries)) => entries.as_slice(),
                        Some(RawValue::Null) if !self.binary => {
                            elements.push(Planned::Null);
                            continue;
                        }
                        Some(other) => {
                            return Err(self.entry_error(
                                &item_path,
                                EntryProblem::WrongType {
                                    given: raw_words(other),
                                    expected: "a record",
                                },
                            ));
                        }
                    };
                    elements.push(Planned::Record(self.container(
                        &record_type.entries,
                        &item_path,
                        inner_raw,
                        None,
                    )?));
                }
            }
        }
        Ok(Planned::Array(elements))
    }
}

/// Every param the entries map, through struct entries and the records of
/// arrays.
fn mapped_params<'a>(
    entries: &'a [Entry],
    types: &'a BTreeMap<String, StructType>,
    mapped: &mut BTreeSet<&'a str>,
) {
    for entry in entries {
        match entry {
            Entry::Field(field) => mapped.extend(field.canonical.as_deref()),
            Entry::Array(array) => {
                mapped.extend(array.canonical.as_deref());
                if let Element::Struct { name } = &array.element
                    && let Some(record) = types.get(name)
                {
                    mapped_params(&record.entries, types, mapped);
                }
            }
            Entry::Struct(inner) => {
                if let Some(record) = types.get(&inner.struct_type) {
                    mapped_params(&record.entries, types, mapped);
                }
            }
            Entry::Padding(_) | Entry::Checksum(_) => {}
        }
    }
}

/// Whether a struct type's entries hold nothing a request gives: padding,
/// structs of padding, and arrays of a fixed count of such records, or of
/// none (a count from a field needs that field's value). Validation bounds
/// the nesting.
fn holds_no_values(entries: &[Entry], types: &BTreeMap<String, StructType>) -> bool {
    let empty = |name: &str| {
        types
            .get(name)
            .is_some_and(|record| holds_no_values(&record.entries, types))
    };
    entries.iter().all(|entry| match entry {
        Entry::Padding(_) => true,
        Entry::Struct(inner) => empty(&inner.struct_type),
        Entry::Array(array) => match array.count {
            Some(Count::Fixed { count: 0 }) => true,
            Some(Count::Fixed { .. }) => {
                matches!(&array.element, Element::Struct { name } if empty(name))
            }
            _ => false,
        },
        _ => false,
    })
}

fn entry_name(entry: &Entry) -> Option<&str> {
    match entry {
        Entry::Field(field) => Some(&field.name),
        Entry::Array(array) => Some(&array.name),
        Entry::Struct(inner) => Some(&inner.name),
        Entry::Checksum(checksum) => Some(&checksum.name),
        Entry::Padding(_) => None,
    }
}

fn joined(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}.{name}")
    }
}

/// The raw value of the container's field `name`, if the message holds it.
fn value_of<'p>(
    entries: &[Entry],
    planned: &'p [Planned],
    name: &str,
) -> Option<&'p Value<'static>> {
    let index = entries.iter().position(|entry| match entry {
        Entry::Field(field) => field.name == name,
        _ => false,
    })?;
    match planned.get(index) {
        Some(Planned::Scalar(raw)) => Some(&raw.value),
        _ => None,
    }
}

/// An array's count from its count field's raw value: `raw * multiply +
/// add`, as decoding reads it.
fn counted(entries: &[Entry], planned: &[Planned], from: &FromField) -> Option<usize> {
    let Value::Uint(raw) = value_of(entries, planned, &from.field)? else {
        return None;
    };
    let value = i128::from(*raw) * i128::from(from.multiply.unwrap_or(1))
        + i128::from(from.add.unwrap_or(0));
    usize::try_from(value).ok()
}

/// A count or length field's raw value for `n`: `(n - add) / multiply`,
/// which must be a whole number of at least 0 that the field holds.
fn measure_raw(
    field: &Field,
    from: &FromField,
    n: usize,
    noun: &str,
) -> Result<Value<'static>, FieldProblem> {
    let bad = |reason: String| match noun {
        "count" => FieldProblem::BadCount { count: n, reason },
        _ => FieldProblem::BadLength { length: n, reason },
    };
    let multiply = i128::from(from.multiply.unwrap_or(1));
    let add = i128::from(from.add.unwrap_or(0));
    let stored = i128::try_from(n).map_or(-1, |n| n - add);
    if multiply < 1 || stored < 0 || stored % multiply != 0 {
        return Err(bad(format!(
            "{} holds ({noun} - {add}) / {multiply}, which must be a whole number of at least 0",
            from.field
        )));
    }
    integer_raw(field, stored / multiply, true).map_err(|problem| match problem {
        FieldProblem::RawRange { holds, .. } => bad(format!("{} holds {holds}", from.field)),
        problem => problem,
    })
}

/// A fixed value as a field's raw value.
fn fix_value(field: &Field, fix: &Fix, binary: bool) -> Result<Value<'static>, FieldProblem> {
    match (field.ty, fix) {
        (ScalarType::Uint | ScalarType::Int, Fix::Int(n)) => integer_raw(field, *n, binary),
        (ScalarType::Uint | ScalarType::Int, Fix::Text(text)) => {
            let n = NativeId::from_text(text.clone())
                .as_int()
                .ok_or(FieldProblem::Layout("an integer for an integer field"))?;
            integer_raw(field, n, binary)
        }
        (ScalarType::Float, Fix::Float(x)) => float_raw(field, *x, binary),
        (ScalarType::Bool, Fix::Int(n @ (0 | 1))) => Ok(Value::Bool(*n == 1)),
        (ScalarType::String, Fix::Int(n)) => Ok(Value::Str(n.to_string())),
        (ScalarType::String, Fix::Text(text)) => Ok(Value::Str(text.clone())),
        _ => Err(FieldProblem::Layout("a fixed value its field holds")),
    }
}

/// A raw value given for a field or scalar element.
fn raw_field(field: &Field, raw: &RawValue, binary: bool) -> Result<Value<'static>, EntryProblem> {
    let wrong = |expected: &'static str| EntryProblem::WrongType {
        given: raw_words(raw),
        expected,
    };
    match (field.ty, raw) {
        (ScalarType::Uint | ScalarType::Int, RawValue::Integer(n)) => {
            Ok(integer_raw(field, *n, binary)?)
        }
        (ScalarType::Uint | ScalarType::Int, _) => Err(wrong("an integer")),
        (ScalarType::Float, RawValue::Float(x)) => raw_float(field, *x, binary),
        (ScalarType::Float, RawValue::Integer(n)) => {
            let x = *n as f64;
            if x.is_finite() && x as i128 == *n {
                raw_float(field, x, binary)
            } else {
                Err(wrong("a number a float holds exactly"))
            }
        }
        (ScalarType::Float, _) => Err(wrong("a number")),
        (ScalarType::Bool, RawValue::Bool(b)) => Ok(Value::Bool(*b)),
        (ScalarType::Bool, _) => Err(wrong("a bool")),
        (ScalarType::String, RawValue::Text(text)) => Ok(Value::Str(text.clone())),
        (ScalarType::String, _) => Err(wrong("text")),
        (ScalarType::Bytes, RawValue::Bytes(bytes)) => Ok(Value::Bytes(bytes.clone())),
        (ScalarType::Bytes, _) => Err(wrong("bytes")),
    }
}

/// A raw float: any binary64 or binary32 value in a binary payload, NaN
/// and the infinities included; a finite one in a JSON payload.
fn raw_float(field: &Field, x: f64, binary: bool) -> Result<Value<'static>, EntryProblem> {
    if binary && x.is_nan() {
        return Ok(Value::Float(match field.size_bits {
            Some(32) => f64::from(x as f32),
            _ => x,
        }));
    }
    if binary && x.is_infinite() {
        return Ok(Value::Float(x));
    }
    Ok(float_raw(field, x, binary)?)
}

/// A param's value as a field's raw value, by the param's type.
fn param_raw(
    field: &Field,
    name: &str,
    param: &Param,
    value: &SampleValue,
    binary: bool,
) -> Result<Value<'static>, EntryProblem> {
    let wrong = |expected: &'static str| EntryProblem::WrongType {
        given: describe(value),
        expected,
    };
    match param.ty {
        ParamType::F64 => {
            let x = match value {
                SampleValue::F64(x) => *x,
                SampleValue::I64(n) => exact_double(i128::from(*n)).ok_or(wrong("a number"))?,
                SampleValue::U64(n) => exact_double(i128::from(*n)).ok_or(wrong("a number"))?,
                _ => return Err(wrong("a number")),
            };
            number_raw(field, name, x, param.unit.as_deref(), binary)
        }
        ParamType::I64 | ParamType::U64 => {
            let n = match value {
                SampleValue::I64(n) => i128::from(*n),
                SampleValue::U64(n) => i128::from(*n),
                _ => return Err(wrong("an integer")),
            };
            let fits = match param.ty {
                ParamType::I64 => i64::try_from(n).is_ok(),
                _ => u64::try_from(n).is_ok(),
            };
            if !fits {
                return Err(wrong(if param.ty == ParamType::I64 {
                    "an i64"
                } else {
                    "a u64"
                }));
            }
            Ok(integer_raw(field, n, binary)?)
        }
        ParamType::Bool => {
            let SampleValue::Bool(b) = value else {
                return Err(wrong("a bool"));
            };
            match field.ty {
                ScalarType::Bool => Ok(Value::Bool(*b)),
                _ => Ok(integer_raw(field, i128::from(*b), binary)?),
            }
        }
        ParamType::Str => match value {
            SampleValue::Str(text) => Ok(Value::Str(text.clone())),
            _ => Err(wrong("text")),
        },
        ParamType::Bytes => match value {
            SampleValue::Bytes(bytes) => Ok(Value::Bytes(bytes.clone())),
            _ => Err(wrong("bytes")),
        },
        ParamType::Enum => {
            let state = match value {
                SampleValue::Enum(state) | SampleValue::Str(state) => state,
                _ => return Err(wrong("one of its states")),
            };
            if !param.states.contains(state) {
                return Err(EntryProblem::UnknownState {
                    given: state.clone(),
                    among: format!("one of param {name}'s states"),
                });
            }
            match (&field.conversion, field.ty) {
                (Some(Conversion::States { states }), _) => {
                    let raw = states
                        .iter()
                        .find(|candidate| &candidate.name == state)
                        .and_then(|found| integer_value(&found.raw))
                        .ok_or_else(|| EntryProblem::UnknownState {
                            given: state.clone(),
                            among: "one of the states its field lists".to_string(),
                        })?;
                    Ok(integer_raw(field, raw, binary)?)
                }
                (None, ScalarType::String) => Ok(Value::Str(state.clone())),
                _ => Err(FieldProblem::Layout("an enum field validation accepts").into()),
            }
        }
        ParamType::Vec3 | ParamType::Quat => {
            Err(FieldProblem::Layout("a vector, which a composite maps").into())
        }
    }
}

/// A number in `unit` as a field's raw value: into the field's unit,
/// through the inverse of its conversion, then rounded and fit as encoding
/// a command's argument does (`spec/encoding.md` §3.1).
fn number_raw(
    field: &Field,
    name: &str,
    x: f64,
    unit: Option<&str>,
    binary: bool,
) -> Result<Value<'static>, EntryProblem> {
    if !x.is_finite() {
        return Err(EntryProblem::NonFinite {
            given: show_double(x),
        });
    }
    let in_field = convert_between(x, unit, field.unit.as_deref())
        .ok_or(FieldProblem::Layout("a unit that converts to its param's"))?;
    let raw = match inverse(field, in_field) {
        Converted::Number(raw) => raw,
        Converted::Outside => {
            return Err(EntryProblem::Outside {
                given: show_double(x),
            });
        }
        Converted::NotNumeric => {
            return Err(EntryProblem::NoInverse {
                param: name.to_string(),
            });
        }
    };
    match field.ty {
        ScalarType::Uint | ScalarType::Int => match rounded(raw) {
            Some(n) => Ok(integer_raw(field, n, binary)?),
            None => Err(FieldProblem::RawRange {
                raw: show_double(raw),
                holds: "integers".to_string(),
            }
            .into()),
        },
        ScalarType::Float => Ok(float_raw(field, raw, binary)?),
        _ => Err(FieldProblem::Layout("a number field").into()),
    }
}

/// An integer as a double, if the double is exactly it.
fn exact_double(n: i128) -> Option<f64> {
    let x = n as f64;
    (x as i128 == n).then_some(x)
}

fn decode_problem(problem: Problem) -> EntryProblem {
    match problem {
        Problem::Layout(what) => EntryProblem::Field(FieldProblem::Layout(what)),
        _ => EntryProblem::Field(FieldProblem::Layout("a condition decoding can read")),
    }
}

fn describe(value: &SampleValue) -> String {
    match value {
        SampleValue::F64(x) => show_double(*x),
        SampleValue::I64(n) => n.to_string(),
        SampleValue::U64(n) => n.to_string(),
        SampleValue::Bool(b) => b.to_string(),
        SampleValue::Str(text) | SampleValue::Enum(text) => format!("{text:?}"),
        SampleValue::Bytes(bytes) => format!("{} bytes", bytes.len()),
        SampleValue::Vec3 { .. } => "a vec3".to_string(),
        SampleValue::Quat { .. } => "a quat".to_string(),
    }
}

fn frame_name(frame: Frame) -> String {
    serde_json::to_value(frame)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn raw_words(raw: &RawValue) -> String {
    match raw {
        RawValue::Integer(n) => n.to_string(),
        RawValue::Float(x) => show_double(*x),
        RawValue::Bool(b) => b.to_string(),
        RawValue::Text(text) => format!("{text:?}"),
        RawValue::Bytes(bytes) => format!("{} bytes", bytes.len()),
        RawValue::List(_) => "a list".to_string(),
        RawValue::Record(_) => "a record".to_string(),
        RawValue::Null => "null".to_string(),
    }
}

fn show_value(value: &Value<'_>) -> String {
    match value {
        Value::Uint(n) => n.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(x) => show_double(*x),
        Value::Bool(b) => b.to_string(),
        Value::Str(text) => format!("{text:?}"),
        Value::Bytes(bytes) => format!("{} bytes", bytes.len()),
        _ => "a value".to_string(),
    }
}

/// Raw values compared as the payload holds them: floats by their bits.
fn same(a: &Value<'_>, b: &Value<'_>) -> bool {
    match (a, b) {
        (Value::Float(x), Value::Float(y)) => x.to_bits() == y.to_bits(),
        (Value::Array(xs), Value::Array(ys)) => {
            xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| same(x, y))
        }
        (Value::Record(x), Value::Record(y)) => first_difference(x, y, "").is_none(),
        _ => a == b,
    }
}

/// The path of the first entry two records differ at.
fn first_difference(a: &Record<'_>, b: &Record<'_>, prefix: &str) -> Option<String> {
    let mut left = a.iter();
    let mut right = b.iter();
    loop {
        match (left.next(), right.next()) {
            (None, None) => return None,
            (Some((name, x)), Some((other, y))) if name == other => {
                let path = joined(prefix, name);
                match (x, y) {
                    (Value::Record(x), Value::Record(y)) => {
                        if let Some(found) = first_difference(x, y, &path) {
                            return Some(found);
                        }
                    }
                    _ if !same(x, y) => return Some(path),
                    _ => {}
                }
            }
            (Some((name, _)), _) | (None, Some((name, _))) => return Some(joined(prefix, name)),
        }
    }
}

// ---- Binary layout ------------------------------------------------------------

/// A binary packet being laid out.
struct BinaryOut<'t> {
    types: &'t BTreeMap<String, StructType>,
    order: ByteOrder,
    payload: Vec<u8>,
    /// Top-level checksums: position and entry index.
    checksums: Vec<(u64, usize)>,
    /// Entries that run to the packet's end: their path, start and end.
    to_end: Vec<(String, u64, u64)>,
    /// Each checksum's value, by entry index.
    computed: BTreeMap<usize, u64>,
}

type Failed = (String, FieldProblem);

impl<'t> BinaryOut<'t> {
    fn new(types: &'t BTreeMap<String, StructType>, order: ByteOrder) -> Self {
        BinaryOut {
            types,
            order,
            payload: Vec::new(),
            checksums: Vec::new(),
            to_end: Vec::new(),
            computed: BTreeMap::new(),
        }
    }

    /// The packet: its entries where decoding finds them, its length, the
    /// locators' bits, then its checksums.
    fn lay(&mut self, packet: &Packet, planned: &[Planned], fixed: &Fixed) -> Result<(), Failed> {
        let end = self.container(&packet.entries, planned, 0, "", true)?;
        let mut bits = end;
        for run in &fixed.bits {
            bits = bits.max(run.offset + u64::from(run.size));
        }
        match &packet.length {
            Some(Length::Exact { bytes }) => {
                let exact = u64::from(*bytes) * 8;
                if end > exact {
                    return Err((
                        String::new(),
                        FieldProblem::BadLength {
                            length: usize::try_from(end.div_ceil(8)).unwrap_or(usize::MAX),
                            reason: format!("packet {} is exactly {bytes} bytes", packet.name),
                        },
                    ));
                }
                bits = bits.max(exact);
            }
            Some(Length::AtLeast { bytes }) => bits = bits.max(u64::from(*bytes) * 8),
            _ => {}
        }
        self.reach(bits, "")?;
        let length = self.payload.len() as u64 * 8;
        for (path, start, entry_end) in &self.to_end {
            if *entry_end != length {
                return Err((
                    path.clone(),
                    FieldProblem::BadLength {
                        length: usize::try_from((entry_end - start) / 8).unwrap_or(usize::MAX),
                        reason: format!(
                            "it takes the rest of the packet, which is {} bytes from its start here",
                            (length - start.min(&length)) / 8
                        ),
                    },
                ));
            }
        }
        for run in &fixed.bits {
            let order = run.order.unwrap_or(self.order);
            write_bits(&mut self.payload, run.offset, run.size, run.value, order).map_err(
                |_| {
                    (
                        run.what.clone(),
                        FieldProblem::Layout("locator bits inside the packet"),
                    )
                },
            )?;
        }
        self.checksums.sort_unstable();
        let checksums = std::mem::take(&mut self.checksums);
        for (at, index) in checksums {
            let Some(Entry::Checksum(checksum)) = packet.entries.get(index) else {
                continue;
            };
            let fail = |problem| (checksum.name.clone(), problem);
            let algorithm = Algorithm::resolve(&checksum.algorithm).ok_or_else(|| {
                fail(FieldProblem::Layout(
                    "a checksum whose parameters fit its width",
                ))
            })?;
            let from = u64::from(checksum.from_bits.unwrap_or(0));
            let covered = match (usize::try_from(from / 8), usize::try_from(at / 8)) {
                (Ok(first), Ok(stop))
                    if from.is_multiple_of(8) && at.is_multiple_of(8) && first <= stop =>
                {
                    self.payload.get(first..stop)
                }
                _ => None,
            }
            .ok_or_else(|| {
                fail(FieldProblem::Layout(
                    "a checksum over whole bytes before it",
                ))
            })?;
            let value = algorithm.compute(covered);
            write_bits(
                &mut self.payload,
                at,
                checksum.algorithm.width_bits(),
                value,
                algorithm.order(self.order),
            )
            .map_err(|_| fail(FieldProblem::Layout("a checksum inside the packet")))?;
            self.computed.insert(index, value);
        }
        Ok(())
    }

    /// Grows the payload to hold `bits`.
    fn reach(&mut self, bits: u64, path: &str) -> Result<(), Failed> {
        let bytes = usize::try_from(bits.div_ceil(8)).map_err(|_| {
            (
                path.to_string(),
                FieldProblem::Layout("a packet that fits in memory"),
            )
        })?;
        if self.payload.len() < bytes {
            self.payload.resize(bytes, 0);
        }
        Ok(())
    }

    /// A container's entries from bit `start`, as decoding reads them: at
    /// an entry's offset from the start, or where the previous one ended
    /// (an absent one ending where it would have started). Its end is the
    /// furthest end of its present entries.
    fn container(
        &mut self,
        entries: &[Entry],
        planned: &[Planned],
        start: u64,
        prefix: &str,
        top: bool,
    ) -> Result<u64, Failed> {
        let mut cursor = start;
        let mut furthest = start;
        for (index, entry) in entries.iter().enumerate() {
            let path = entry_name(entry).map_or_else(|| prefix.to_string(), |n| joined(prefix, n));
            let past = || {
                (
                    path.clone(),
                    FieldProblem::Layout("a packet under 2^64 bits"),
                )
            };
            let at = match offset_of(entry) {
                Some(offset) => start.checked_add(u64::from(offset)).ok_or_else(past)?,
                None => cursor,
            };
            let plan = planned.get(index).unwrap_or(&Planned::Absent);
            if matches!(plan, Planned::Absent) {
                cursor = at;
                continue;
            }
            let end = match (entry, plan) {
                (Entry::Padding(padding), _) => at
                    .checked_add(u64::from(padding.size_bits))
                    .ok_or_else(past)?,
                (Entry::Field(field), Planned::Scalar(raw)) => {
                    self.scalar(field, raw, at, &path)?
                }
                (Entry::Array(array), Planned::Array(elements)) => {
                    let end = self.array(array, elements, at, &path)?;
                    if matches!(array.count, Some(Count::Remaining {})) {
                        self.to_end.push((path.clone(), at, end));
                    }
                    end
                }
                (Entry::Struct(inner), Planned::Record(inner_planned)) => {
                    let record_type = self.types.get(&inner.struct_type).ok_or_else(|| {
                        (
                            path.clone(),
                            FieldProblem::Layout("a struct type the set has"),
                        )
                    })?;
                    self.container(&record_type.entries, inner_planned, at, &path, false)?
                }
                (Entry::Checksum(checksum), Planned::Checksum(_)) if top => {
                    self.checksums.push((at, index));
                    let end = at
                        .checked_add(u64::from(checksum.algorithm.width_bits()))
                        .ok_or_else(past)?;
                    self.reach(end, &path)?;
                    end
                }
                _ => {
                    return Err((
                        path,
                        FieldProblem::Layout("an entry with a value of its kind"),
                    ));
                }
            };
            cursor = end;
            furthest = furthest.max(end);
        }
        Ok(furthest)
    }

    /// A field's or element's raw value at `at`; its end.
    fn scalar(&mut self, field: &Field, raw: &Raw, at: u64, path: &str) -> Result<u64, Failed> {
        let size = match (field.ty, &field.length) {
            (ScalarType::String | ScalarType::Bytes, Some(ByteLength::Fixed { bytes, .. })) => {
                u64::from(*bytes) * 8
            }
            (ScalarType::String | ScalarType::Bytes, length) => {
                let terminator = matches!(length, Some(ByteLength::Terminator { .. }));
                (raw.bytes.len() as u64 + u64::from(terminator)) * 8
            }
            _ => u64::from(field.size_bits.unwrap_or(0)),
        };
        let end = at.checked_add(size).ok_or_else(|| {
            (
                path.to_string(),
                FieldProblem::Layout("a packet under 2^64 bits"),
            )
        })?;
        self.reach(end, path)?;
        let order = field.byte_order.unwrap_or(self.order);
        let form = IntForm::of(field.ty, field.signed);
        write_field(
            &mut self.payload,
            raw,
            at,
            size,
            field.length.as_ref(),
            form,
            order,
        )
        .map_err(|problem| (path.to_string(), problem))?;
        if matches!(field.length, Some(ByteLength::Remaining {})) {
            self.to_end.push((path.to_string(), at, end));
        }
        Ok(end)
    }

    /// An array's elements from `at`, one after the other; its end.
    fn array(
        &mut self,
        array: &ArrayEntry,
        elements: &[Planned],
        at: u64,
        path: &str,
    ) -> Result<u64, Failed> {
        let mut cursor = at;
        for (index, element) in elements.iter().enumerate() {
            let item_path = format!("{path}[{index}]");
            let end = match (&array.element, element) {
                (Element::Scalar(scalar), Planned::Scalar(raw)) => {
                    self.scalar(&scalar.as_field(&array.name), raw, cursor, &item_path)?
                }
                (Element::Struct { name }, Planned::Record(inner)) => {
                    let record_type = self.types.get(name).ok_or_else(|| {
                        (
                            item_path.clone(),
                            FieldProblem::Layout("a struct type the set has"),
                        )
                    })?;
                    self.container(&record_type.entries, inner, cursor, &item_path, false)?
                }
                _ => {
                    return Err((
                        item_path,
                        FieldProblem::Layout("elements of the array's kind"),
                    ));
                }
            };
            if end == cursor {
                return Err((
                    item_path,
                    FieldProblem::Layout("elements that take at least one bit"),
                ));
            }
            cursor = end;
        }
        Ok(cursor)
    }
}

fn offset_of(entry: &Entry) -> Option<u32> {
    match entry {
        Entry::Field(field) => field.offset_bits,
        Entry::Array(array) => array.offset_bits,
        Entry::Struct(inner) => inner.offset_bits,
        Entry::Padding(padding) => padding.offset_bits,
        Entry::Checksum(checksum) => checksum.offset_bits,
    }
}

// ---- JSON layout --------------------------------------------------------------

/// The packet as a JSON document: each entry at its pointer, then the
/// values locators and identify keys read where no field holds them.
fn json_payload(
    packet: &Packet,
    types: &BTreeMap<String, StructType>,
    planned: &[Planned],
    fixed: &Fixed,
) -> Result<Vec<u8>, TelemetryError> {
    let fail = |path: String, problem: FieldProblem| TelemetryError::Entry {
        packet: packet.name.clone(),
        path,
        problem: EntryProblem::Field(problem),
    };
    let mut root = json_object(&packet.entries, types, planned, "")
        .map_err(|(path, problem)| fail(path, problem))?;
    for (pointer, value, what) in &fixed.pointers {
        let tokens = pointer_tokens(pointer)
            .filter(|tokens| !tokens.is_empty())
            .ok_or_else(|| fail(pointer.clone(), FieldProblem::Layout("a JSON Pointer")))?;
        let text = value.json();
        let holds = match root.lookup(&tokens) {
            Lookup::Free => {
                root.insert(&tokens, Node::Leaf(text));
                continue;
            }
            Lookup::Found(Node::Leaf(found)) if *found == text => continue,
            Lookup::Found(Node::Leaf(found)) => found.clone(),
            Lookup::Found(_) | Lookup::Blocked => "another entry's value".to_string(),
        };
        return Err(TelemetryError::Entry {
            packet: packet.name.clone(),
            path: pointer.clone(),
            problem: EntryProblem::WrongConstant {
                why: what.clone(),
                holds: value.show(),
                given: holds,
            },
        });
    }
    let mut text = String::new();
    root.write(&mut text);
    Ok(text.into_bytes())
}

/// A container's present entries as a JSON object.
fn json_object(
    entries: &[Entry],
    types: &BTreeMap<String, StructType>,
    planned: &[Planned],
    prefix: &str,
) -> Result<Node, Failed> {
    let mut node = Node::Object(BTreeMap::new());
    for (index, entry) in entries.iter().enumerate() {
        let plan = planned.get(index).unwrap_or(&Planned::Absent);
        if matches!(plan, Planned::Absent) {
            continue;
        }
        let name = entry_name(entry).unwrap_or_default();
        let path = joined(prefix, name);
        let pointer = match entry {
            Entry::Field(field) => field.pointer.as_deref(),
            Entry::Array(array) => array.pointer.as_deref(),
            Entry::Struct(inner) => inner.pointer.as_deref(),
            _ => None,
        };
        let tokens = pointer
            .and_then(pointer_tokens)
            .filter(|tokens| !tokens.is_empty())
            .ok_or_else(|| {
                (
                    path.clone(),
                    FieldProblem::Layout("an entry with a JSON Pointer"),
                )
            })?;
        let value = json_node(entry, plan, types, &path)?;
        if !matches!(node.lookup(&tokens), Lookup::Free) {
            return Err((path, FieldProblem::Layout("entries at places of their own")));
        }
        node.insert(&tokens, value);
    }
    Ok(node)
}

fn json_node(
    entry: &Entry,
    plan: &Planned,
    types: &BTreeMap<String, StructType>,
    path: &str,
) -> Result<Node, Failed> {
    let leaf = |raw: &Raw, path: &str| {
        json_text(&positive_zero(&raw.value))
            .map(Node::Leaf)
            .map_err(|problem| (path.to_string(), problem))
    };
    match (entry, plan) {
        (Entry::Field(_), Planned::Scalar(raw)) => leaf(raw, path),
        (Entry::Array(array), Planned::Array(elements)) => {
            let mut items = Vec::with_capacity(elements.len());
            for (index, element) in elements.iter().enumerate() {
                let item_path = format!("{path}[{index}]");
                items.push(match (&array.element, element) {
                    (_, Planned::Null) => Node::Leaf("null".to_string()),
                    (Element::Scalar(_), Planned::Scalar(raw)) => leaf(raw, &item_path)?,
                    (Element::Struct { name }, Planned::Record(inner)) => {
                        let record_type = types.get(name).ok_or_else(|| {
                            (
                                item_path.clone(),
                                FieldProblem::Layout("a struct type the set has"),
                            )
                        })?;
                        json_object(&record_type.entries, types, inner, &item_path)?
                    }
                    _ => {
                        return Err((
                            item_path,
                            FieldProblem::Layout("elements of the array's kind"),
                        ));
                    }
                });
            }
            Ok(Node::Array(items))
        }
        (Entry::Struct(inner), Planned::Record(inner_planned)) => {
            let record_type = types.get(&inner.struct_type).ok_or_else(|| {
                (
                    path.to_string(),
                    FieldProblem::Layout("a struct type the set has"),
                )
            })?;
            json_object(&record_type.entries, types, inner_planned, path)
        }
        _ => Err((
            path.to_string(),
            FieldProblem::Layout("a JSON entry with a value of its kind"),
        )),
    }
}

/// JSON writes -0.0 as 0, which reads back as +0.0.
fn positive_zero(value: &Value<'static>) -> Value<'static> {
    match value {
        Value::Float(x) if *x == 0.0 => Value::Float(0.0),
        other => other.clone(),
    }
}

// ---- The record and the check -------------------------------------------------

/// The record decoding the payload gives: each present field, array,
/// struct and checksum, by entry index.
fn record_of<'a>(
    entries: &'a [Entry],
    types: &'a BTreeMap<String, StructType>,
    planned: Vec<Planned>,
    checksums: &BTreeMap<usize, u64>,
    binary: bool,
) -> Record<'a> {
    // JSON writes -0.0 as 0, which reads back as +0.0.
    let read_back = |value: Value<'static>| {
        if binary { value } else { positive_zero(&value) }
    };
    let mut record = Record::default();
    for (index, (entry, plan)) in entries.iter().zip(planned).enumerate() {
        let value = match (entry, plan) {
            (Entry::Field(field), Planned::Scalar(raw)) => {
                Some((&field.name, read_back(raw.value)))
            }
            (Entry::Checksum(checksum), Planned::Checksum(_)) => checksums
                .get(&index)
                .map(|value| (&checksum.name, Value::Uint(*value))),
            (Entry::Array(array), Planned::Array(elements)) => {
                let items = elements
                    .into_iter()
                    .filter_map(|element| match (&array.element, element) {
                        (_, Planned::Null) => Some(Value::Null),
                        (Element::Scalar(_), Planned::Scalar(raw)) => Some(read_back(raw.value)),
                        (Element::Struct { name }, Planned::Record(inner)) => {
                            let inner_entries = types.get(name).map_or(&[][..], |t| &t.entries);
                            Some(Value::Record(record_of(
                                inner_entries,
                                types,
                                inner,
                                &BTreeMap::new(),
                                binary,
                            )))
                        }
                        _ => None,
                    })
                    .collect();
                Some((&array.name, Value::Array(items)))
            }
            (Entry::Struct(inner), Planned::Record(inner_planned)) => {
                let inner_entries = types
                    .get(&inner.struct_type)
                    .map_or(&[][..], |t| &t.entries);
                Some((
                    &inner.name,
                    Value::Record(record_of(
                        inner_entries,
                        types,
                        inner_planned,
                        &BTreeMap::new(),
                        binary,
                    )),
                ))
            }
            _ => None,
        };
        if let Some((name, value)) = value {
            record.push(index, name, value);
        }
    }
    record
}

/// The native id sending every message, its binding naming the set unless
/// each message's version names it.
struct Sender {
    set: Option<String>,
}

impl Resolver for Sender {
    fn resolve(&self, _family: &str, native_id: &NativeId, _at: Timestamp) -> Option<Arc<Binding>> {
        let binding = Binding::new(native_id.to_string());
        Some(Arc::new(match &self.set {
            Some(set) => binding.with_set(set),
            None => binding,
        }))
    }
}

/// Decodes the payload with its metadata, as from the native id, and
/// refuses it unless decoding gives this set, packet, native id and
/// record.
fn check_round_trip(
    sets: &FamilySets,
    set: &Set,
    packet: &Packet,
    request: &TelemetryRequest,
    payload: &[u8],
    metadata: &[(String, MetadataValue)],
    record: &Record<'_>,
) -> Result<(EventTime, Option<Timestamp>), TelemetryError> {
    let otherwise = |detail: String| TelemetryError::DecodesOtherwise {
        packet: packet.name.clone(),
        detail,
    };
    let meta: Vec<(&str, MetaValue<'_>)> = metadata
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_meta()))
        .collect();
    // Received at the event time, a GPS week of fewer bits than the week
    // resolves to the event time's.
    let ground_time = request.at.unwrap_or(Timestamp::UNIX_EPOCH);
    let message = Message::new(Payload::of(sets.family().encoding, payload), ground_time)
        .with_metadata(&meta);
    let sender = Sender {
        set: (!sets.versioned()).then(|| set.name.clone()),
    };
    let decoded = sets
        .decode(&message, &sender)
        .map_err(|error| match error {
            DecodeError::Time { reason, .. } => TelemetryError::Entry {
                packet: packet.name.clone(),
                path: clock_path(set, packet),
                problem: EntryProblem::InvalidTime { reason },
            },
            error => otherwise(format!(
                "decoding the payload fails: {error} [{}]",
                error.code()
            )),
        })?;
    if decoded.route.set.name != set.name {
        return Err(otherwise(format!(
            "decoding the payload chooses set {}",
            decoded.route.set.name
        )));
    }
    if decoded.packet.name != packet.name {
        return Err(otherwise(format!(
            "decoding the payload identifies packet {}, whose identify keys these values match \
             too",
            decoded.packet.name
        )));
    }
    if decoded.route.native_id != request.native_id {
        return Err(otherwise(format!(
            "decoding the payload reads native id {}",
            decoded.route.native_id
        )));
    }
    if let Some(path) = first_difference(&decoded.record, record, "") {
        return Err(otherwise(format!(
            "decoding the payload gives {path} another value, since something else is written \
             over it"
        )));
    }
    let booted = boot_instant(&decoded);
    if let Some(at) = request.at
        && let Some(read) = instant(decoded.event_time, request.booted)
        && read > at
    {
        return Err(otherwise(format!(
            "decoding the payload reads event time {read}, after the request's {at}"
        )));
    }
    Ok((decoded.event_time, booted))
}

/// Where a packet's event time is read, for an error: its time field, the
/// field or pointer an ISO 8601 time reads, or `time` for a CUC time code.
fn clock_path(set: &Set, packet: &Packet) -> String {
    match &set.time {
        Some(Time::Iso8601 { pointer }) => {
            top_level_at(packet, pointer).unwrap_or_else(|| pointer.clone())
        }
        Some(Time::Elapsed { .. } | Time::Gps { .. } | Time::Boot { .. }) => {
            packet.time_field.clone().unwrap_or_default()
        }
        Some(Time::Cuc { .. }) | None => "time".to_string(),
    }
}

mod clock;
#[cfg(test)]
mod tests;
