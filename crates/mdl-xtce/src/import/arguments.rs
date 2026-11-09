//! Arguments of every shape, and arguments that are others' sizes.
//!
//! An aggregate argument is one catalog argument per member (`target_id`,
//! `target_range`), written by one field each. An array argument is a list
//! argument written by an array: of its elements, or for an aggregate's
//! elements, of records whose fields take one list per member. An
//! argument that gives another's size (an array's ending index, a string's
//! or binary's dynamic size) is the count or length field MDL works out
//! from the value it measures, so it isn't a catalog argument, as Yamcs
//! fills an array's size in when it's left out.
//!
//! Defaults and assigned values of aggregates and arrays are JSON, as
//! Yamcs reads them: an object by member name, an array of values. A value
//! is completed as Yamcs completes it: from the first member it leaves out
//! down, each member's own initial value (an aggregate member's a JSON
//! object of its members), outermost first, then the type's.

use std::borrow::Cow;

use mdl::v2::source::Span;
use serde_json::{Value, json};

use super::entries::{from_field, size_tag};
use super::{Entry, Field, Import};
use crate::Code;
use crate::names::{catalog_part, identifier};
use crate::types::{Dynamic, Length, Raw, Scalar, Shape};

/// One value an argument takes: the argument itself, or one of its
/// members, by the names of the members down to it.
#[derive(Clone)]
pub(super) struct Leaf<'d> {
    pub(super) path: Vec<&'d str>,
    pub(super) scalar: Scalar<'d>,
    /// Each member's own initial value down the path, outermost first; an
    /// aggregate member's is a JSON object of its members.
    pub(super) initials: Vec<Option<(&'d str, Span)>>,
    pub(super) description: Option<(String, Span)>,
}

impl Leaf<'_> {
    /// The member names joined by `.`, for messages.
    pub(super) fn shown(&self) -> String {
        self.path.join(".")
    }
}

/// The values a shape holds, members in order: one for a scalar. `None`
/// for a shape holding an array, which a command can't spread over
/// fields.
pub(super) fn leaves<'d>(shape: &Shape<'d>) -> Option<Vec<Leaf<'d>>> {
    match shape {
        Shape::Scalar(scalar) => Some(vec![Leaf {
            path: Vec::new(),
            scalar: (**scalar).clone(),
            initials: Vec::new(),
            description: None,
        }]),
        Shape::Aggregate(members) => {
            let mut out = Vec::new();
            for member in members {
                for mut leaf in leaves(&member.shape)? {
                    leaf.path.insert(0, member.name);
                    leaf.initials.insert(0, member.initial);
                    leaf.description = leaf.description.or_else(|| member.description.clone());
                    out.push(leaf);
                }
            }
            Some(out)
        }
        Shape::Array(_) => None,
    }
}

/// The sizes a shape's values take from other arguments: an array's
/// count, and its scalars' lengths.
pub(super) fn size_references<'s, 'd>(shape: &'s Shape<'d>) -> Vec<(&'s Dynamic<'d>, Measure)> {
    let mut out = Vec::new();
    match shape {
        Shape::Scalar(scalar) => {
            if let Some(Length::Counted(dynamic)) = &scalar.length {
                out.push((dynamic, Measure::Length));
            }
        }
        Shape::Aggregate(members) => {
            for member in members {
                out.extend(size_references(&member.shape));
            }
        }
        Shape::Array(array) => {
            if let Some(crate::types::Count::Counted { dynamic, .. }) = &array.count {
                out.push((dynamic, Measure::Count));
            }
            out.extend(size_references(&array.element));
        }
    }
    out
}

/// What a size argument measures: an array's count, or a value's length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Measure {
    Count,
    Length,
}

/// A size argument's field, once written: its name, the value assigned to
/// it if any, and the raw values it may hold.
#[derive(Clone)]
pub(super) struct SizeField {
    pub(super) name: String,
    pub(super) assigned: Option<i128>,
    pub(super) raw: (i128, i128),
}

/// A catalog name for a member: the argument's, then each member's name
/// as a catalog name part, joined by `_`.
pub(super) fn member_name(base: &str, path: &[&str]) -> Option<String> {
    let mut name = base.to_string();
    for member in path {
        name.push('_');
        name.push_str(&catalog_part(member)?);
    }
    Some(name)
}

/// A field name for a member: the argument's field name, then each
/// member's name as an identifier, joined by `_`; and whether a member's
/// name had to change.
pub(super) fn member_field(base: &str, path: &[&str]) -> (String, bool) {
    let mut name = base.to_string();
    let mut changed = false;
    for member in path {
        let (part, renamed) = identifier(member);
        name.push('_');
        name.push_str(&part);
        changed |= renamed;
    }
    (name, changed)
}

/// A JSON value's text as Yamcs reads a member's: a string as it is,
/// anything else as JSON writes it.
pub(super) fn json_text(value: &Value) -> Cow<'_, str> {
    match value {
        Value::String(text) => Cow::Borrowed(text),
        other => Cow::Owned(other.to_string()),
    }
}

/// The member names in a JSON value that no leaf names: Yamcs refuses
/// them.
pub(super) fn unknown_member(value: &Value, leaves: &[Leaf<'_>]) -> Option<String> {
    let known: Vec<Vec<&str>> = leaves.iter().map(|leaf| leaf.path.clone()).collect();
    unknown_below(value, &known)
}

/// The member names in a JSON value that none of the `known` paths starts
/// with.
fn unknown_below(value: &Value, known: &[Vec<&str>]) -> Option<String> {
    fn walk(value: &Value, prefix: &mut Vec<String>, known: &[Vec<String>]) -> Option<String> {
        let object = value.as_object()?;
        for (name, inner) in object {
            prefix.push(name.clone());
            let is_known = known.iter().any(|path| path.starts_with(prefix));
            if !is_known {
                return Some(prefix.join("."));
            }
            if inner.is_object()
                && !known.iter().any(|path| path == prefix)
                && let Some(found) = walk(inner, prefix, known)
            {
                return Some(found);
            }
            prefix.pop();
        }
        None
    }
    let known: Vec<Vec<String>> = known
        .iter()
        .map(|path| path.iter().map(|p| p.to_string()).collect())
        .collect();
    walk(value, &mut Vec::new(), &known)
}

impl<'d> Import<'d, '_> {
    /// The text of the value a leaf takes, as Yamcs completes an aggregate's
    /// value: from `given` (a JSON object of the aggregate's members) as deep
    /// as it reaches; from the first member it leaves out down, each
    /// member's own initial value, outermost first; then the leaf's type's.
    /// `Ok(None)` when nothing gives it one; `Err` after reporting a value
    /// that isn't what its member is.
    pub(super) fn member_value(
        &mut self,
        leaf: &Leaf<'d>,
        leaves: &[Leaf<'d>],
        argument: &str,
        given: Option<(&Value, Span)>,
    ) -> Result<Option<(String, Span)>, ()> {
        let mut current: Option<(Value, Span)> = given.map(|(value, span)| (value.clone(), span));
        let last = leaf.path.len().saturating_sub(1);
        for (level, member) in leaf.path.iter().enumerate() {
            let found = current.as_ref().and_then(|(value, span)| {
                value
                    .as_object()
                    .and_then(|object| object.get(*member))
                    .map(|value| (value.clone(), *span))
            });
            let (value, span) = match (found, leaf.initials.get(level).copied().flatten()) {
                (Some(found), _) => found,
                (None, Some((text, span))) if level == last => {
                    return Ok(Some((text.to_string(), span)));
                }
                (None, Some((text, span))) => {
                    let prefix = &leaf.path[..=level];
                    (
                        self.member_initial(text, span, argument, prefix, leaves)?,
                        span,
                    )
                }
                (None, None) if level == last => {
                    return Ok(leaf
                        .scalar
                        .initial
                        .map(|(text, span)| (text.to_string(), span)));
                }
                (None, None) => {
                    current = None;
                    continue;
                }
            };
            if level == last {
                return Ok(Some((json_text(&value).into_owned(), span)));
            }
            if !value.is_object() {
                self.report.error(
                    Code::Invalid,
                    span,
                    format!(
                        "member {} of argument {argument} is an aggregate, so its value is a JSON \
                         object of its members, not {value}, as Yamcs reads it",
                        leaf.path[..=level].join(".")
                    ),
                );
                return Err(());
            }
            current = Some((value, span));
        }
        Ok(None)
    }

    /// An aggregate member's initial value: a JSON object of its members,
    /// none of them unknown, as Yamcs reads it when it loads the type.
    pub(super) fn member_initial(
        &mut self,
        text: &str,
        span: Span,
        argument: &str,
        prefix: &[&'d str],
        leaves: &[Leaf<'d>],
    ) -> Result<Value, ()> {
        let shown = prefix.join(".");
        let value = match serde_json::from_str::<Value>(text) {
            Ok(value) if value.is_object() => value,
            _ => {
                self.report.error(
                    Code::Invalid,
                    span,
                    format!(
                        "member {shown} of argument {argument} is an aggregate, so its initial \
                         value is a JSON object of its members, not {text:?}, as Yamcs reads it"
                    ),
                );
                return Err(());
            }
        };
        let below: Vec<Vec<&str>> = leaves
            .iter()
            .filter_map(|leaf| leaf.path.strip_prefix(prefix).map(<[&str]>::to_vec))
            .collect();
        if let Some(unknown) = unknown_below(&value, &below) {
            self.report.error(
                Code::Invalid,
                span,
                format!("member {shown} of argument {argument} has no member {unknown}"),
            );
            return Err(());
        }
        Ok(value)
    }

    /// A size argument's field: an unsigned integer whose raw value is its
    /// value, computed (or a constant, when it's assigned).
    pub(super) fn check_size_field(
        &mut self,
        scalar: &Scalar<'d>,
        argument: &str,
        span: Span,
    ) -> Option<(i128, i128)> {
        let unsigned = scalar.ty.0 == "uint" && matches!(scalar.raw, Raw::Integer);
        let bits = scalar.size_bits.map(|(bits, _)| bits);
        match (unsigned, bits) {
            (true, Some(bits)) if (1..=64).contains(&bits) => {
                let high = (1_i128 << bits) - 1;
                let (low, high) = match &scalar.range {
                    Some(range) => (
                        range.min.map_or(0, |(min, _)| (min.ceil() as i128).max(0)),
                        range
                            .max
                            .map_or(high, |(max, _)| (max.floor() as i128).min(high)),
                    ),
                    None => (0, high),
                };
                if low > high {
                    self.report.error(
                        Code::Invalid,
                        span,
                        format!(
                            "argument {argument} gives a size, and its valid range lies outside \
                             the raw values its field holds, 0 to {}",
                            (1_i128 << bits) - 1
                        ),
                    );
                    return None;
                }
                Some((low, high))
            }
            _ => {
                self.report.error(
                    Code::Unsupported,
                    span,
                    format!(
                        "argument {argument} gives a size, and MDL takes a size from an unsigned \
                         integer field whose raw value is its value"
                    ),
                );
                None
            }
        }
    }

    /// The field a size reads, written earlier in the packet.
    pub(super) fn written_size<'s>(
        &mut self,
        dynamic: &Dynamic<'d>,
        sizes: &'s [(&'d str, SizeField)],
        measured: &str,
    ) -> Option<&'s SizeField> {
        let found = sizes
            .iter()
            .find(|(name, _)| *name == dynamic.reference)
            .map(|(_, field)| field);
        if found.is_none() {
            self.report.error(
                Code::Unsupported,
                dynamic.span,
                format!(
                    "argument {}, which gives the size of {measured}, isn't written before it, and \
                     MDL takes a size from a field written earlier",
                    dynamic.reference
                ),
            );
        }
        found
    }

    /// A string's or binary's length from a size argument's field: from the
    /// field, or fixed when the size is assigned (a string then NUL-padded,
    /// as Yamcs pads it).
    pub(super) fn size_length(
        &mut self,
        dynamic: &Dynamic<'d>,
        scalar: &Scalar<'d>,
        sizes: &[(&'d str, SizeField)],
        measured: &str,
    ) -> Option<Value> {
        let (multiply, add) = match dynamic.bytes() {
            Ok(linear) => linear,
            Err(reason) => {
                self.report.error(Code::Unsupported, dynamic.span, reason);
                return None;
            }
        };
        let field = self.written_size(dynamic, sizes, measured)?.clone();
        let Some(value) = field.assigned else {
            return Some(from_field(&field.name, multiply, add));
        };
        let bytes = value * i128::from(multiply) + i128::from(add);
        let Ok(bytes) = u32::try_from(bytes) else {
            self.report.error(
                Code::Invalid,
                dynamic.span,
                format!(
                    "argument {} gives {measured} {bytes} bytes",
                    dynamic.reference
                ),
            );
            return None;
        };
        Some(if scalar.ty.0 == "string" {
            json!({ "kind": "fixed", "bytes": bytes, "terminator": 0 })
        } else {
            json!({ "kind": "fixed", "bytes": bytes })
        })
    }

    /// The entries a scalar value is written by: a size tag before a
    /// Pascal string, then the field. `length` is worked out for a size
    /// another argument gives.
    pub(super) fn length_entries(
        &mut self,
        mut field: Field<'d>,
        sizes: &[(&'d str, SizeField)],
    ) -> Option<Vec<Entry<'d>>> {
        let mut out = Vec::new();
        match field.scalar.length.clone() {
            None => {}
            Some(Length::Written(length, span)) => field.length = Some((length, span)),
            Some(Length::Counted(dynamic)) => {
                let measured = format!("field {}", field.name);
                let length = self.size_length(&dynamic, &field.scalar, sizes, &measured)?;
                field.length = Some((length, dynamic.span));
            }
            Some(Length::Leading { bits, order, span }) => {
                let tag = format!("{}_size", field.name);
                out.push(Entry::Field(Box::new(Field {
                    name: tag.clone(),
                    key: None,
                    span: field.span,
                    scalar: size_tag(&field.scalar, bits, order, span),
                    length: None,
                    canonical: None,
                    argument: None,
                    value: None,
                    offset: field.offset.take(),
                    when: None,
                })));
                field.length = Some((json!({ "kind": "field", "field": tag }), span));
            }
        }
        out.push(Entry::Field(Box::new(field)));
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_names_join_with_underscores() {
        assert_eq!(
            member_name("target", &["Id", "Range-M"]).as_deref(),
            Some("target_id_range_m")
        );
        assert_eq!(
            member_field("Target", &["Id", "Range-M"]),
            ("Target_Id_Range_M".to_string(), true)
        );
        assert_eq!(member_name("x", &["--"]), None);
    }

    #[test]
    fn json_values_read_as_yamcs_reads_members() {
        let value: Value =
            serde_json::from_str(r#"{"a": 1, "b": {"c": "ON"}, "d": true}"#).unwrap();
        assert_eq!(json_text(&value["a"]), "1");
        assert_eq!(json_text(&value["b"]["c"]), "ON");
        assert_eq!(json_text(&value["d"]), "true");
        let known = [vec!["a"], vec!["b", "c"], vec!["d"]];
        assert_eq!(unknown_below(&value, &known), None);
        assert_eq!(unknown_below(&value, &known[..2]), Some("d".to_string()));
        let deeper: Value = serde_json::from_str(r#"{"b": {"x": 1}}"#).unwrap();
        assert_eq!(unknown_below(&deeper, &known), Some("b.x".to_string()));
    }
}
