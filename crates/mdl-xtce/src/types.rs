//! Parameter types: what a field reads and what its catalog param holds.

use std::collections::{BTreeMap, BTreeSet};

use mdl::v2::source::Span;
use mdl::v2::validate::MAX_NESTING;
use serde_json::{Value, json};

use crate::Code;
use crate::out::{List, Object};
use crate::report::Report;
use crate::space::{Kind, Tree};
use crate::units::{Ucum, ucum};
use crate::xml::{self, Attr, El, Expect};

/// What a parameter of a type is: one value, or an aggregate's members.
#[derive(Clone)]
pub(crate) enum Shape<'d> {
    Scalar(Box<Scalar<'d>>),
    Aggregate(Vec<Member<'d>>),
}

#[derive(Clone)]
pub(crate) struct Member<'d> {
    pub(crate) name: &'d str,
    pub(crate) description: Option<(String, Span)>,
    pub(crate) shape: Shape<'d>,
}

/// One value: how a field reads it, and what its param holds.
#[derive(Clone)]
pub(crate) struct Scalar<'d> {
    /// The type element.
    pub(crate) el: El<'d>,
    /// The field's type: `uint`, `int`, `float`, `bool`, `string` or
    /// `bytes`, and the encoding it comes from.
    pub(crate) ty: (&'static str, Span),
    pub(crate) size_bits: Option<(u32, Span)>,
    /// The encoding's byte order, for numbers wider than a byte.
    pub(crate) order: Option<(Order, Span)>,
    pub(crate) length: Option<(Value, Span)>,
    pub(crate) text: Option<(&'static str, Span)>,
    pub(crate) conversion: Option<Object>,
    pub(crate) unit: Option<(String, Span)>,
    /// The param's type: `u64`, `i64`, `f64`, `bool`, `str`, `bytes` or
    /// `enum`.
    pub(crate) param: &'static str,
    pub(crate) states: Vec<(String, Span)>,
    /// How a comparison's engineering value becomes a raw value.
    pub(crate) raw: Raw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Order {
    Big,
    Little,
}

impl Order {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Order::Big => "big",
            Order::Little => "little",
        }
    }
}

/// Engineering values a comparison may name, as raw values.
#[derive(Debug, Clone)]
pub(crate) enum Raw {
    /// Integers whose engineering value is the raw value.
    Integer,
    /// An enumeration's labels for its raw values; a label may stand for
    /// several.
    Labels(Vec<(String, i64)>),
    /// A boolean's labels for true and false: false is a raw 0, and true
    /// any other value.
    Boolean { one: String, zero: String },
    /// Engineering values that aren't raw values: calibrated, floats,
    /// strings, bytes.
    Other,
}

/// The raw values a compared value stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Wanted {
    One(i64),
    /// An enumeration label on several raw values.
    Any(Vec<i64>),
    /// A boolean's true, wider than one bit.
    NonZero,
}

/// The raw values a comparison of a field of type `ty` means: the
/// engineering value `value` when `calibrated`, else the raw value.
pub(crate) fn raw_value(
    ty: &str,
    raw: &Raw,
    value: &str,
    calibrated: bool,
) -> Result<Wanted, String> {
    let integer_field = matches!(ty, "uint" | "int" | "bool");
    let integer = || {
        parse_integer(value)
            .map(Wanted::One)
            .ok_or_else(|| format!("{value:?} isn't an integer"))
    };
    if !calibrated {
        return if integer_field {
            integer()
        } else {
            Err(format!(
                "it compares a raw {ty} value, and MDL compares only integers"
            ))
        };
    }
    match raw {
        Raw::Integer => integer(),
        Raw::Labels(labels) => {
            let raws: Vec<i64> = labels
                .iter()
                .filter(|(label, _)| label == value)
                .map(|(_, raw)| *raw)
                .collect();
            match raws.as_slice() {
                [] => {
                    let mut known: Vec<&str> = Vec::new();
                    for (label, _) in labels {
                        if !known.contains(&label.as_str()) {
                            known.push(label);
                        }
                    }
                    Err(format!(
                        "{value:?} isn't one of its values ({})",
                        known.join(", ")
                    ))
                }
                [one] => Ok(Wanted::One(*one)),
                _ => Ok(Wanted::Any(raws)),
            }
        }
        Raw::Boolean { one, zero } => {
            if value == one || value == "true" {
                Ok(if ty == "bool" {
                    Wanted::One(1)
                } else {
                    Wanted::NonZero
                })
            } else if value == zero || value == "false" {
                Ok(Wanted::One(0))
            } else {
                Err(format!(
                    "{value:?} isn't one of its values ({one}, {zero}, true, false)"
                ))
            }
        }
        Raw::Other => Err(
            "it compares an engineering value that isn't a raw value (calibrated, a float or \
                 text), and MDL compares raw values"
                .to_string(),
        ),
    }
}

/// A decimal integer, or `0x` hexadecimal.
fn parse_integer(text: &str) -> Option<i64> {
    let text = text.trim();
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let magnitude = match digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        Some(hex) => i128::from_str_radix(hex, 16).ok()?,
        None if digits.bytes().all(|b| b.is_ascii_digit()) && !digits.is_empty() => {
            digits.parse::<i128>().ok()?
        }
        None => return None,
    };
    i64::try_from(if negative { -magnitude } else { magnitude }).ok()
}

/// Shapes of the types used so far, each worked out once.
#[derive(Default)]
pub(crate) struct Types<'d> {
    shapes: BTreeMap<(usize, &'d str), Option<Shape<'d>>>,
    /// Aggregates being worked out, to catch one that contains itself.
    open: BTreeSet<(usize, &'d str)>,
}

/// Children every type may have that don't change decoded values.
const DESCRIBED: &[&str] = &[
    "shortDescription",
    "LongDescription",
    "AliasSet",
    "AncillaryDataSet",
];

const ENCODINGS: &[&str] = &[
    "IntegerDataEncoding",
    "FloatDataEncoding",
    "StringDataEncoding",
    "BinaryDataEncoding",
];

impl<'d> Types<'d> {
    /// The shape of the type `el`, defined in SpaceSystem `space`. Its
    /// problems are reported the first time it's asked for.
    pub(crate) fn shape(
        &mut self,
        tree: &Tree<'d>,
        space: usize,
        el: El<'d>,
        report: &mut Report,
    ) -> Option<Shape<'d>> {
        let key = (space, el.item_name().unwrap_or_default());
        if let Some(shape) = self.shapes.get(&key) {
            return shape.clone();
        }
        // Only aggregates contain types, so only they can nest or loop.
        let aggregate = el.name() == "AggregateParameterType";
        if aggregate {
            if self.open.contains(&key) {
                report.error(
                    Code::Invalid,
                    el.span(),
                    format!("aggregate type {} contains itself", key.1),
                );
                return None;
            }
            if self.open.len() >= MAX_NESTING {
                report.error(
                    Code::Unsupported,
                    el.span(),
                    format!(
                        "aggregates nested more than {MAX_NESTING} deep aren't supported: MDL \
                         bounds nesting there, as for structs"
                    ),
                );
                return None;
            }
            self.open.insert(key);
        }
        let shape = self.convert(tree, space, el, report);
        if aggregate {
            self.open.remove(&key);
        }
        self.shapes.insert(key, shape.clone());
        shape
    }

    fn convert(
        &mut self,
        tree: &Tree<'d>,
        space: usize,
        el: El<'d>,
        report: &mut Report,
    ) -> Option<Shape<'d>> {
        if let Some(base) = el.attr("baseType") {
            report.error(
                Code::Unsupported,
                base.span,
                "type inheritance (baseType) isn't supported; write the type out whole",
            );
            return None;
        }
        let scalar = match el.name() {
            "IntegerParameterType" => integer(el, report),
            "FloatParameterType" => float(el, report),
            "EnumeratedParameterType" => enumerated(el, report),
            "BooleanParameterType" => boolean(el, report),
            "StringParameterType" => string(el, report),
            "BinaryParameterType" => binary(el, report),
            "AggregateParameterType" => return self.aggregate(tree, space, el, report),
            "AbsoluteTimeParameterType" => {
                report.error(
                    Code::Unsupported,
                    el.span(),
                    "absolute time parameters aren't supported: an MDL field holds a number, \
                     not an instant, so the decoded value would change",
                );
                None
            }
            "RelativeTimeParameterType" | "ArrayParameterType" => {
                report.error(
                    Code::Unsupported,
                    el.span(),
                    format!("{} isn't supported yet", el.name()),
                );
                None
            }
            other => {
                report.error(
                    Code::Invalid,
                    el.span(),
                    format!("{other} isn't a parameter type"),
                );
                None
            }
        };
        scalar.map(|scalar| Shape::Scalar(Box::new(scalar)))
    }

    fn aggregate(
        &mut self,
        tree: &Tree<'d>,
        space: usize,
        el: El<'d>,
        report: &mut Report,
    ) -> Option<Shape<'d>> {
        el.check(
            &Expect {
                attrs: &["name"],
                children: &["MemberList"],
                ignored: DESCRIBED,
                text: false,
            },
            report,
        );
        let Some(list) = el.child("MemberList") else {
            report.error(
                Code::Invalid,
                el.span(),
                "an aggregate type lists its members",
            );
            return None;
        };
        list.check(
            &Expect {
                attrs: &[],
                children: &["Member"],
                ignored: &[],
                text: false,
            },
            report,
        );
        let mut members = Vec::new();
        let mut complete = true;
        for member in list.all("Member") {
            member.check(
                &Expect {
                    attrs: &["name", "typeRef", "shortDescription"],
                    children: &["LongDescription"],
                    ignored: &["AliasSet", "AncillaryDataSet", "initialValue"],
                    text: false,
                },
                report,
            );
            let (Some(name), Some(type_ref)) = (
                member.required("name", report),
                member.required("typeRef", report),
            ) else {
                complete = false;
                continue;
            };
            let shape = match tree.resolve(space, type_ref.value, Kind::ParameterType) {
                Ok(found) => self.shape(tree, found.space, found.el, report),
                Err(message) => {
                    report.error(Code::Reference, type_ref.span, message);
                    None
                }
            };
            match shape {
                Some(shape) => members.push(Member {
                    name: name.value,
                    description: description(member),
                    shape,
                }),
                None => complete = false,
            }
        }
        complete.then_some(Shape::Aggregate(members))
    }
}

fn integer<'d>(el: El<'d>, report: &mut Report) -> Option<Scalar<'d>> {
    el.check(
        &numeric_expect(&["name", "signed", "sizeInBits", "baseType"]),
        report,
    );
    let signed = el.attr("signed").and_then(|a| xml::boolean(a, report));
    if let Some(size) = el.attr("sizeInBits")
        && xml::long(size, report).is_some_and(|bits| !(1..=64).contains(&bits))
    {
        report.error(
            Code::Unsupported,
            size.span,
            "MDL's integers are at most 64 bits",
        );
    }
    let unit = unit(el, report);
    let encoding = only_encoding(el, &["IntegerDataEncoding"], report)?;
    let encoding = integer_encoding(encoding, report)?;
    if let Some(calibrator) = &encoding.calibrator {
        report.error(
            Code::Unsupported,
            calibrator.1,
            "an integer type with a calibrator: MDL's calibrations give floats, so the value \
             would change; make it a FloatParameterType",
        );
        return None;
    }
    Some(Scalar {
        param: if signed.unwrap_or(true) { "i64" } else { "u64" },
        raw: Raw::Integer,
        unit,
        ..encoding.scalar(el)
    })
}

fn float<'d>(el: El<'d>, report: &mut Report) -> Option<Scalar<'d>> {
    el.check(&numeric_expect(&["name", "sizeInBits", "baseType"]), report);
    let unit = unit(el, report);
    let encoding = only_encoding(el, &["FloatDataEncoding", "IntegerDataEncoding"], report)?;
    let (mut scalar, calibrated) = if encoding.name() == "FloatDataEncoding" {
        let encoding = float_encoding(encoding, report)?;
        let calibrated = encoding.calibrator.is_some();
        (encoding.scalar(el), calibrated)
    } else {
        let encoding = integer_encoding(encoding, report)?;
        let calibrated = encoding.calibrator.is_some();
        (encoding.scalar(el), calibrated)
    };
    scalar.param = "f64";
    scalar.unit = unit;
    scalar.raw = if calibrated || scalar.ty.0 == "float" {
        Raw::Other
    } else {
        Raw::Integer
    };
    Some(scalar)
}

fn enumerated<'d>(el: El<'d>, report: &mut Report) -> Option<Scalar<'d>> {
    el.check(
        &Expect {
            attrs: &["name", "baseType"],
            children: &[
                "UnitSet",
                "EnumerationList",
                "IntegerDataEncoding",
                "FloatDataEncoding",
                "StringDataEncoding",
                "BinaryDataEncoding",
            ],
            ignored: &[
                "shortDescription",
                "LongDescription",
                "AliasSet",
                "AncillaryDataSet",
                "initialValue",
                "DefaultAlarm",
                "ContextAlarmList",
            ],
            text: false,
        },
        report,
    );
    no_unit(el, report);
    let encoding = only_encoding(el, &["IntegerDataEncoding"], report)?;
    let encoding = integer_encoding(encoding, report)?;
    if let Some(calibrator) = &encoding.calibrator {
        report.error(
            Code::Unsupported,
            calibrator.1,
            "an enumerated type with a calibrator isn't supported",
        );
        return None;
    }
    let Some(list) = el.child("EnumerationList") else {
        report.error(
            Code::Invalid,
            el.span(),
            "an enumerated type lists its values",
        );
        return None;
    };
    list.check(
        &Expect {
            attrs: &[],
            children: &["Enumeration"],
            ignored: &[],
            text: false,
        },
        report,
    );
    let mut conversion_states = List::default();
    let mut labels = Vec::new();
    let mut states: Vec<(String, Span)> = Vec::new();
    for entry in list.all("Enumeration") {
        entry.check(
            &Expect {
                attrs: &["value", "label", "maxValue"],
                children: &[],
                ignored: &["shortDescription"],
                text: false,
            },
            report,
        );
        let (Some(value), Some(label)) = (
            entry.required("value", report),
            entry.required("label", report),
        ) else {
            continue;
        };
        let Some(raw) = xml::long(value, report) else {
            continue;
        };
        if let Some(max) = entry.attr("maxValue")
            && xml::long(max, report).is_some_and(|max| max != raw)
        {
            report.error(
                Code::Unsupported,
                max.span,
                "an enumeration over a range of values (maxValue) isn't supported",
            );
            continue;
        }
        let mut state = Object::at(entry.span());
        state.put("raw", raw, Some(value.span));
        state.put("name", label.value, Some(label.span));
        conversion_states.push_object(state);
        labels.push((label.value.to_string(), raw));
        if !states.iter().any(|(name, _)| name == label.value) {
            states.push((label.value.to_string(), label.span));
        }
    }
    if states.is_empty() {
        report.error(Code::Invalid, list.span(), "an enumeration list has values");
        return None;
    }
    let mut conversion = Object::at(list.span());
    conversion.put("kind", "states", None);
    conversion.put_list("states", conversion_states);
    Some(Scalar {
        conversion: Some(conversion),
        param: "enum",
        states,
        raw: Raw::Labels(labels),
        ..encoding.scalar(el)
    })
}

fn boolean<'d>(el: El<'d>, report: &mut Report) -> Option<Scalar<'d>> {
    el.check(
        &Expect {
            attrs: &["name", "baseType", "oneStringValue", "zeroStringValue"],
            children: &[
                "UnitSet",
                "IntegerDataEncoding",
                "FloatDataEncoding",
                "StringDataEncoding",
                "BinaryDataEncoding",
            ],
            ignored: &[
                "shortDescription",
                "LongDescription",
                "AliasSet",
                "AncillaryDataSet",
                "initialValue",
                "DefaultAlarm",
                "ContextAlarmList",
            ],
            text: false,
        },
        report,
    );
    no_unit(el, report);
    let one = el.attr("oneStringValue");
    let zero = el.attr("zeroStringValue");
    let one_label = one.map_or("True", |a| a.value);
    let zero_label = zero.map_or("False", |a| a.value);
    if (one_label, zero_label) != ("True", "False") {
        let span = one.or(zero).map_or(el.span(), |a| a.span);
        report.warning(
            Code::Ignored,
            span,
            format!(
                "type {}'s labels ({one_label}, {zero_label}) aren't imported: MDL booleans are \
                 true and false",
                el.item_name().unwrap_or_default()
            ),
        );
    }
    let encoding = only_encoding(el, &["IntegerDataEncoding"], report)?;
    let encoding = integer_encoding(encoding, report)?;
    if let Some(calibrator) = &encoding.calibrator {
        report.error(
            Code::Unsupported,
            calibrator.1,
            "a boolean type with a calibrator isn't supported",
        );
        return None;
    }
    let mut scalar = encoding.scalar(el);
    if encoding.size == 1 {
        scalar.ty.0 = "bool";
    }
    scalar.param = "bool";
    scalar.raw = Raw::Boolean {
        one: one_label.to_string(),
        zero: zero_label.to_string(),
    };
    Some(scalar)
}

fn string<'d>(el: El<'d>, report: &mut Report) -> Option<Scalar<'d>> {
    el.check(
        &Expect {
            attrs: &["name", "baseType"],
            children: &[
                "UnitSet",
                "IntegerDataEncoding",
                "FloatDataEncoding",
                "StringDataEncoding",
                "BinaryDataEncoding",
            ],
            ignored: &[
                "shortDescription",
                "LongDescription",
                "AliasSet",
                "AncillaryDataSet",
                "initialValue",
                "restrictionPattern",
                "SizeRangeInCharacters",
                "DefaultAlarm",
                "ContextAlarmList",
            ],
            text: false,
        },
        report,
    );
    no_unit(el, report);
    let encoding = only_encoding(el, &["StringDataEncoding"], report)?;
    encoding.check(
        &Expect {
            attrs: &["encoding", "byteOrder", "bitOrder"],
            children: &["SizeInBits", "Variable"],
            ignored: &[],
            text: false,
        },
        report,
    );
    msb_first(encoding, report)?;
    let text = match encoding.attr("encoding") {
        None => None,
        Some(attr) => match attr.value.trim() {
            "UTF-8" => None,
            "US-ASCII" => Some(("ascii", attr.span)),
            "ISO-8859-1" => Some(("latin1", attr.span)),
            other => {
                report.error(
                    Code::Unsupported,
                    attr.span,
                    format!(
                        "{other} text isn't supported: MDL reads UTF-8, US-ASCII and ISO-8859-1"
                    ),
                );
                return None;
            }
        },
    };
    let length = string_length(encoding, report)?;
    Some(Scalar {
        el,
        ty: ("string", encoding.span()),
        size_bits: None,
        order: None,
        length: Some(length),
        text,
        conversion: None,
        unit: None,
        param: "str",
        states: Vec::new(),
        raw: Raw::Other,
    })
}

/// A string's length: fixed, or up to a terminator.
fn string_length(encoding: El<'_>, report: &mut Report) -> Option<(Value, Span)> {
    if let Some(size) = encoding.child("SizeInBits") {
        size.check(
            &Expect {
                attrs: &[],
                children: &["Fixed", "TerminationChar", "LeadingSize"],
                ignored: &[],
                text: false,
            },
            report,
        );
        if let Some(leading) = size.child("LeadingSize") {
            report.error(
                Code::Unsupported,
                leading.span(),
                "strings with a leading size aren't supported yet",
            );
            return None;
        }
        let terminator = size.child("TerminationChar");
        return match (size.child("Fixed"), terminator) {
            (Some(fixed), None) => {
                fixed.check(
                    &Expect {
                        attrs: &[],
                        children: &["FixedValue"],
                        ignored: &[],
                        text: false,
                    },
                    report,
                );
                let Some(value) = fixed.child("FixedValue") else {
                    report.error(Code::Invalid, fixed.span(), "Fixed holds a FixedValue");
                    return None;
                };
                let bytes = whole_bytes(value, report)?;
                Some((json!({ "kind": "fixed", "bytes": bytes }), value.span()))
            }
            (None, Some(terminator)) => terminated(terminator, report),
            (Some(_), Some(terminator)) => {
                report.error(
                    Code::Unsupported,
                    terminator.span(),
                    "a terminated string in a fixed-size buffer isn't supported: MDL's fixed \
                     strings keep every byte, so the value would change",
                );
                None
            }
            (None, None) => {
                report.error(
                    Code::Invalid,
                    size.span(),
                    "a string's SizeInBits is Fixed or a TerminationChar",
                );
                None
            }
        };
    }
    if let Some(variable) = encoding.child("Variable") {
        variable.check(
            &Expect {
                attrs: &[],
                children: &[
                    "TerminationChar",
                    "LeadingSize",
                    "DynamicValue",
                    "DiscreteLookupList",
                ],
                ignored: &["maxSizeInBits"],
                text: false,
            },
            report,
        );
        return match variable.child("TerminationChar") {
            Some(terminator) => terminated(terminator, report),
            None => {
                report.error(
                    Code::Unsupported,
                    variable.span(),
                    "variable strings are supported only up to a TerminationChar yet",
                );
                None
            }
        };
    }
    report.error(
        Code::Invalid,
        encoding.span(),
        "a StringDataEncoding has a SizeInBits or a Variable size",
    );
    None
}

fn terminated(terminator: El<'_>, report: &mut Report) -> Option<(Value, Span)> {
    let (text, span) = terminator.text();
    match (text.len(), u8::from_str_radix(&text, 16)) {
        (2, Ok(byte)) => Some((json!({ "kind": "terminator", "byte": byte }), span)),
        _ => {
            report.error(
                Code::Unsupported,
                span,
                format!("terminator {text:?} isn't one byte in hexadecimal, as MDL's are"),
            );
            None
        }
    }
}

fn binary<'d>(el: El<'d>, report: &mut Report) -> Option<Scalar<'d>> {
    el.check(
        &Expect {
            attrs: &["name", "baseType"],
            children: &[
                "UnitSet",
                "IntegerDataEncoding",
                "FloatDataEncoding",
                "StringDataEncoding",
                "BinaryDataEncoding",
            ],
            ignored: &[
                "shortDescription",
                "LongDescription",
                "AliasSet",
                "AncillaryDataSet",
                "initialValue",
                "DefaultAlarm",
                "ContextAlarmList",
            ],
            text: false,
        },
        report,
    );
    no_unit(el, report);
    let encoding = only_encoding(el, &["BinaryDataEncoding"], report)?;
    encoding.check(
        &Expect {
            attrs: &["byteOrder", "bitOrder"],
            children: &[
                "SizeInBits",
                "FromBinaryTransformAlgorithm",
                "ToBinaryTransformAlgorithm",
            ],
            ignored: &[],
            text: false,
        },
        report,
    );
    msb_first(encoding, report)?;
    for name in ["FromBinaryTransformAlgorithm", "ToBinaryTransformAlgorithm"] {
        if let Some(algorithm) = encoding.child(name) {
            report.error(
                Code::Unsupported,
                algorithm.span(),
                "binary transform algorithms aren't supported",
            );
            return None;
        }
    }
    let Some(size) = encoding.child("SizeInBits") else {
        report.error(
            Code::Invalid,
            encoding.span(),
            "a BinaryDataEncoding has a SizeInBits",
        );
        return None;
    };
    size.check(
        &Expect {
            attrs: &[],
            children: &["FixedValue", "DynamicValue", "DiscreteLookupList"],
            ignored: &[],
            text: false,
        },
        report,
    );
    let Some(value) = size.child("FixedValue") else {
        report.error(
            Code::Unsupported,
            size.span(),
            "binary values of a dynamic size aren't supported yet",
        );
        return None;
    };
    let bytes = whole_bytes(value, report)?;
    Some(Scalar {
        el,
        ty: ("bytes", encoding.span()),
        size_bits: None,
        order: None,
        length: Some((json!({ "kind": "fixed", "bytes": bytes }), value.span())),
        text: None,
        conversion: None,
        unit: None,
        param: "bytes",
        states: Vec::new(),
        raw: Raw::Other,
    })
}

/// A FixedValue in bits as whole bytes.
fn whole_bytes(value: El<'_>, report: &mut Report) -> Option<u32> {
    let (text, span) = value.text();
    let bits = xml::long_text(&text, "FixedValue", span, report)?;
    match u32::try_from(bits / 8) {
        Ok(bytes) if bits > 0 && bits % 8 == 0 => Some(bytes),
        _ => {
            report.error(
                Code::Unsupported,
                span,
                format!(
                    "{bits} bits isn't a whole number of bytes, as MDL's strings and bytes are"
                ),
            );
            None
        }
    }
}

/// Number types' attributes and children, with `attrs` read.
fn numeric_expect<'s>(attrs: &'s [&'s str]) -> Expect<'s> {
    Expect {
        attrs,
        children: &[
            "UnitSet",
            "IntegerDataEncoding",
            "FloatDataEncoding",
            "StringDataEncoding",
            "BinaryDataEncoding",
        ],
        ignored: &[
            "shortDescription",
            "LongDescription",
            "AliasSet",
            "AncillaryDataSet",
            "initialValue",
            "ToString",
            "ValidRange",
            "validRangeAppliesToCalibrated",
            "DefaultAlarm",
            "ContextAlarmList",
        ],
        text: false,
    }
}

/// The type's one data encoding, which must be one of `allowed`.
fn only_encoding<'d>(el: El<'d>, allowed: &[&str], report: &mut Report) -> Option<El<'d>> {
    let name = el.item_name().unwrap_or_default();
    let mut encodings = el.children().filter(|c| ENCODINGS.contains(&c.name()));
    let Some(encoding) = encodings.next() else {
        report.error(
            Code::Invalid,
            el.span(),
            format!("type {name} has no data encoding, so it can't be read from a packet"),
        );
        return None;
    };
    if let Some(second) = encodings.next() {
        report.error(Code::Invalid, second.span(), "a type has one data encoding");
        return None;
    }
    if !allowed.contains(&encoding.name()) {
        let what = match encoding.name() {
            "StringDataEncoding" => "a value written as text".to_string(),
            other => format!("{other} for a {}", el.name()),
        };
        report.error(
            Code::Unsupported,
            encoding.span(),
            format!("{what} isn't supported"),
        );
        return None;
    }
    Some(encoding)
}

/// A number's encoding.
struct Encoding<'d> {
    el: El<'d>,
    ty: &'static str,
    size: u32,
    size_span: Span,
    order: Option<(Order, Span)>,
    calibrator: Option<(Object, Span)>,
}

impl<'d> Encoding<'d> {
    fn scalar(&self, type_el: El<'d>) -> Scalar<'d> {
        Scalar {
            el: type_el,
            ty: (self.ty, self.el.span()),
            size_bits: Some((self.size, self.size_span)),
            order: self.order,
            length: None,
            text: None,
            conversion: self.calibrator.as_ref().map(|(object, _)| object.clone()),
            unit: None,
            param: "f64",
            states: Vec::new(),
            raw: Raw::Other,
        }
    }
}

fn integer_encoding<'d>(el: El<'d>, report: &mut Report) -> Option<Encoding<'d>> {
    el.check(
        &Expect {
            attrs: &["sizeInBits", "encoding", "byteOrder", "bitOrder"],
            children: &[
                "DefaultCalibrator",
                "ContextCalibratorList",
                "ByteOrderList",
                "ErrorDetectCorrect",
            ],
            ignored: &["changeThreshold"],
            text: false,
        },
        report,
    );
    unsupported_children(el, report)?;
    msb_first(el, report)?;
    let (size, size_span) = match el.attr("sizeInBits") {
        Some(attr) => (xml::long(attr, report)?, attr.span),
        None => (8, el.span()),
    };
    let size = match u32::try_from(size) {
        Ok(size) if (1..=64).contains(&size) => size,
        _ => {
            report.error(
                Code::Unsupported,
                size_span,
                format!("a {size}-bit integer isn't supported: MDL reads 1 to 64 bits"),
            );
            return None;
        }
    };
    let ty = match el.attr("encoding") {
        None => "uint",
        Some(attr) => match attr.value.trim() {
            "unsigned" => "uint",
            // XTCE 1.0 and 1.1 spell it this way.
            "twosComplement" | "twosCompliment" => "int",
            other => {
                report.error(
                    Code::Unsupported,
                    attr.span,
                    format!(
                        "{other} integers aren't supported: MDL reads unsigned and two's \
                         complement"
                    ),
                );
                return None;
            }
        },
    };
    let order = byte_order(el, size, report)?;
    let calibrator = match el.child("DefaultCalibrator") {
        Some(calibrator) => Some(default_calibrator(calibrator, report)?),
        None => None,
    };
    Some(Encoding {
        el,
        ty,
        size,
        size_span,
        order,
        calibrator,
    })
}

fn float_encoding<'d>(el: El<'d>, report: &mut Report) -> Option<Encoding<'d>> {
    el.check(
        &Expect {
            attrs: &["sizeInBits", "encoding", "byteOrder", "bitOrder"],
            children: &[
                "DefaultCalibrator",
                "ContextCalibratorList",
                "ByteOrderList",
                "ErrorDetectCorrect",
            ],
            ignored: &["changeThreshold"],
            text: false,
        },
        report,
    );
    unsupported_children(el, report)?;
    msb_first(el, report)?;
    if let Some(attr) = el.attr("encoding") {
        let value = attr.value.trim();
        if !matches!(value, "IEEE754_1985" | "IEEE754") {
            report.error(
                Code::Unsupported,
                attr.span,
                format!("{value} floats aren't supported: MDL reads IEEE 754"),
            );
            return None;
        }
    }
    let (size, size_span) = match el.attr("sizeInBits") {
        Some(attr) => (xml::long(attr, report)?, attr.span),
        None => (32, el.span()),
    };
    let size = match size {
        32 => 32,
        64 => 64,
        other => {
            report.error(
                Code::Unsupported,
                size_span,
                format!("{other}-bit floats aren't supported: MDL reads binary32 and binary64"),
            );
            return None;
        }
    };
    let order = byte_order(el, size, report)?;
    let calibrator = match el.child("DefaultCalibrator") {
        Some(calibrator) => Some(default_calibrator(calibrator, report)?),
        None => None,
    };
    Some(Encoding {
        el,
        ty: "float",
        size,
        size_span,
        order,
        calibrator,
    })
}

fn unsupported_children(el: El<'_>, report: &mut Report) -> Option<()> {
    if let Some(context) = el.child("ContextCalibratorList") {
        report.error(
            Code::Unsupported,
            context.span(),
            "context calibrators (a calibration chosen by a condition) aren't supported",
        );
        return None;
    }
    if let Some(check) = el.child("ErrorDetectCorrect") {
        report.error(
            Code::Unsupported,
            check.span(),
            "error detection on a single value isn't supported",
        );
        return None;
    }
    Some(())
}

/// Refuses least significant bit first, which MDL doesn't read.
fn msb_first(el: El<'_>, report: &mut Report) -> Option<()> {
    match el.attr("bitOrder") {
        Some(attr) if attr.value.trim() == "leastSignificantBitFirst" => {
            report.error(
                Code::Unsupported,
                attr.span,
                "least significant bit first isn't supported: MDL numbers bits from the most \
                 significant",
            );
            None
        }
        Some(attr) if attr.value.trim() != "mostSignificantBitFirst" => {
            report.error(
                Code::Invalid,
                attr.span,
                format!("bitOrder is {:?}, not a bit order", attr.value),
            );
            None
        }
        _ => Some(()),
    }
}

/// The byte order of a number wider than a byte; `None` for one that fits
/// in a byte, where it doesn't apply.
fn byte_order(el: El<'_>, size: u32, report: &mut Report) -> Option<Option<(Order, Span)>> {
    let attr = el.attr("byteOrder");
    let list = el.child("ByteOrderList");
    let order = match (attr, list) {
        (Some(attr), _) => (attribute_order(attr, report)?, attr.span),
        (None, Some(list)) => (list_order(list, size, report)?, list.span()),
        (None, None) => (Order::Big, el.span()),
    };
    if order.0 == Order::Little && size > 8 && !size.is_multiple_of(8) {
        report.error(
            Code::Unsupported,
            order.1,
            format!(
                "a little-endian value of {size} bits isn't supported: MDL reads little-endian \
                 values wider than a byte as whole bytes"
            ),
        );
        return None;
    }
    Some((size > 8).then_some(order))
}

fn attribute_order(attr: Attr<'_>, report: &mut Report) -> Option<Order> {
    match attr.value.trim() {
        "mostSignificantByteFirst" => Some(Order::Big),
        "leastSignificantByteFirst" => Some(Order::Little),
        other => {
            report.error(
                Code::Unsupported,
                attr.span,
                format!("byte order {other:?} isn't supported: MDL reads big- and little-endian"),
            );
            None
        }
    }
}

/// A ByteOrderList that's big- or little-endian.
fn list_order(list: El<'_>, size: u32, report: &mut Report) -> Option<Order> {
    list.check(
        &Expect {
            attrs: &[],
            children: &["Byte"],
            ignored: &[],
            text: false,
        },
        report,
    );
    let mut significance = Vec::new();
    for byte in list.all("Byte") {
        let attr = byte.required("byteSignificance", report)?;
        significance.push(xml::long(attr, report)?);
    }
    let bytes = size / 8;
    if !size.is_multiple_of(8) || usize::try_from(bytes).ok() != Some(significance.len()) {
        report.error(
            Code::Invalid,
            list.span(),
            format!(
                "a ByteOrderList names each byte of the value, and this one names {} for {size} \
                 bits",
                significance.len()
            ),
        );
        return None;
    }
    let count = i64::from(bytes);
    if significance.iter().copied().eq((0..count).rev()) {
        Some(Order::Big)
    } else if significance.iter().copied().eq(0..count) {
        Some(Order::Little)
    } else {
        report.error(
            Code::Unsupported,
            list.span(),
            "a byte order that's neither big- nor little-endian isn't supported",
        );
        None
    }
}

/// A DefaultCalibrator as an MDL conversion.
fn default_calibrator(el: El<'_>, report: &mut Report) -> Option<(Object, Span)> {
    el.check(
        &Expect {
            attrs: &[],
            children: &[
                "PolynomialCalibrator",
                "SplineCalibrator",
                "MathOperationCalibrator",
            ],
            ignored: &[
                "name",
                "shortDescription",
                "LongDescription",
                "AliasSet",
                "AncillaryDataSet",
            ],
            text: false,
        },
        report,
    );
    let span = el.span();
    if let Some(polynomial) = el.child("PolynomialCalibrator") {
        return Some((polynomial_conversion(polynomial, report)?, span));
    }
    if let Some(spline) = el.child("SplineCalibrator") {
        return Some((spline_conversion(spline, report)?, span));
    }
    if let Some(math) = el.child("MathOperationCalibrator") {
        report.error(
            Code::Unsupported,
            math.span(),
            "math operation calibrators aren't supported",
        );
        return None;
    }
    report.error(
        Code::Invalid,
        span,
        "a DefaultCalibrator holds a calibrator",
    );
    None
}

/// The highest polynomial degree imported.
const MAX_DEGREE: usize = 32;

fn polynomial_conversion(el: El<'_>, report: &mut Report) -> Option<Object> {
    el.check(
        &Expect {
            attrs: &[],
            children: &["Term"],
            ignored: &[
                "name",
                "shortDescription",
                "LongDescription",
                "AliasSet",
                "AncillaryDataSet",
            ],
            text: false,
        },
        report,
    );
    let mut coefficients: Vec<f64> = Vec::new();
    for term in el.all("Term") {
        term.check(
            &Expect {
                attrs: &["coefficient", "exponent"],
                children: &[],
                ignored: &[],
                text: false,
            },
            report,
        );
        let coefficient = xml::double(term.required("coefficient", report)?, report)?;
        let exponent_attr = term.required("exponent", report)?;
        let exponent = xml::long(exponent_attr, report)?;
        let exponent = match usize::try_from(exponent) {
            Ok(e) if e <= MAX_DEGREE => e,
            _ => {
                report.error(
                    Code::Unsupported,
                    exponent_attr.span,
                    format!("exponent {exponent} isn't supported: MDL's polynomials go to degree {MAX_DEGREE}"),
                );
                return None;
            }
        };
        if coefficients.len() <= exponent {
            coefficients.resize(exponent + 1, 0.0);
        }
        coefficients[exponent] += coefficient;
    }
    while coefficients.len() > 1 && coefficients.last() == Some(&0.0) {
        coefficients.pop();
    }
    let mut conversion = Object::at(el.span());
    match coefficients.as_slice() {
        [] => {
            report.error(Code::Invalid, el.span(), "a polynomial has terms");
            return None;
        }
        [offset] => {
            conversion.put("kind", "linear", None);
            conversion.put("scale", 0.0, None);
            conversion.put("offset", *offset, None);
        }
        [offset, scale] => {
            conversion.put("kind", "linear", None);
            conversion.put("scale", *scale, None);
            conversion.put("offset", *offset, None);
        }
        _ => {
            conversion.put("kind", "poly", None);
            conversion.put("coefficients", coefficients.clone(), None);
        }
    }
    Some(conversion)
}

fn spline_conversion(el: El<'_>, report: &mut Report) -> Option<Object> {
    el.check(
        &Expect {
            attrs: &["order", "extrapolate"],
            children: &["SplinePoint"],
            ignored: &[
                "name",
                "shortDescription",
                "LongDescription",
                "AliasSet",
                "AncillaryDataSet",
            ],
            text: false,
        },
        report,
    );
    let order = match el.attr("order") {
        Some(attr) => (xml::long(attr, report)?, attr.span),
        None => (1, el.span()),
    };
    let interpolation = match order.0 {
        0 => Some("step"),
        1 => None,
        n => {
            report.error(
                Code::Unsupported,
                order.1,
                format!(
                    "order {n} splines aren't supported: MDL interpolates flat (0) or linearly (1)"
                ),
            );
            return None;
        }
    };
    let extrapolate = match el.attr("extrapolate") {
        Some(attr) => xml::boolean(attr, report)?.then_some(attr.span),
        None => None,
    };
    let mut points = List::default();
    for point in el.all("SplinePoint") {
        point.check(
            &Expect {
                attrs: &["raw", "calibrated", "order"],
                children: &[],
                ignored: &[],
                text: false,
            },
            report,
        );
        let raw = point.required("raw", report)?;
        let calibrated = point.required("calibrated", report)?;
        if let Some(own) = point.attr("order")
            && xml::long(own, report)? != order.0
        {
            report.error(
                Code::Unsupported,
                own.span,
                "a spline whose points interpolate differently isn't supported",
            );
            return None;
        }
        let mut object = Object::at(point.span());
        object.put("raw", xml::double(raw, report)?, Some(raw.span));
        object.put(
            "value",
            xml::double(calibrated, report)?,
            Some(calibrated.span),
        );
        points.push_object(object);
    }
    let mut conversion = Object::at(el.span());
    conversion.put("kind", "table", None);
    conversion.put_list("points", points);
    if let Some(interpolation) = interpolation {
        conversion.put("interpolation", interpolation, Some(order.1));
    }
    if let Some(span) = extrapolate {
        conversion.put("extrapolate", true, Some(span));
    }
    Some(conversion)
}

/// An item's description: its shortDescription and LongDescription, both
/// when it has both.
pub(crate) fn description(el: El<'_>) -> Option<(String, Span)> {
    let short = el
        .attr("shortDescription")
        .map(|a| (a.value.trim(), a.span))
        .filter(|(text, _)| !text.is_empty());
    let long = el
        .child("LongDescription")
        .map(|l| l.text())
        .filter(|(text, _)| !text.is_empty());
    match (short, long) {
        (Some((short, span)), Some((long, _))) => Some((format!("{short}\n\n{long}"), span)),
        (Some((text, span)), None) => Some((text.to_string(), span)),
        (None, Some((text, span))) => Some((text.into_owned(), span)),
        (None, None) => None,
    }
}

/// A number type's unit, as a UCUM code.
fn unit(el: El<'_>, report: &mut Report) -> Option<(String, Span)> {
    let set = el.child("UnitSet")?;
    set.check(
        &Expect {
            attrs: &[],
            children: &["Unit"],
            ignored: &[],
            text: false,
        },
        report,
    );
    let units: Vec<El<'_>> = set.all("Unit").collect();
    let [only] = units.as_slice() else {
        if let Some(second) = units.get(1) {
            report.warning(
                Code::Ignored,
                second.span(),
                "a unit made of several Units isn't imported; the value is unchanged",
            );
        }
        return None;
    };
    only.check(
        &Expect {
            attrs: &["power", "factor", "form"],
            children: &[],
            ignored: &["description"],
            text: true,
        },
        report,
    );
    let plain = only.attr("power").is_none_or(|a| a.value.trim() == "1")
        && only.attr("factor").is_none_or(|a| a.value.trim() == "1")
        && only
            .attr("form")
            .is_none_or(|a| a.value.trim() == "calibrated");
    let (text, span) = only.text();
    if !plain {
        report.warning(
            Code::Ignored,
            span,
            "a unit with a power, a factor or a raw form isn't imported; the value is unchanged",
        );
        return None;
    }
    if text.is_empty() {
        return None;
    }
    match ucum(&text) {
        Ucum::Code(code) => Some((code, span)),
        Ucum::Ambiguous {
            reads,
            meant,
            write,
        } => {
            report.warning(
                Code::Ignored,
                span,
                format!(
                    "unit {text:?} isn't imported: UCUM reads it as the {reads}, but XTCE documents \
                     often mean {meant} by it; write {write} for {meant} (a {reads} can't be \
                     imported as {text}); the value is unchanged"
                ),
            );
            None
        }
        Ucum::Unknown => {
            report.warning(
                Code::Ignored,
                span,
                format!(
                    "unit {text:?} isn't a UCUM code MDL knows, so it isn't imported; the value \
                     is unchanged"
                ),
            );
            None
        }
    }
}

/// Units on types whose values aren't quantities.
fn no_unit(el: El<'_>, report: &mut Report) {
    if let Some(set) = el.child("UnitSet")
        && let Some(unit) = set.child("Unit")
    {
        report.warning(
            Code::Ignored,
            unit.span(),
            format!(
                "a {}'s unit isn't imported: its values aren't quantities",
                el.name()
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_are_decimal_or_hex() {
        assert_eq!(parse_integer("100"), Some(100));
        assert_eq!(parse_integer(" -7 "), Some(-7));
        assert_eq!(parse_integer("+3"), Some(3));
        assert_eq!(parse_integer("0x64"), Some(100));
        assert_eq!(parse_integer("-0x10"), Some(-16));
        assert_eq!(parse_integer("1.0"), None);
        assert_eq!(parse_integer(""), None);
        assert_eq!(parse_integer("9223372036854775808"), None);
        assert_eq!(parse_integer("-9223372036854775808"), Some(i64::MIN));
    }
}
