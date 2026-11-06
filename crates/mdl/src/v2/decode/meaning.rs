//! Meaning: raw values through their conversions, into engineering values
//! and canonical samples.

use std::collections::BTreeMap;

use super::{Problem, Record, Value};
use crate::v2::ir::{
    Catalog, Composite, Conversion, Element, Entry, Frame, Packet, Param, ParamType, Scalar,
    ScalarElement, StructType,
};
use crate::v2::validate::rules::integer_value;

/// A canonical value from one decoded message.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Sample<'a> {
    /// The catalog param.
    pub param: &'a str,
    /// The param's value when `quality` is good; otherwise the raw value.
    pub value: SampleValue,
    /// Where the value came from, as a record path (`batt.volts`,
    /// `samples[3]`); for a composite, its members' paths joined by `,`.
    pub path: String,
    /// The value's position in its array, for values from array elements.
    pub index: Option<u64>,
    /// How long after the packet's time the value was taken, for high-rate
    /// blocks: element *i* is *i* × `sample_interval_ms` later.
    pub time_offset_ns: Option<i64>,
    pub quality: Quality,
}

/// A sample or engineering value.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SampleValue {
    F64(f64),
    I64(i64),
    U64(u64),
    Bool(bool),
    Str(String),
    Bytes(Vec<u8>),
    /// An enum param's state name.
    Enum(String),
    /// In the source's frame, which consumers convert to the param's.
    Vec3 {
        frame: Frame,
        x: f64,
        y: f64,
        z: f64,
    },
    /// Scalar first, in the source's frame.
    Quat {
        frame: Frame,
        w: f64,
        x: f64,
        y: f64,
        z: f64,
    },
}

/// Whether a value can be trusted, from most to least: qualities compare
/// in that order, and a composite has the quality of its least
/// trustworthy member. A quality added later takes its place in this
/// order, so comparisons keep their meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Quality {
    Good,
    /// An integer that matches none of its states, or a string that isn't
    /// one of the param's states.
    UnknownState,
    /// A raw value the param's type can't hold (a JSON integer past an
    /// i64 param's range).
    OutOfRange,
    /// NaN or an infinity, read or produced by a conversion.
    NonFinite,
    /// A raw value listed as meaning "no reading".
    Invalid,
}

impl Quality {
    pub fn as_str(self) -> &'static str {
        match self {
            Quality::Good => "good",
            Quality::UnknownState => "unknown_state",
            Quality::OutOfRange => "out_of_range",
            Quality::NonFinite => "non_finite",
            Quality::Invalid => "invalid",
        }
    }
}

/// A field's value through its conversion, for any field, mapped or not.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Engineering {
    /// The converted value when `quality` is good; otherwise the raw value.
    /// A field without a conversion has its raw value.
    pub value: SampleValue,
    pub quality: Quality,
}

/// The parts of a field or scalar element that give a raw value meaning.
#[derive(Clone, Copy)]
pub(super) struct ScalarMeaning<'a> {
    conversion: Option<&'a Conversion>,
    invalid: &'a [Scalar],
}

impl<'a> ScalarMeaning<'a> {
    fn of_element(element: &'a ScalarElement) -> Self {
        ScalarMeaning {
            conversion: element.conversion.as_ref(),
            invalid: &element.invalid,
        }
    }

    fn of_entry(entry: &'a Entry) -> Option<Self> {
        match entry {
            Entry::Field(field) => Some(ScalarMeaning {
                conversion: field.conversion.as_ref(),
                invalid: &field.invalid,
            }),
            _ => None,
        }
    }
}

/// One scalar of a record, with its definition and where it is.
pub(super) struct Visit<'a, 'v> {
    pub(super) path: String,
    pub(super) index: Option<u64>,
    pub(super) time_offset_ns: Option<i64>,
    pub(super) canonical: Option<&'a str>,
    pub(super) meaning: ScalarMeaning<'a>,
    pub(super) value: &'v Value<'a>,
}

/// Every scalar of `record` (fields, and elements of scalar arrays) at any
/// depth, in record order.
pub(super) fn visit<'a, 'v, E>(
    entries: &'a [Entry],
    record: &'v Record<'a>,
    types: &'a BTreeMap<String, StructType>,
    prefix: &str,
    index: Option<u64>,
    each: &mut dyn FnMut(Visit<'a, 'v>) -> Result<(), E>,
) -> Result<(), E> {
    for (entry_index, name, value) in &record.entries {
        let Some(entry) = entries.get(*entry_index) else {
            continue;
        };
        let path = joined(prefix, name);
        match (entry, value) {
            (Entry::Field(field), value) => each(Visit {
                path,
                index,
                time_offset_ns: None,
                canonical: field.canonical.as_deref(),
                meaning: ScalarMeaning {
                    conversion: field.conversion.as_ref(),
                    invalid: &field.invalid,
                },
                value,
            })?,
            (Entry::Array(array), Value::Array(items)) => {
                for (position, item) in items.iter().enumerate() {
                    let position = position as u64;
                    if *item == Value::Null {
                        continue;
                    }
                    let item_path = format!("{path}[{position}]");
                    match &array.element {
                        Element::Scalar(element) => each(Visit {
                            path: item_path,
                            index: Some(position),
                            time_offset_ns: array
                                .sample_interval_ms
                                .map(|interval| offset_ns(position, interval)),
                            canonical: array.canonical.as_deref(),
                            meaning: ScalarMeaning::of_element(element),
                            value: item,
                        })?,
                        Element::Struct { name } => {
                            if let (Value::Record(inner), Some(struct_type)) =
                                (item, types.get(name))
                            {
                                visit(
                                    &struct_type.entries,
                                    inner,
                                    types,
                                    &item_path,
                                    Some(position),
                                    each,
                                )?;
                            }
                        }
                    }
                }
            }
            (Entry::Struct(inner_entry), Value::Record(inner)) => {
                if let Some(struct_type) = types.get(&inner_entry.struct_type) {
                    visit(&struct_type.entries, inner, types, &path, index, each)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn joined(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}.{name}")
    }
}

/// Element `position` of a block sampled every `interval_ms`, in whole
/// nanoseconds (rounded to nearest; saturating, though validation keeps
/// intervals finite and positive).
fn offset_ns(position: u64, interval_ms: f64) -> i64 {
    (position as f64 * interval_ms * 1_000_000.0).round() as i64
}

/// A value through its conversion.
pub(super) fn engineering(meaning: ScalarMeaning<'_>, value: &Value<'_>) -> Option<Engineering> {
    let raw = raw_value(value)?;
    if is_sentinel(meaning.invalid, value) {
        return Some(Engineering {
            value: raw,
            quality: Quality::Invalid,
        });
    }
    let (value, quality) = match meaning.conversion {
        None if number(value).is_some_and(|x| !x.is_finite()) => (raw, Quality::NonFinite),
        None => (raw, Quality::Good),
        Some(Conversion::States { states }) => match integer(value)
            .and_then(|n| states.iter().find(|state| i128::from(state.raw) == n))
        {
            Some(state) => (SampleValue::Enum(state.name.clone()), Quality::Good),
            None => (raw, Quality::UnknownState),
        },
        Some(conversion) => {
            let x = number(value)?;
            let converted = convert(conversion, x)?;
            match numeric_quality(x, converted) {
                Quality::Good => (SampleValue::F64(converted), Quality::Good),
                quality => (raw, quality),
            }
        }
    };
    Some(Engineering { value, quality })
}

/// The sample a scalar gives `param`.
pub(super) fn sample_value(
    param: &Param,
    meaning: ScalarMeaning<'_>,
    value: &Value<'_>,
) -> Result<(SampleValue, Quality), Problem> {
    let mismatch = Problem::Layout("a value that doesn't fit its param");
    let raw = raw_value(value).ok_or(mismatch.clone())?;
    if is_sentinel(meaning.invalid, value) {
        return Ok((raw, Quality::Invalid));
    }
    let good = |value: SampleValue| Ok((value, Quality::Good));
    match param.ty {
        ParamType::F64 => {
            let x = number(value).ok_or(mismatch.clone())?;
            let converted = match meaning.conversion {
                Some(conversion) => convert(conversion, x).ok_or(mismatch)?,
                None => x,
            };
            match numeric_quality(x, converted) {
                Quality::Good => good(SampleValue::F64(converted)),
                quality => Ok((raw, quality)),
            }
        }
        ParamType::I64 => match value {
            Value::Int(n) => good(SampleValue::I64(*n)),
            Value::Uint(n) => match i64::try_from(*n) {
                Ok(n) => good(SampleValue::I64(n)),
                Err(_) => Ok((raw, Quality::OutOfRange)),
            },
            _ => Err(mismatch),
        },
        ParamType::U64 => match value {
            Value::Uint(n) => good(SampleValue::U64(*n)),
            _ => Err(mismatch),
        },
        ParamType::Bool => match value {
            Value::Bool(b) => good(SampleValue::Bool(*b)),
            Value::Uint(n) => good(SampleValue::Bool(*n != 0)),
            Value::Int(n) => good(SampleValue::Bool(*n != 0)),
            _ => Err(mismatch),
        },
        ParamType::Str | ParamType::Bytes => good(raw),
        ParamType::Enum => {
            let name = match (meaning.conversion, value) {
                (Some(Conversion::States { states }), _) => integer(value).and_then(|n| {
                    states
                        .iter()
                        .find(|state| i128::from(state.raw) == n)
                        .map(|state| state.name.clone())
                }),
                (None, Value::Str(text)) => Some(text.clone()),
                _ => return Err(mismatch),
            };
            match name {
                Some(name) if param.states.contains(&name) => good(SampleValue::Enum(name)),
                _ => Ok((raw, Quality::UnknownState)),
            }
        }
        ParamType::Vec3 | ParamType::Quat => Err(mismatch),
    }
}

fn raw_value(value: &Value<'_>) -> Option<SampleValue> {
    Some(match value {
        Value::Uint(n) => SampleValue::U64(*n),
        Value::Int(n) => SampleValue::I64(*n),
        Value::Float(x) => SampleValue::F64(*x),
        Value::Bool(b) => SampleValue::Bool(*b),
        Value::Str(text) => SampleValue::Str(text.clone()),
        Value::Bytes(bytes) => SampleValue::Bytes(bytes.clone()),
        Value::Array(_) | Value::Record(_) | Value::Null => return None,
    })
}

/// A numeric raw value as a double (the nearest one, for integers beyond
/// 2^53).
fn number(value: &Value<'_>) -> Option<f64> {
    match value {
        Value::Uint(n) => Some(*n as f64),
        Value::Int(n) => Some(*n as f64),
        Value::Float(x) => Some(*x),
        _ => None,
    }
}

fn integer(value: &Value<'_>) -> Option<i128> {
    match value {
        Value::Uint(n) => Some(i128::from(*n)),
        Value::Int(n) => Some(i128::from(*n)),
        Value::Bool(b) => Some(i128::from(*b)),
        _ => None,
    }
}

/// Linear and polynomial conversions; `None` for states.
pub(crate) fn convert(conversion: &Conversion, x: f64) -> Option<f64> {
    match conversion {
        Conversion::Linear { scale, offset } => Some(x * scale + offset),
        // Horner's rule: c0 + x(c1 + x(c2 + ...)).
        Conversion::Poly { coefficients } => Some(
            coefficients
                .iter()
                .rev()
                .fold(0.0, |sum, coefficient| sum * x + coefficient),
        ),
        Conversion::States { .. } => None,
    }
}

/// Does `value` match one of `invalid`? Integers compare by value, a float
/// with an integer sentinel by numeric value, and strings exactly.
fn is_sentinel(invalid: &[Scalar], value: &Value<'_>) -> bool {
    // The value's type picks the comparison: a string field's sentinel
    // "9999" is the text 9999, not a number.
    invalid.iter().any(|sentinel| match value {
        Value::Str(text) => matches!(sentinel, Scalar::Str(s) if s == text),
        Value::Float(x) => integer_value(sentinel).is_some_and(|n| *x == n as f64),
        _ => integer_value(sentinel).is_some_and(|n| integer(value) == Some(n)),
    })
}

/// `NonFinite` if the raw or converted number is NaN or infinite.
fn numeric_quality(raw: f64, converted: f64) -> Quality {
    if raw.is_finite() && converted.is_finite() {
        Quality::Good
    } else {
        Quality::NonFinite
    }
}

/// A record path, parsed: `cells[2].volts`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Name(String),
    Index(usize),
}

fn parse_path(path: &str) -> Option<Vec<Part>> {
    let mut parts = Vec::new();
    for piece in path.split('.') {
        let (name, rest) = match piece.split_once('[') {
            Some((name, rest)) => (name, Some(rest)),
            None => (piece, None),
        };
        if name.is_empty() {
            return None;
        }
        parts.push(Part::Name(name.to_string()));
        if let Some(rest) = rest {
            let digits = rest.strip_suffix(']')?;
            let plain = !digits.is_empty()
                && digits.bytes().all(|b| b.is_ascii_digit())
                && (digits == "0" || !digits.starts_with('0'));
            if !plain {
                return None;
            }
            parts.push(Part::Index(digits.parse().ok()?));
        }
    }
    Some(parts)
}

/// The definition a path names, if it's a scalar.
fn meaning_at<'a>(
    parts: &[Part],
    entries: &'a [Entry],
    types: &'a BTreeMap<String, StructType>,
) -> Option<ScalarMeaning<'a>> {
    let (Part::Name(name), rest) = parts.split_first()? else {
        return None;
    };
    let entry = entries
        .iter()
        .find(|entry| entry_name(entry) == Some(name))?;
    match (entry, rest) {
        (Entry::Field(_), []) => ScalarMeaning::of_entry(entry),
        (Entry::Struct(inner), rest) => {
            meaning_at(rest, &types.get(&inner.struct_type)?.entries, types)
        }
        (Entry::Array(array), [Part::Index(_), rest @ ..]) => match (&array.element, rest) {
            (Element::Scalar(element), []) => Some(ScalarMeaning::of_element(element)),
            (Element::Struct { name }, rest) => meaning_at(rest, &types.get(name)?.entries, types),
            _ => None,
        },
        _ => None,
    }
}

/// The value a path names in a record.
fn value_at<'r, 'a>(parts: &[Part], record: &'r Record<'a>) -> Option<&'r Value<'a>> {
    let (Part::Name(name), rest) = parts.split_first()? else {
        return None;
    };
    let mut value = record.get(name)?;
    let mut rest = rest;
    loop {
        match (value, rest) {
            (_, []) => return Some(value),
            (Value::Array(items), [Part::Index(index), more @ ..]) => {
                value = items.get(*index)?;
                rest = more;
            }
            (Value::Record(inner), [Part::Name(_), ..]) => return value_at(rest, inner),
            _ => return None,
        }
    }
}

fn entry_name(entry: &Entry) -> Option<&String> {
    match entry {
        Entry::Field(field) => Some(&field.name),
        Entry::Array(array) => Some(&array.name),
        Entry::Struct(inner) => Some(&inner.name),
        Entry::Checksum(checksum) => Some(&checksum.name),
        Entry::Padding(_) => None,
    }
}

/// The engineering value at `path` in a decoded packet.
pub(super) fn engineering_at(
    path: &str,
    packet: &Packet,
    types: &BTreeMap<String, StructType>,
    record: &Record<'_>,
) -> Option<Engineering> {
    let parts = parse_path(path)?;
    let meaning = meaning_at(&parts, &packet.entries, types)?;
    engineering(meaning, value_at(&parts, record)?)
}

/// A composite, ready to evaluate: its members' paths and definitions.
#[derive(Debug, Clone)]
pub(super) struct CompiledComposite {
    param: String,
    frame: Frame,
    lineage: String,
    members: Vec<Member>,
}

#[derive(Debug, Clone)]
struct Member {
    parts: Vec<Part>,
    conversion: Option<Conversion>,
    invalid: Vec<Scalar>,
}

impl CompiledComposite {
    /// `None` for a composite whose members don't resolve, which a
    /// validated set doesn't have.
    pub(super) fn compile(
        composite: &Composite,
        packet: &Packet,
        types: &BTreeMap<String, StructType>,
    ) -> Option<CompiledComposite> {
        let members = composite
            .members
            .iter()
            .map(|path| {
                let parts = parse_path(path)?;
                let meaning = meaning_at(&parts, &packet.entries, types)?;
                Some(Member {
                    conversion: meaning.conversion.cloned(),
                    invalid: meaning.invalid.to_vec(),
                    parts,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(CompiledComposite {
            param: composite.canonical.clone(),
            frame: composite.frame,
            lineage: composite.members.join(","),
            members,
        })
    }

    /// The composite's sample, or `None` if a member is absent.
    pub(super) fn sample<'a>(
        &'a self,
        catalog: &Catalog,
        record: &Record<'_>,
    ) -> Result<Option<Sample<'a>>, Problem> {
        let mut values = Vec::with_capacity(self.members.len());
        let mut quality = Quality::Good;
        for member in &self.members {
            let Some(value) = value_at(&member.parts, record).filter(|v| **v != Value::Null) else {
                return Ok(None);
            };
            let meaning = ScalarMeaning {
                conversion: member.conversion.as_ref(),
                invalid: &member.invalid,
            };
            let raw =
                number(value).ok_or(Problem::Layout("a composite member that isn't a number"))?;
            let converted = match meaning.conversion {
                Some(conversion) => convert(conversion, raw)
                    .ok_or(Problem::Layout("a composite member with states"))?,
                None => raw,
            };
            let member_quality = if is_sentinel(meaning.invalid, value) {
                Quality::Invalid
            } else {
                numeric_quality(raw, converted)
            };
            quality = quality.max(member_quality);
            values.push((raw, converted));
        }
        // Good composites carry converted members; others, as any value
        // that isn't good, carry raw ones.
        let pick = |(raw, converted): (f64, f64)| {
            if quality == Quality::Good {
                converted
            } else {
                raw
            }
        };
        let picked: Vec<f64> = values.into_iter().map(pick).collect();
        let param_type = catalog.params.get(&self.param).map(|param| param.ty);
        let value = match (param_type, picked.as_slice()) {
            (Some(ParamType::Vec3), [x, y, z]) => SampleValue::Vec3 {
                frame: self.frame,
                x: *x,
                y: *y,
                z: *z,
            },
            (Some(ParamType::Quat), [w, x, y, z]) => SampleValue::Quat {
                frame: self.frame,
                w: *w,
                x: *x,
                y: *y,
                z: *z,
            },
            _ => return Err(Problem::Layout("a composite that doesn't fit its param")),
        };
        Ok(Some(Sample {
            param: &self.param,
            value,
            path: self.lineage.clone(),
            index: None,
            time_offset_ns: None,
            quality,
        }))
    }
}

#[cfg(test)]
mod tests;
