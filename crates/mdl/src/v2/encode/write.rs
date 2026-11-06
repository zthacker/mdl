//! Laying a command packet out: bits for a binary family, a JSON document
//! for a JSON one.

use std::collections::BTreeMap;

use super::{EncodeError, FieldProblem, Raw};
use crate::canonical::{format_double, write_string};
use crate::v2::bits::write_bits;
use crate::v2::checksum::Algorithm;
use crate::v2::decode::{Record, Value};
use crate::v2::ir::{ByteLength, ByteOrder, CommandPacket, Entry, ScalarType};
use crate::v2::validate::rules::pointer_tokens;

/// The packet as bits, and its record. Entries go where the decoder looks
/// for them; anything no entry covers is zero; checksums are written last,
/// in order of position, so one may cover another.
pub(super) fn binary<'a>(
    packet: &'a CommandPacket,
    mut raws: Vec<Option<Raw>>,
    order: ByteOrder,
) -> Result<(Vec<u8>, Record<'a>), EncodeError> {
    let fail = |index: usize, problem| EncodeError::Field {
        packet: packet.name.clone(),
        field: entry_name(packet, index),
        problem,
    };
    let positions = positions(packet, &raws).map_err(|(index, problem)| fail(index, problem))?;
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
    for (index, (entry, &(at, size))) in packet.entries.iter().zip(&positions).enumerate() {
        match entry {
            Entry::Field(field) => {
                let raw = raws
                    .get(index)
                    .and_then(Option::as_ref)
                    .ok_or_else(|| fail(index, FieldProblem::Layout("a field with no value")))?;
                write_field(&mut payload, raw, at, size, field.length.as_ref(), order)
                    .map_err(|problem| fail(index, problem))?;
            }
            Entry::Checksum(_) => checksums.push((at, index)),
            Entry::Padding(_) => {}
            Entry::Array(_) | Entry::Struct(_) => {
                return Err(fail(
                    index,
                    FieldProblem::Layout("a command packet holds fields, padding and checksums"),
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
        if let Some(slot) = raws.get_mut(index) {
            *slot = Some(Raw {
                value: Value::Uint(value),
                bytes: Vec::new(),
            });
        }
    }
    Ok((payload, record(packet, raws)))
}

/// Each entry's position and size in bits, as the decoder finds them: at
/// its `offset_bits`, or where the previous entry ended. No two may
/// overlap.
fn positions(
    packet: &CommandPacket,
    raws: &[Option<Raw>],
) -> Result<Vec<(u64, u64)>, (usize, FieldProblem)> {
    let mut positions = Vec::with_capacity(packet.entries.len());
    let mut cursor = 0_u64;
    for (index, entry) in packet.entries.iter().enumerate() {
        let (offset, size) = match entry {
            Entry::Field(field) => {
                let size = match field.ty {
                    ScalarType::String | ScalarType::Bytes => {
                        let bytes = raws
                            .get(index)
                            .and_then(Option::as_ref)
                            .map_or(0, |raw| raw.bytes.len());
                        let terminator =
                            matches!(field.length, Some(ByteLength::Terminator { .. }));
                        (bytes as u64 + u64::from(terminator)) * 8
                    }
                    _ => u64::from(field.size_bits.unwrap_or(0)),
                };
                (field.offset_bits, size)
            }
            Entry::Padding(padding) => (padding.offset_bits, u64::from(padding.size_bits)),
            Entry::Checksum(checksum) => (
                checksum.offset_bits,
                u64::from(checksum.algorithm.width_bits()),
            ),
            Entry::Array(_) | Entry::Struct(_) => (None, 0),
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
            let terminator = match length {
                Some(ByteLength::Terminator { byte }) => Some(*byte),
                _ => None,
            };
            let run = raw.bytes.iter().copied().chain(terminator);
            let stop = first + raw.bytes.len() + usize::from(terminator.is_some());
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
/// its pointer, objects created along the way. No pointer may be another's
/// prefix.
pub(super) fn json<'a>(
    packet: &'a CommandPacket,
    raws: Vec<Option<Raw>>,
) -> Result<(Vec<u8>, Record<'a>), EncodeError> {
    let fail = |index: usize, problem| EncodeError::Field {
        packet: packet.name.clone(),
        field: entry_name(packet, index),
        problem,
    };
    let mut placed: Vec<Vec<String>> = Vec::new();
    let mut root = Node::Object(BTreeMap::new());
    for (index, entry) in packet.entries.iter().enumerate() {
        let Entry::Field(field) = entry else {
            return Err(fail(
                index,
                FieldProblem::Layout("a JSON command packet holds fields"),
            ));
        };
        // Validation refuses the empty pointer, so the payload stays an
        // object.
        let tokens = field
            .pointer
            .as_deref()
            .and_then(pointer_tokens)
            .filter(|tokens| !tokens.is_empty())
            .ok_or_else(|| fail(index, FieldProblem::Layout("a field with a JSON Pointer")))?;
        // Validation refuses pointers that equal or lead into each other.
        if placed
            .iter()
            .any(|earlier| earlier.starts_with(&tokens) || tokens.starts_with(earlier))
        {
            return Err(fail(
                index,
                FieldProblem::Layout("fields written to places of their own"),
            ));
        }
        let raw = raws
            .get(index)
            .and_then(Option::as_ref)
            .ok_or_else(|| fail(index, FieldProblem::Layout("a field with a value")))?;
        let leaf = json_text(&raw.value).map_err(|problem| fail(index, problem))?;
        root.insert(&tokens, leaf);
        placed.push(tokens);
    }
    let mut text = String::new();
    root.write(&mut text);
    // JSON writes -0.0 as 0, which reads back as +0.0.
    let raws = raws
        .into_iter()
        .map(|raw| {
            raw.map(|raw| match raw.value {
                Value::Float(x) => Raw {
                    value: Value::Float(if x == 0.0 { 0.0 } else { x }),
                    bytes: raw.bytes,
                },
                _ => raw,
            })
        })
        .collect();
    Ok((text.into_bytes(), record(packet, raws)))
}

/// A JSON document being built, each value already written as text.
enum Node {
    Leaf(String),
    Object(BTreeMap<String, Node>),
}

impl Node {
    /// Puts `leaf` at the path, creating objects along it. The caller
    /// checked that the path isn't empty and that no other leaf is on it or
    /// below it.
    fn insert(&mut self, path: &[String], leaf: String) {
        let Some((first, rest)) = path.split_first() else {
            *self = Node::Leaf(leaf);
            return;
        };
        if let Node::Leaf(_) = self {
            *self = Node::Object(BTreeMap::new());
        }
        if let Node::Object(members) = self {
            members
                .entry(first.clone())
                .or_insert_with(|| Node::Object(BTreeMap::new()))
                .insert(rest, leaf);
        }
    }

    /// As RFC 8785 writes JSON: no whitespace, members in order of their
    /// keys' UTF-16 code units, strings escaped as it requires.
    fn write(&self, out: &mut String) {
        match self {
            Node::Leaf(text) => out.push_str(text),
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

/// The record decoding the payload gives: each field and checksum, by
/// entry index.
fn record(packet: &CommandPacket, raws: Vec<Option<Raw>>) -> Record<'_> {
    let mut record = Record::default();
    for (index, (entry, raw)) in packet.entries.iter().zip(raws).enumerate() {
        let name = match entry {
            Entry::Field(field) => &field.name,
            Entry::Checksum(checksum) => &checksum.name,
            _ => continue,
        };
        if let Some(raw) = raw {
            record.push(index, name, raw.value);
        }
    }
    record
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
