//! Extraction: a packet's entries, read from binary or JSON payloads.

use std::collections::BTreeMap;

use serde_json::Value as Json;

use super::{Problem, Record, Value};
use crate::v2::bits::{BitsError, read_bits, sign_extend};
use crate::v2::ir::{
    ArrayEntry, ByteLength, ByteOrder, CompareOp, Condition, Count, Element, Entry, Field,
    FromField, Scalar, ScalarElement, ScalarType, StructType, TextEncoding,
};
use crate::v2::route::json_integer;
use crate::v2::validate::rules::integer_value;

/// Each field of a container by name, with its index among the
/// container's entries, so references are found without a scan.
pub(super) type FieldNames = BTreeMap<String, usize>;

/// What the walkers need from a set and its compiled program.
#[derive(Clone, Copy)]
pub(super) struct Tables<'a> {
    pub(super) types: &'a BTreeMap<String, StructType>,
    pub(super) type_sizes: &'a BTreeMap<String, u64>,
    pub(super) type_names: &'a BTreeMap<String, FieldNames>,
}

impl<'a> Tables<'a> {
    fn struct_type(&self, name: &str) -> Result<(&'a [Entry], &'a FieldNames), Problem> {
        match (self.types.get(name), self.type_names.get(name)) {
            (Some(struct_type), Some(names)) => Ok((&struct_type.entries, names)),
            _ => Err(Problem::Layout("a struct type that isn't defined")),
        }
    }
}

/// The fields of the container being decoded, for conditions, counts and
/// lengths.
struct Scope<'r, 'a> {
    record: &'r Record<'a>,
    names: &'a FieldNames,
}

impl<'r, 'a> Scope<'r, 'a> {
    fn get(&self, name: &str) -> Result<&'r Value<'a>, Problem> {
        self.names
            .get(name)
            .and_then(|&index| self.record.at_entry(index))
            .ok_or(Problem::Layout("a reference to a field not decoded yet"))
    }
}

/// Where the walker is, for naming an entry that fails.
#[derive(Debug, Clone, Copy)]
pub(super) enum Segment<'a> {
    Name(&'a str),
    Index(u64),
}

/// The parts of a field or array element that decide how it's read.
#[derive(Clone, Copy)]
struct ScalarShape<'a> {
    ty: ScalarType,
    size_bits: Option<u32>,
    length: Option<&'a ByteLength>,
    text: Option<TextEncoding>,
}

impl<'a> ScalarShape<'a> {
    fn of_field(field: &'a Field) -> Self {
        ScalarShape {
            ty: field.ty,
            size_bits: field.size_bits,
            length: field.length.as_ref(),
            text: field.text,
        }
    }

    fn of_element(element: &'a ScalarElement) -> Self {
        ScalarShape {
            ty: element.ty,
            size_bits: element.size_bits,
            length: element.length.as_ref(),
            text: element.text,
        }
    }
}

/// Reads entries from a binary packet. Positions are in bits from the
/// start of the packet; `bytes` is the packet, cut to its exact length.
pub(super) struct BinaryWalker<'a, 'p> {
    tables: Tables<'a>,
    bytes: &'p [u8],
    order: ByteOrder,
    path: Vec<Segment<'a>>,
}

impl<'a, 'p> BinaryWalker<'a, 'p> {
    pub(super) fn new(tables: Tables<'a>, bytes: &'p [u8], order: ByteOrder) -> Self {
        BinaryWalker {
            tables,
            bytes,
            order,
            path: Vec::new(),
        }
    }

    /// The path of the entry being read, or of the one that failed.
    pub(super) fn path(&self) -> &[Segment<'a>] {
        &self.path
    }

    fn end(&self) -> u64 {
        // usize is at most 64 bits on every target Rust supports.
        (self.bytes.len() as u64).saturating_mul(8)
    }

    /// A container's entries from bit `start`: the record and the furthest
    /// end of its present entries. With `until`, stop once the field of
    /// that name is decoded (identification reads a key this way).
    pub(super) fn entries(
        &mut self,
        entries: &'a [Entry],
        names: &'a FieldNames,
        start: u64,
        until: Option<&str>,
    ) -> Result<(Record<'a>, u64), Problem> {
        let mut record = Record::default();
        let mut cursor = start;
        let mut furthest = start;
        for (index, entry) in entries.iter().enumerate() {
            let at = match offset(entry) {
                Some(offset) => start
                    .checked_add(u64::from(offset))
                    .ok_or(Problem::PastEnd {
                        needed_bits: u64::MAX,
                        available_bits: self.end(),
                    })?,
                None => cursor,
            };
            if let Some(condition) = when(entry) {
                let scope = Scope {
                    record: &record,
                    names,
                };
                if !holds(condition, &scope)? {
                    cursor = at;
                    continue;
                }
            }
            let end = match entry {
                Entry::Padding(padding) => self.span(at, u64::from(padding.size_bits))?,
                Entry::Field(field) => {
                    self.path.push(Segment::Name(&field.name));
                    let scope = Scope {
                        record: &record,
                        names,
                    };
                    let (value, size) = self.scalar(ScalarShape::of_field(field), at, &scope)?;
                    self.path.pop();
                    record.push(index, &field.name, value);
                    if until == Some(field.name.as_str()) {
                        return Ok((record, at + size));
                    }
                    at + size
                }
                Entry::Array(array) => {
                    self.path.push(Segment::Name(&array.name));
                    let scope = Scope {
                        record: &record,
                        names,
                    };
                    let (value, size) = self.array(array, at, &scope)?;
                    self.path.pop();
                    record.push(index, &array.name, value);
                    at + size
                }
                Entry::Struct(inner) => {
                    self.path.push(Segment::Name(&inner.name));
                    let (entries, inner_names) = self.tables.struct_type(&inner.struct_type)?;
                    let (value, end) = self.entries(entries, inner_names, at, None)?;
                    self.path.pop();
                    record.push(index, &inner.name, Value::Record(value));
                    end
                }
            };
            cursor = end;
            furthest = furthest.max(end);
        }
        Ok((record, furthest))
    }

    /// The end of `size` bits from `at`, if they're inside the packet.
    fn span(&self, at: u64, size: u64) -> Result<u64, Problem> {
        let end = self.end();
        match at.checked_add(size) {
            Some(stop) if stop <= end => Ok(stop),
            stop => Err(Problem::PastEnd {
                needed_bits: stop.unwrap_or(u64::MAX),
                available_bits: end,
            }),
        }
    }

    fn scalar(
        &self,
        scalar: ScalarShape<'a>,
        at: u64,
        scope: &Scope<'_, 'a>,
    ) -> Result<(Value<'a>, u64), Problem> {
        match scalar.ty {
            ScalarType::Uint | ScalarType::Int | ScalarType::Bool | ScalarType::Float => {
                let size = scalar.size_bits.unwrap_or(0);
                let raw = read_bits(self.bytes, at, size, self.order).map_err(bits_problem)?;
                let value = match (scalar.ty, size) {
                    (ScalarType::Uint, _) => Value::Uint(raw),
                    (ScalarType::Int, _) => Value::Int(sign_extend(raw, size)),
                    (ScalarType::Bool, _) => Value::Bool(raw != 0),
                    (_, 32) => {
                        let bits = u32::try_from(raw).map_err(|_| Problem::Layout("a float"))?;
                        Value::Float(f64::from(f32::from_bits(bits)))
                    }
                    (_, 64) => Value::Float(f64::from_bits(raw)),
                    _ => return Err(Problem::Layout("floats are 32 or 64 bits")),
                };
                Ok((value, u64::from(size)))
            }
            ScalarType::String | ScalarType::Bytes => {
                let (bytes, size) = self.byte_run(scalar.length, at, scope)?;
                let value = if scalar.ty == ScalarType::String {
                    Value::Str(text(bytes, scalar.text)?)
                } else {
                    Value::Bytes(bytes.to_vec())
                };
                Ok((value, size))
            }
        }
    }

    /// The bytes of a string or bytes value at `at`, and its size in bits.
    fn byte_run(
        &self,
        length: Option<&ByteLength>,
        at: u64,
        scope: &Scope<'_, 'a>,
    ) -> Result<(&'p [u8], u64), Problem> {
        let end = self.end();
        if at > end {
            return Err(Problem::PastEnd {
                needed_bits: at,
                available_bits: end,
            });
        }
        if !at.is_multiple_of(8) {
            return Err(Problem::Layout("strings and bytes start on a byte"));
        }
        let bytes = self.bytes;
        let first = usize::try_from(at / 8).map_err(|_| Problem::Layout("a position"))?;
        let rest = bytes.get(first..).unwrap_or_default();
        let len = match length {
            Some(ByteLength::Fixed { bytes }) => u64::from(*bytes),
            Some(ByteLength::Field(from)) => from_field(from, scope)?,
            Some(ByteLength::Remaining {}) => rest.len() as u64,
            Some(ByteLength::Terminator { byte }) => {
                let Some(position) = rest.iter().position(|b| b == byte) else {
                    return Err(Problem::NoTerminator { byte: *byte });
                };
                // The terminator is part of the value's size, not its bytes.
                return Ok((&rest[..position], (position as u64 + 1) * 8));
            }
            None => return Err(Problem::Layout("strings and bytes have a length")),
        };
        let stop = self.span(at, len.saturating_mul(8))?;
        let count = usize::try_from(len).map_err(|_| Problem::Layout("a length"))?;
        Ok((&rest[..count], stop - at))
    }

    fn array(
        &mut self,
        array: &'a ArrayEntry,
        at: u64,
        scope: &Scope<'_, 'a>,
    ) -> Result<(Value<'a>, u64), Problem> {
        let end = self.end();
        let count = match &array.count {
            Some(Count::Fixed { count }) => Some(u64::from(*count)),
            Some(Count::Field(from)) => Some(from_field(from, scope)?),
            Some(Count::Remaining {}) => None,
            None => return Err(Problem::Layout("binary arrays have a count")),
        };
        let exact_bits = self.element_bits(&array.element);
        let mut items = Vec::new();
        if let Some(count) = count {
            // Every element takes at least one bit, and exactly
            // `exact_bits` when that's known, so the bits left bound the
            // count before anything is allocated.
            let least = exact_bits.unwrap_or(1).max(1);
            self.span(at, count.saturating_mul(least))?;
            if exact_bits.is_some() {
                items.reserve_exact(usize::try_from(count).unwrap_or(0));
            }
        }
        let mut cursor = at;
        let mut index = 0_u64;
        loop {
            let done = match count {
                Some(count) => index == count,
                None => cursor >= end,
            };
            if done {
                break;
            }
            self.path.push(Segment::Index(index));
            let (value, size) = self.element(&array.element, cursor, scope)?;
            if size == 0 {
                return Err(Problem::EmptyElement);
            }
            self.path.pop();
            items.push(value);
            cursor += size;
            index += 1;
        }
        Ok((Value::Array(items), cursor - at))
    }

    fn element(
        &mut self,
        element: &'a Element,
        at: u64,
        scope: &Scope<'_, 'a>,
    ) -> Result<(Value<'a>, u64), Problem> {
        match element {
            Element::Scalar(scalar) => self.scalar(ScalarShape::of_element(scalar), at, scope),
            Element::Struct { name } => {
                let (entries, names) = self.tables.struct_type(name)?;
                let (value, end) = self.entries(entries, names, at, None)?;
                Ok((Value::Record(value), end - at))
            }
        }
    }

    /// An element's size when every element has the same one.
    fn element_bits(&self, element: &Element) -> Option<u64> {
        match element {
            Element::Scalar(scalar) => match (scalar.ty, &scalar.length) {
                (ScalarType::String | ScalarType::Bytes, Some(ByteLength::Fixed { bytes })) => {
                    Some(u64::from(*bytes) * 8)
                }
                (ScalarType::String | ScalarType::Bytes, _) => None,
                _ => scalar.size_bits.map(u64::from),
            },
            Element::Struct { name } => self.tables.type_sizes.get(name.as_str()).copied(),
        }
    }
}

/// Reads entries from a JSON document.
pub(super) struct JsonWalker<'a> {
    tables: Tables<'a>,
    path: Vec<Segment<'a>>,
}

impl<'a> JsonWalker<'a> {
    pub(super) fn new(tables: Tables<'a>) -> Self {
        JsonWalker {
            tables,
            path: Vec::new(),
        }
    }

    pub(super) fn path(&self) -> &[Segment<'a>] {
        &self.path
    }

    /// A container's entries, with pointers relative to `node`.
    pub(super) fn entries(
        &mut self,
        entries: &'a [Entry],
        names: &'a FieldNames,
        node: &Json,
    ) -> Result<Record<'a>, Problem> {
        let mut record = Record::default();
        for (index, entry) in entries.iter().enumerate() {
            if let Some(condition) = when(entry) {
                let scope = Scope {
                    record: &record,
                    names,
                };
                if !holds(condition, &scope)? {
                    continue;
                }
            }
            match entry {
                Entry::Field(field) => {
                    self.path.push(Segment::Name(&field.name));
                    let value = json_scalar(field.ty, found(node, field.pointer.as_deref())?)?;
                    self.path.pop();
                    record.push(index, &field.name, value);
                }
                Entry::Array(array) => {
                    self.path.push(Segment::Name(&array.name));
                    let value = self.array(array, found(node, array.pointer.as_deref())?)?;
                    self.path.pop();
                    record.push(index, &array.name, value);
                }
                Entry::Struct(inner) => {
                    self.path.push(Segment::Name(&inner.name));
                    let (entries, inner_names) = self.tables.struct_type(&inner.struct_type)?;
                    let value =
                        self.entries(entries, inner_names, found(node, inner.pointer.as_deref())?)?;
                    self.path.pop();
                    record.push(index, &inner.name, Value::Record(value));
                }
                Entry::Padding(_) => return Err(Problem::Layout("padding in a JSON payload")),
            }
        }
        Ok(record)
    }

    fn array(&mut self, array: &'a ArrayEntry, value: &Json) -> Result<Value<'a>, Problem> {
        let Json::Array(items) = value else {
            return Err(Problem::WrongType {
                expected: "an array",
                found: json_kind(value),
            });
        };
        let mut decoded = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            self.path.push(Segment::Index(index as u64));
            decoded.push(match &array.element {
                Element::Scalar(scalar) => json_scalar(scalar.ty, item)?,
                Element::Struct { name } => {
                    let (entries, names) = self.tables.struct_type(name)?;
                    Value::Record(self.entries(entries, names, item)?)
                }
            });
            self.path.pop();
        }
        Ok(Value::Array(decoded))
    }
}

fn found<'j>(node: &'j Json, pointer: Option<&str>) -> Result<&'j Json, Problem> {
    let pointer = pointer.ok_or(Problem::Layout("JSON entries have a pointer"))?;
    node.pointer(pointer).ok_or_else(|| Problem::Missing {
        pointer: pointer.to_string(),
    })
}

fn json_scalar<'a>(ty: ScalarType, value: &Json) -> Result<Value<'a>, Problem> {
    let wrong = |expected: &'static str| Problem::WrongType {
        expected,
        found: json_kind(value),
    };
    match ty {
        ScalarType::Uint | ScalarType::Int => {
            let (expected, low, high) = if ty == ScalarType::Uint {
                ("an unsigned 64-bit integer", 0, i128::from(u64::MAX))
            } else {
                (
                    "a signed 64-bit integer",
                    i128::from(i64::MIN),
                    i128::from(i64::MAX),
                )
            };
            let Json::Number(number) = value else {
                return Err(wrong(expected));
            };
            let out_of_range = || Problem::OutOfRange {
                value: number.to_string(),
                expected,
            };
            let integer = json_integer(number)
                .filter(|n| (low..=high).contains(n))
                .ok_or_else(out_of_range)?;
            if ty == ScalarType::Uint {
                u64::try_from(integer)
                    .map(Value::Uint)
                    .map_err(|_| out_of_range())
            } else {
                i64::try_from(integer)
                    .map(Value::Int)
                    .map_err(|_| out_of_range())
            }
        }
        ScalarType::Float => value
            .as_f64()
            .map(Value::Float)
            .ok_or_else(|| wrong("a number")),
        ScalarType::Bool => value
            .as_bool()
            .map(Value::Bool)
            .ok_or_else(|| wrong("a boolean")),
        ScalarType::String => value
            .as_str()
            .map(|text| Value::Str(text.to_string()))
            .ok_or_else(|| wrong("a string")),
        ScalarType::Bytes => Err(Problem::Layout("bytes in a JSON payload")),
    }
}

fn json_kind(value: &Json) -> &'static str {
    match value {
        Json::Null => "null",
        Json::Bool(_) => "a boolean",
        Json::Number(_) => "a number",
        Json::String(_) => "a string",
        Json::Array(_) => "an array",
        Json::Object(_) => "an object",
    }
}

fn offset(entry: &Entry) -> Option<u32> {
    match entry {
        Entry::Field(field) => field.offset_bits,
        Entry::Array(array) => array.offset_bits,
        Entry::Struct(inner) => inner.offset_bits,
        Entry::Padding(padding) => padding.offset_bits,
    }
}

fn when(entry: &Entry) -> Option<&Condition> {
    match entry {
        Entry::Field(field) => field.when.as_ref(),
        Entry::Array(array) => array.when.as_ref(),
        Entry::Struct(inner) => inner.when.as_ref(),
        Entry::Padding(padding) => padding.when.as_ref(),
    }
}

/// Does `condition` hold for the fields decoded so far? Validation keeps
/// references to earlier, unconditional fields of a type the comparison
/// suits, and nesting within `MAX_NESTING`.
fn holds(condition: &Condition, scope: &Scope<'_, '_>) -> Result<bool, Problem> {
    match condition {
        Condition::Compare { field, op, value } => compare(scope.get(field)?, *op, value),
        Condition::In { field, values } => {
            let field = scope.get(field)?;
            for value in values {
                if compare(field, CompareOp::Eq, value)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Condition::All { conditions } => {
            for inner in conditions {
                if !holds(inner, scope)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Condition::Any { conditions } => {
            for inner in conditions {
                if holds(inner, scope)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Condition::Not { condition } => Ok(!holds(condition, scope)?),
    }
}

fn compare(field: &Value<'_>, op: CompareOp, value: &Scalar) -> Result<bool, Problem> {
    if let (Value::Str(text), Scalar::Str(expected)) = (field, value) {
        return match op {
            CompareOp::Eq => Ok(text == expected),
            CompareOp::Ne => Ok(text != expected),
            _ => Err(Problem::Layout("strings compare only for equality")),
        };
    }
    let raw = match field {
        Value::Uint(n) => i128::from(*n),
        Value::Int(n) => i128::from(*n),
        Value::Bool(b) => i128::from(*b),
        _ => {
            return Err(Problem::Layout(
                "conditions compare integers, bools or strings",
            ));
        }
    };
    let expected = integer_value(value).ok_or(Problem::Layout("an integer condition value"))?;
    Ok(match op {
        CompareOp::Eq => raw == expected,
        CompareOp::Ne => raw != expected,
        CompareOp::Lt => raw < expected,
        CompareOp::Le => raw <= expected,
        CompareOp::Gt => raw > expected,
        CompareOp::Ge => raw >= expected,
        CompareOp::MaskAny => raw & expected != 0,
        CompareOp::MaskAll => raw & expected == expected,
    })
}

/// A count or length from an earlier field: `raw * multiply + add`.
fn from_field(from: &FromField, scope: &Scope<'_, '_>) -> Result<u64, Problem> {
    let Value::Uint(raw) = scope.get(&from.field)? else {
        return Err(Problem::Layout(
            "a count or length from a field that isn't a uint",
        ));
    };
    // At most 2^64 * 2^32 + 2^63, far inside an i128.
    let value = i128::from(*raw) * i128::from(from.multiply.unwrap_or(1))
        + i128::from(from.add.unwrap_or(0));
    u64::try_from(value).map_err(|_| Problem::BadLength { value })
}

fn text(bytes: &[u8], encoding: Option<TextEncoding>) -> Result<String, Problem> {
    match encoding.unwrap_or(TextEncoding::Utf8) {
        TextEncoding::Utf8 => std::str::from_utf8(bytes)
            .map(str::to_string)
            .map_err(|_| Problem::InvalidText { encoding: "UTF-8" }),
        TextEncoding::Ascii if bytes.is_ascii() => {
            Ok(bytes.iter().map(|&b| char::from(b)).collect())
        }
        TextEncoding::Ascii => Err(Problem::InvalidText { encoding: "ASCII" }),
        TextEncoding::Latin1 => Ok(bytes.iter().map(|&b| char::from(b)).collect()),
    }
}

fn bits_problem(err: BitsError) -> Problem {
    match err {
        BitsError::PastEnd {
            end_bits,
            payload_bits,
        } => Problem::PastEnd {
            needed_bits: end_bits,
            available_bits: payload_bits,
        },
        BitsError::Misaligned => {
            Problem::Layout("a little-endian value spanning bytes must be whole bytes")
        }
        BitsError::BadSize => Problem::Layout("numbers are 1 to 64 bits"),
    }
}
