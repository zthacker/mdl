//! Laying a command packet out: bits for a binary family, a JSON document
//! for a JSON one.

use std::collections::BTreeMap;

use super::{EncodeError, FieldProblem, Raw, Slot};
use crate::canonical::{format_double, write_string};
use crate::v2::bits::write_bits;
use crate::v2::checksum::Algorithm;
use crate::v2::decode::{Record, Value};
use crate::v2::ir::{
    ArrayEntry, ByteLength, ByteOrder, CommandPacket, Element, Entry, Field, ScalarType, StructType,
};
use crate::v2::validate::rules::pointer_tokens;

/// The packet as bits, and its record. Entries go where the decoder looks
/// for them; anything no entry covers is zero; checksums are written last,
/// in order of position, so one may cover another.
pub(super) fn binary<'a>(
    packet: &'a CommandPacket,
    types: &'a BTreeMap<String, StructType>,
    mut slots: Vec<Slot>,
    order: ByteOrder,
) -> Result<(Vec<u8>, Record<'a>), EncodeError> {
    let fail = |index: usize, problem| EncodeError::Field {
        packet: packet.name.clone(),
        field: entry_name(packet, index),
        problem,
    };
    let positions =
        positions(packet, types, &slots).map_err(|(index, problem)| fail(index, problem))?;
    let end = positions
        .iter()
        .map(|(at, size)| at + size)
        .max()
        .unwrap_or(0);
    let last = packet.entries.len().saturating_sub(1);
    let length = usize::try_from(end.div_ceil(8))
        .map_err(|_| fail(last, FieldProblem::Layout("a packet that fits in memory")))?;
    let mut payload = vec![0_u8; length];
    let mut checksums = Vec::new();
    for (index, (entry, &(at, _))) in packet.entries.iter().zip(&positions).enumerate() {
        let slot = slots.get(index).unwrap_or(&Slot::Empty);
        match entry {
            Entry::Field(field) => {
                write_scalar(&mut payload, field, slot, at, order)
                    .map_err(|problem| fail(index, problem))?;
            }
            Entry::Array(array) => {
                write_array(&mut payload, array, types, slot, at, order)
                    .map_err(|problem| fail(index, problem))?;
            }
            Entry::Checksum(_) => checksums.push((at, index)),
            Entry::Padding(_) => {}
            Entry::Struct(_) => {
                return Err(fail(
                    index,
                    FieldProblem::Layout(
                        "a command packet holds fields, arrays, padding and checksums",
                    ),
                ));
            }
        }
    }
    checksums.sort_unstable();
    for (at, index) in checksums {
        let Some(Entry::Checksum(checksum)) = packet.entries.get(index) else {
            continue;
        };
        let algorithm = Algorithm::resolve(&checksum.algorithm).ok_or_else(|| {
            fail(
                index,
                FieldProblem::Layout("a checksum whose parameters fit its width"),
            )
        })?;
        let from = u64::from(checksum.from_bits.unwrap_or(0));
        let covered = match (usize::try_from(from / 8), usize::try_from(at / 8)) {
            (Ok(first), Ok(stop))
                if from.is_multiple_of(8) && at.is_multiple_of(8) && first <= stop =>
            {
                payload.get(first..stop)
            }
            _ => None,
        }
        .ok_or_else(|| {
            fail(
                index,
                FieldProblem::Layout("a checksum over whole bytes before it"),
            )
        })?;
        let value = algorithm.compute(covered);
        write_bits(
            &mut payload,
            at,
            checksum.algorithm.width_bits(),
            value,
            algorithm.order(order),
        )
        .map_err(|_| fail(index, FieldProblem::Layout("a checksum inside the packet")))?;
        if let Some(slot) = slots.get_mut(index) {
            *slot = Slot::Value(Raw {
                value: Value::Uint(value),
                bytes: Vec::new(),
            });
        }
    }
    Ok((payload, record(&packet.entries, types, slots)))
}

/// The bits a scalar takes: a fixed buffer all of them, terminated or not;
/// other strings and bytes their value and any terminator; anything else
/// its size.
fn scalar_bits(field: &Field, slot: &Slot) -> u64 {
    match (field.ty, &field.length) {
        (ScalarType::String | ScalarType::Bytes, Some(ByteLength::Fixed { bytes, .. })) => {
            u64::from(*bytes) * 8
        }
        (ScalarType::String | ScalarType::Bytes, length) => {
            let bytes = slot.raw().map_or(0, |raw| raw.bytes.len());
            let terminator = matches!(length, Some(ByteLength::Terminator { .. }));
            (bytes as u64 + u64::from(terminator)) * 8
        }
        _ => u64::from(field.size_bits.unwrap_or(0)),
    }
}

/// Each entry's position and size in bits, as the decoder finds them: at
/// its `offset_bits`, or where the previous entry ended. No two may
/// overlap.
fn positions(
    packet: &CommandPacket,
    types: &BTreeMap<String, StructType>,
    slots: &[Slot],
) -> Result<Vec<(u64, u64)>, (usize, FieldProblem)> {
    let mut positions = Vec::with_capacity(packet.entries.len());
    let mut cursor = 0_u64;
    for (index, entry) in packet.entries.iter().enumerate() {
        let slot = slots.get(index).unwrap_or(&Slot::Empty);
        let (offset, size) = match entry {
            Entry::Field(field) => (field.offset_bits, scalar_bits(field, slot)),
            Entry::Array(array) => {
                let size = array_bits(array, types, slot).map_err(|problem| (index, problem))?;
                (array.offset_bits, size)
            }
            Entry::Padding(padding) => (padding.offset_bits, u64::from(padding.size_bits)),
            Entry::Checksum(checksum) => (
                checksum.offset_bits,
                u64::from(checksum.algorithm.width_bits()),
            ),
            Entry::Struct(_) => (None, 0),
        };
        let at = offset.map_or(cursor, u64::from);
        cursor = at
            .checked_add(size)
            .ok_or((index, FieldProblem::Layout("a packet under 2^64 bits")))?;
        positions.push((at, size));
    }
    // Sorted by position, an entry overlaps an earlier one when it starts
    // before the furthest end so far.
    let mut by_position: Vec<(u64, u64, usize)> = positions
        .iter()
        .enumerate()
        .filter(|(_, (_, size))| *size > 0)
        .map(|(index, (at, size))| (*at, at + size, index))
        .collect();
    by_position.sort_unstable();
    let mut furthest = 0_u64;
    for (at, end, index) in by_position {
        if at < furthest {
            // Validation refuses entries that can overlap.
            return Err((index, FieldProblem::Layout("entries that don't overlap")));
        }
        furthest = furthest.max(end);
    }
    Ok(positions)
}

/// The elements of an array, as `slot` holds them.
fn elements(slot: &Slot) -> Result<&[Slot], FieldProblem> {
    match slot {
        Slot::Elements(elements) => Ok(elements),
        _ => Err(FieldProblem::Layout("an array with elements")),
    }
}

/// The bits an array's elements take, one after the other.
fn array_bits(
    array: &ArrayEntry,
    types: &BTreeMap<String, StructType>,
    slot: &Slot,
) -> Result<u64, FieldProblem> {
    let shape = Shape::of(array, types)?;
    let mut size = 0_u64;
    for element in elements(slot)? {
        let bits = match &shape {
            Shape::Scalar(field) => scalar_bits(field, element),
            Shape::Record(entries) => record_layout(entries, element)?.1,
        };
        size = size
            .checked_add(bits)
            .ok_or(FieldProblem::Layout("an array under 2^64 bits"))?;
    }
    Ok(size)
}

/// What an array's elements are: a scalar, as a field, or a record's
/// entries.
enum Shape<'t> {
    Scalar(Box<Field>),
    Record(&'t [Entry]),
}

impl<'t> Shape<'t> {
    fn of(
        array: &ArrayEntry,
        types: &'t BTreeMap<String, StructType>,
    ) -> Result<Self, FieldProblem> {
        Ok(match &array.element {
            Element::Scalar(scalar) => Shape::Scalar(Box::new(scalar.as_field(&array.name))),
            Element::Struct { name } => Shape::Record(record_entries(types, name)?),
        })
    }
}

fn record_entries<'t>(
    types: &'t BTreeMap<String, StructType>,
    name: &str,
) -> Result<&'t [Entry], FieldProblem> {
    types
        .get(name)
        .map(|record| record.entries.as_slice())
        .ok_or(FieldProblem::Layout("a struct type the set has"))
}

/// Where each entry of a record goes from the record's start, and where
/// the record ends: its furthest entry's end, as the decoder measures a
/// struct.
fn record_layout(entries: &[Entry], slot: &Slot) -> Result<(Vec<u64>, u64), FieldProblem> {
    let Slot::Record(slots) = slot else {
        return Err(FieldProblem::Layout("a record with its values"));
    };
    let mut starts = Vec::with_capacity(entries.len());
    let mut cursor = 0_u64;
    let mut furthest = 0_u64;
    for (index, entry) in entries.iter().enumerate() {
        let (offset, size) = match entry {
            Entry::Field(field) => (
                field.offset_bits,
                scalar_bits(field, slots.get(index).unwrap_or(&Slot::Empty)),
            ),
            Entry::Padding(padding) => (padding.offset_bits, u64::from(padding.size_bits)),
            _ => return Err(FieldProblem::Layout("a record of fields and padding")),
        };
        let at = offset.map_or(cursor, u64::from);
        cursor = at
            .checked_add(size)
            .ok_or(FieldProblem::Layout("a record under 2^64 bits"))?;
        furthest = furthest.max(cursor);
        starts.push(at);
    }
    Ok((starts, furthest))
}

/// A field's raw value at `at`.
fn write_scalar(
    payload: &mut [u8],
    field: &Field,
    slot: &Slot,
    at: u64,
    order: ByteOrder,
) -> Result<(), FieldProblem> {
    let raw = slot
        .raw()
        .ok_or(FieldProblem::Layout("a field with a value"))?;
    let order = field.byte_order.unwrap_or(order);
    let size = scalar_bits(field, slot);
    write_field(payload, raw, at, size, field.length.as_ref(), order)
}

/// An array's elements from `at`, one after the other.
fn write_array(
    payload: &mut [u8],
    array: &ArrayEntry,
    types: &BTreeMap<String, StructType>,
    slot: &Slot,
    at: u64,
    order: ByteOrder,
) -> Result<(), FieldProblem> {
    let shape = Shape::of(array, types)?;
    let mut cursor = at;
    for element in elements(slot)? {
        let bits = match &shape {
            Shape::Scalar(field) => {
                write_scalar(payload, field, element, cursor, order)?;
                scalar_bits(field, element)
            }
            Shape::Record(entries) => {
                let (starts, end) = record_layout(entries, element)?;
                let Slot::Record(slots) = element else {
                    return Err(FieldProblem::Layout("a record with its values"));
                };
                for ((entry, start), slot) in entries.iter().zip(starts).zip(slots) {
                    if let Entry::Field(field) = entry {
                        write_scalar(payload, field, slot, cursor + start, order)?;
                    }
                }
                end
            }
        };
        cursor += bits;
    }
    Ok(())
}

/// One field's raw value at `at`, `size` bits (as `positions` found them).
fn write_field(
    payload: &mut [u8],
    raw: &Raw,
    at: u64,
    size: u64,
    length: Option<&ByteLength>,
    order: ByteOrder,
) -> Result<(), FieldProblem> {
    let inside = FieldProblem::Layout("a field inside the packet");
    let bits = |value: u64, payload: &mut [u8]| {
        let size = u32::try_from(size).map_err(|_| inside.clone())?;
        write_bits(payload, at, size, value, order).map_err(|_| inside.clone())
    };
    match &raw.value {
        Value::Str(_) | Value::Bytes(_) => {
            let first = usize::try_from(at / 8)
                .ok()
                .filter(|_| at.is_multiple_of(8))
                .ok_or(FieldProblem::Layout("strings and bytes start on a byte"))?;
            // After the value, a terminator: once for a terminated length,
            // to the end of the buffer for a terminated fixed one, which a
            // value filling the buffer leaves out.
            let filler = match length {
                Some(
                    ByteLength::Terminator { byte }
                    | ByteLength::Fixed {
                        terminator: Some(byte),
                        ..
                    },
                ) => *byte,
                _ => 0,
            };
            let run = raw.bytes.iter().copied().chain(std::iter::repeat(filler));
            let stop = usize::try_from(size / 8)
                .ok()
                .and_then(|bytes| first.checked_add(bytes))
                .ok_or_else(|| inside.clone())?;
            let target = payload
                .get_mut(first..stop)
                .ok_or(FieldProblem::Layout("a field inside the packet"))?;
            for (slot, byte) in target.iter_mut().zip(run) {
                *slot = byte;
            }
            Ok(())
        }
        Value::Uint(n) => bits(*n, payload),
        Value::Int(n) => {
            // The two's-complement bits of n in the field's size, which n
            // fits.
            let all = *n as u64;
            let value = if size >= 64 {
                all
            } else {
                all & ((1 << size) - 1)
            };
            bits(value, payload)
        }
        Value::Bool(b) => bits(u64::from(*b), payload),
        // The value is a binary32 already, for a 32-bit field.
        Value::Float(x) => match size {
            32 => bits(u64::from((*x as f32).to_bits()), payload),
            _ => bits(x.to_bits(), payload),
        },
        _ => Err(FieldProblem::Layout("a scalar field")),
    }
}

/// The packet as a JSON document, and its record: each field's value at
/// its pointer, each array's at its pointer as a JSON array, objects
/// created along the way. No pointer may be another's prefix.
pub(super) fn json<'a>(
    packet: &'a CommandPacket,
    types: &'a BTreeMap<String, StructType>,
    slots: Vec<Slot>,
) -> Result<(Vec<u8>, Record<'a>), EncodeError> {
    let fail = |index: usize, problem| EncodeError::Field {
        packet: packet.name.clone(),
        field: entry_name(packet, index),
        problem,
    };
    let root =
        object(&packet.entries, types, &slots).map_err(|(index, problem)| fail(index, problem))?;
    let mut text = String::new();
    root.write(&mut text);
    let slots = slots.into_iter().map(positive_zero).collect();
    Ok((text.into_bytes(), record(&packet.entries, types, slots)))
}

/// JSON writes -0.0 as 0, which reads back as +0.0.
fn positive_zero(slot: Slot) -> Slot {
    match slot {
        Slot::Value(Raw {
            value: Value::Float(x),
            bytes,
        }) => Slot::Value(Raw {
            value: Value::Float(if x == 0.0 { 0.0 } else { x }),
            bytes,
        }),
        Slot::Elements(elements) => {
            Slot::Elements(elements.into_iter().map(positive_zero).collect())
        }
        Slot::Record(slots) => Slot::Record(slots.into_iter().map(positive_zero).collect()),
        slot => slot,
    }
}

/// A packet's or record's entries as a JSON object: each at its pointer.
/// A problem names the entry by index.
fn object(
    entries: &[Entry],
    types: &BTreeMap<String, StructType>,
    slots: &[Slot],
) -> Result<Node, (usize, FieldProblem)> {
    let mut placed: Vec<Vec<String>> = Vec::new();
    let mut root = Node::Object(BTreeMap::new());
    for (index, entry) in entries.iter().enumerate() {
        let fail = |problem| (index, problem);
        let pointer = match entry {
            Entry::Field(field) => &field.pointer,
            Entry::Array(array) => &array.pointer,
            _ => {
                return Err(fail(FieldProblem::Layout(
                    "a JSON command packet holds fields and arrays",
                )));
            }
        };
        // Validation refuses the empty pointer, so the payload stays an
        // object.
        let tokens = pointer
            .as_deref()
            .and_then(pointer_tokens)
            .filter(|tokens| !tokens.is_empty())
            .ok_or_else(|| fail(FieldProblem::Layout("an entry with a JSON Pointer")))?;
        // Validation refuses pointers that equal or lead into each other.
        if placed
            .iter()
            .any(|earlier| earlier.starts_with(&tokens) || tokens.starts_with(earlier))
        {
            return Err(fail(FieldProblem::Layout(
                "entries written to places of their own",
            )));
        }
        let slot = slots.get(index).unwrap_or(&Slot::Empty);
        let node = match entry {
            Entry::Array(array) => json_array(array, types, slot).map_err(fail)?,
            _ => {
                let raw = slot
                    .raw()
                    .ok_or_else(|| fail(FieldProblem::Layout("a field with a value")))?;
                Node::Leaf(json_text(&raw.value).map_err(fail)?)
            }
        };
        root.insert(&tokens, node);
        placed.push(tokens);
    }
    Ok(root)
}

/// An array's elements as a JSON array: values, or records as objects.
fn json_array(
    array: &ArrayEntry,
    types: &BTreeMap<String, StructType>,
    slot: &Slot,
) -> Result<Node, FieldProblem> {
    let mut items = Vec::new();
    for (index, element) in elements(slot)?.iter().enumerate() {
        let item = match (&array.element, element) {
            (Element::Struct { name }, Slot::Record(slots)) => {
                let entries = record_entries(types, name)?;
                object(entries, types, slots).map_err(|(at, problem)| FieldProblem::Element {
                    index,
                    field: entries.get(at).and_then(entry_label),
                    problem: Box::new(problem),
                })?
            }
            (Element::Scalar(_), Slot::Value(raw)) => Node::Leaf(json_text(&raw.value)?),
            _ => return Err(FieldProblem::Layout("elements of the array's kind")),
        };
        items.push(item);
    }
    Ok(Node::Array(items))
}

fn entry_label(entry: &Entry) -> Option<String> {
    match entry {
        Entry::Field(field) => Some(field.name.clone()),
        Entry::Array(array) => Some(array.name.clone()),
        _ => None,
    }
}

/// A JSON document being built, each value already written as text.
enum Node {
    Leaf(String),
    Array(Vec<Node>),
    Object(BTreeMap<String, Node>),
}

impl Node {
    /// Puts `node` at the path, creating objects along it. The caller
    /// checked that the path isn't empty and that nothing else is on it or
    /// below it.
    fn insert(&mut self, path: &[String], node: Node) {
        let Some((first, rest)) = path.split_first() else {
            *self = node;
            return;
        };
        if !matches!(self, Node::Object(_)) {
            *self = Node::Object(BTreeMap::new());
        }
        if let Node::Object(members) = self {
            members
                .entry(first.clone())
                .or_insert_with(|| Node::Object(BTreeMap::new()))
                .insert(rest, node);
        }
    }

    /// As RFC 8785 writes JSON: no whitespace, members in order of their
    /// keys' UTF-16 code units, strings escaped as it requires.
    fn write(&self, out: &mut String) {
        match self {
            Node::Leaf(text) => out.push_str(text),
            Node::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Node::Object(members) => {
                let mut sorted: Vec<(&String, &Node)> = members.iter().collect();
                sorted.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
                out.push('{');
                for (index, (key, value)) in sorted.into_iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write_string(out, key);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }
}

/// A raw value as JSON text: integers in exact decimal digits whatever
/// their size, doubles as ECMAScript writes them (the shortest form that
/// reads back as the same double), strings as RFC 8785 escapes them. None
/// of it depends on how serde_json is built.
pub(super) fn json_text(value: &Value<'_>) -> Result<String, FieldProblem> {
    Ok(match value {
        Value::Uint(n) => n.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(x) if x.is_finite() => format_double(*x),
        Value::Bool(b) => b.to_string(),
        Value::Str(text) => {
            let mut out = String::new();
            write_string(&mut out, text);
            out
        }
        _ => {
            return Err(FieldProblem::Layout(
                "a JSON field holds a finite number, a bool or a string",
            ));
        }
    })
}

/// The record decoding the payload gives: each field, array and checksum,
/// by entry index; an array's elements as values or records.
fn record<'a>(
    entries: &'a [Entry],
    types: &'a BTreeMap<String, StructType>,
    slots: Vec<Slot>,
) -> Record<'a> {
    let mut out = Record::default();
    for (index, (entry, slot)) in entries.iter().zip(slots).enumerate() {
        let value = match (entry, slot) {
            (Entry::Field(field), Slot::Value(raw)) => Some((&field.name, raw.value)),
            (Entry::Checksum(checksum), Slot::Value(raw)) => Some((&checksum.name, raw.value)),
            (Entry::Array(array), Slot::Elements(elements)) => {
                let items = elements
                    .into_iter()
                    .filter_map(|element| match (&array.element, element) {
                        (Element::Scalar(_), Slot::Value(raw)) => Some(raw.value),
                        (Element::Struct { name }, Slot::Record(slots)) => {
                            let entries = types.get(name).map_or(&[][..], |t| &t.entries);
                            Some(Value::Record(record(entries, types, slots)))
                        }
                        _ => None,
                    })
                    .collect();
                Some((&array.name, Value::Array(items)))
            }
            _ => None,
        };
        if let Some((name, value)) = value {
            out.push(index, name, value);
        }
    }
    out
}

fn entry_name(packet: &CommandPacket, index: usize) -> String {
    match packet.entries.get(index) {
        Some(Entry::Field(field)) => field.name.clone(),
        Some(Entry::Checksum(checksum)) => checksum.name.clone(),
        Some(Entry::Array(array)) => array.name.clone(),
        Some(Entry::Struct(inner)) => inner.name.clone(),
        Some(Entry::Padding(_)) => format!("padding at entries/{index}"),
        None => format!("entries/{index}"),
    }
}
