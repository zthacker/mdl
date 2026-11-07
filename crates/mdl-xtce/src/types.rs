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

/// What a parameter of a type is: one value, an aggregate's members, or
/// an array of elements.
#[derive(Clone)]
pub(crate) enum Shape<'d> {
    Scalar(Box<Scalar<'d>>),
    Aggregate(Vec<Member<'d>>),
    Array(Box<ArrayShape<'d>>),
}

/// An array type: its elements, and how many there are.
#[derive(Clone)]
pub(crate) struct ArrayShape<'d> {
    pub(crate) el: El<'d>,
    pub(crate) element: Shape<'d>,
    /// `None` for an XTCE 1.1 array, whose container entry gives its size.
    pub(crate) count: Option<Count<'d>>,
}

/// How many elements an array has.
#[derive(Clone)]
pub(crate) enum Count<'d> {
    Fixed(u32, Span),
    /// From a dimension whose ending index another parameter gives.
    Counted {
        dynamic: Dynamic<'d>,
        start: i64,
    },
}

/// A value's length, before the fields around it are known.
#[derive(Clone)]
pub(crate) enum Length<'d> {
    /// As the IR writes it: fixed, terminated, or both.
    Written(Value, Span),
    /// A size in bits another parameter gives.
    Counted(Dynamic<'d>),
    /// A Pascal string: a size tag of this many bits, in the encoding's
    /// byte order, then that many bytes.
    Leading {
        bits: u32,
        order: Option<(Order, Span)>,
        span: Span,
    },
}

/// XTCE's DynamicValue: another parameter's value, times `slope`, plus
/// `intercept`. The reference is resolved where the value is used, since
/// only the container around it says whether that parameter is there.
#[derive(Debug, Clone)]
pub(crate) struct Dynamic<'d> {
    /// The SpaceSystem the reference is made from.
    pub(crate) space: usize,
    pub(crate) reference: &'d str,
    pub(crate) span: Span,
    pub(crate) calibrated: bool,
    pub(crate) slope: i64,
    pub(crate) intercept: i64,
}

impl Dynamic<'_> {
    /// A size in bits as MDL's `raw × multiply + add` bytes.
    pub(crate) fn bytes(&self) -> Result<(u32, i64), String> {
        if self.slope % 8 != 0 || self.intercept % 8 != 0 {
            return Err(format!(
                "a size of {} × {} + {} bits isn't whole bytes for every value, and MDL's lengths \
                 are bytes",
                self.reference, self.slope, self.intercept
            ));
        }
        linear(self.slope / 8, self.intercept / 8)
    }

    /// A count of elements from an ending index, the first index being
    /// `start`: `raw × slope + intercept − start + 1`.
    pub(crate) fn count(&self, start: i64) -> Result<(u32, i64), String> {
        let add = self
            .intercept
            .checked_sub(start)
            .and_then(|n| n.checked_add(1))
            .ok_or_else(|| "a count out of range".to_string())?;
        linear(self.slope, add)
    }
}

fn linear(multiply: i64, add: i64) -> Result<(u32, i64), String> {
    match u32::try_from(multiply) {
        Ok(multiply) if multiply > 0 => Ok((multiply, add)),
        _ => Err(format!(
            "a slope of {multiply} isn't supported: MDL takes a size from a field times a \
             positive whole number"
        )),
    }
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
    pub(crate) length: Option<Length<'d>>,
    pub(crate) text: Option<(&'static str, Span)>,
    pub(crate) conversion: Option<Object>,
    pub(crate) unit: Option<(String, Span)>,
    /// The param's type: `u64`, `i64`, `f64`, `bool`, `str`, `bytes` or
    /// `enum`.
    pub(crate) param: &'static str,
    pub(crate) states: Vec<(String, Span)>,
    /// How a comparison's engineering value becomes a raw value.
    pub(crate) raw: Raw,
    /// A number type's alarm ranges, as the param's limits.
    pub(crate) limits: Option<Limits>,
}

/// Alarm ranges as MDL's limits, each with where it came from.
#[derive(Debug, Clone)]
pub(crate) struct Limits {
    pub(crate) span: Span,
    pub(crate) red_low: Option<(f64, Span)>,
    pub(crate) yellow_low: Option<(f64, Span)>,
    pub(crate) yellow_high: Option<(f64, Span)>,
    pub(crate) red_high: Option<(f64, Span)>,
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
        // Only aggregates and arrays contain types, so only they can nest
        // or loop.
        let container = matches!(el.name(), "AggregateParameterType" | "ArrayParameterType");
        if container {
            if self.open.contains(&key) {
                report.error(
                    Code::Invalid,
                    el.span(),
                    format!("type {} contains itself", key.1),
                );
                return None;
            }
            if self.open.len() >= MAX_NESTING {
                report.error(
                    Code::Unsupported,
                    el.span(),
                    format!(
                        "aggregates and arrays nested more than {MAX_NESTING} deep aren't \
                         supported: MDL bounds nesting there, as for structs"
                    ),
                );
                return None;
            }
            self.open.insert(key);
        }
        let shape = self.convert(tree, space, el, report);
        if container {
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
            "StringParameterType" => string(el, space, report),
            "BinaryParameterType" => binary(el, space, report),
            "AggregateParameterType" => return self.aggregate(tree, space, el, report),
            "ArrayParameterType" => return self.array(tree, space, el, report),
            "AbsoluteTimeParameterType" => {
                report.error(
                    Code::Unsupported,
                    el.span(),
                    "absolute time parameters aren't supported: an MDL field holds a number, \
                     not an instant, so the decoded value would change",
                );
                None
            }
            "RelativeTimeParameterType" => relative_time(el, report),
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

    fn array(
        &mut self,
        tree: &Tree<'d>,
        space: usize,
        el: El<'d>,
        report: &mut Report,
    ) -> Option<Shape<'d>> {
        el.check(
            &Expect {
                attrs: &["name", "arrayTypeRef", "numberOfDimensions"],
                children: &["DimensionList"],
                ignored: DESCRIBED,
                text: false,
            },
            report,
        );
        let type_ref = el.required("arrayTypeRef", report)?;
        let element = match tree.resolve(space, type_ref.value, Kind::ParameterType) {
            Ok(found) => self.shape(tree, found.space, found.el, report)?,
            Err(message) => {
                report.error(Code::Reference, type_ref.span, message);
                return None;
            }
        };
        if matches!(element, Shape::Array(_)) {
            report.error(
                Code::Unsupported,
                type_ref.span,
                "an array of arrays (a multidimensional array) isn't supported yet: MDL arrays \
                 have one dimension",
            );
            return None;
        }
        let count = match el.child("DimensionList") {
            Some(list) => Some(dimensions(list, space, report)?),
            // XTCE 1.1: the container entry gives the size.
            None => {
                if let Some(attr) = el.attr("numberOfDimensions")
                    && xml::long(attr, report)? != 1
                {
                    report.error(
                        Code::Unsupported,
                        attr.span,
                        "an array of more than one dimension isn't supported yet: MDL arrays \
                         have one",
                    );
                    return None;
                }
                None
            }
        };
        Some(Shape::Array(Box::new(ArrayShape { el, element, count })))
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
        limits: limits(el, report),
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
    scalar.limits = limits(el, report);
    Some(scalar)
}

/// A relative time: a duration, as a number of its unit. The encoding's
/// scale and offset are a linear conversion.
fn relative_time<'d>(el: El<'d>, report: &mut Report) -> Option<Scalar<'d>> {
    el.check(
        &Expect {
            attrs: &["name", "baseType"],
            children: &["Encoding", "ReferenceTime"],
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
    if let Some(reference) = el.child("ReferenceTime") {
        report.error(
            Code::Unsupported,
            reference.span(),
            "a relative time measured from an epoch or another parameter isn't supported",
        );
        return None;
    }
    let Some(encoding) = el.child("Encoding") else {
        report.error(
            Code::Invalid,
            el.span(),
            format!(
                "relative time type {} has no Encoding, so it can't be read from a packet",
                el.item_name().unwrap_or_default()
            ),
        );
        return None;
    };
    encoding.check(
        &Expect {
            attrs: &["units", "scale", "offset"],
            children: ENCODINGS,
            ignored: &[],
            text: false,
        },
        report,
    );
    let unit = match encoding.attr("units") {
        None => Some(("s".to_string(), encoding.span())),
        Some(attr) => match attr.value.trim() {
            "seconds" => Some(("s".to_string(), attr.span)),
            "picoSeconds" => Some(("ps".to_string(), attr.span)),
            "days" => Some(("d".to_string(), attr.span)),
            other => {
                report.warning(
                    Code::Ignored,
                    attr.span,
                    format!(
                        "a duration in {other} isn't imported with a unit, since their lengths \
                         vary; the value is unchanged"
                    ),
                );
                None
            }
        },
    };
    let scale = match encoding.attr("scale") {
        Some(attr) => (xml::double(attr, report)?, attr.span),
        None => (1.0, encoding.span()),
    };
    let offset = match encoding.attr("offset") {
        Some(attr) => (xml::double(attr, report)?, attr.span),
        None => (0.0, encoding.span()),
    };
    let data = only_encoding(
        encoding,
        &["IntegerDataEncoding", "FloatDataEncoding"],
        report,
    )?;
    let mut scalar = if data.name() == "FloatDataEncoding" {
        float_encoding(data, report)?.scalar(el)
    } else {
        integer_encoding(data, report)?.scalar(el)
    };
    if (scale.0, offset.0) != (1.0, 0.0) {
        if scalar.conversion.is_some() {
            report.error(
                Code::Unsupported,
                encoding.span(),
                "a relative time with both a calibrator and a scale or offset isn't supported",
            );
            return None;
        }
        let mut conversion = Object::at(encoding.span());
        conversion.put("kind", "linear", None);
        conversion.put("scale", scale.0, Some(scale.1));
        conversion.put("offset", offset.0, Some(offset.1));
        scalar.conversion = Some(conversion);
    }
    scalar.raw = if scalar.conversion.is_none() && scalar.ty.0 != "float" {
        Raw::Integer
    } else {
        Raw::Other
    };
    scalar.unit = unit;
    Some(scalar)
}

/// A number type's DefaultAlarm as MDL limits: the warning range's ends
/// are yellow and the critical range's red. What MDL's limits don't hold
/// is left out, each a warning, since alarms don't change values.
fn limits(el: El<'_>, report: &mut Report) -> Option<Limits> {
    let alarm = el.child("DefaultAlarm")?;
    alarm.check(
        &Expect {
            attrs: &["minViolations"],
            children: &["StaticAlarmRanges"],
            ignored: &[
                "name",
                "shortDescription",
                "minConformance",
                "AncillaryDataSet",
                "ChangeAlarmRanges",
                "AlarmMultiRanges",
                "AlarmConditions",
                "CustomAlarm",
            ],
            text: false,
        },
        report,
    );
    if let Some(attr) = alarm.attr("minViolations")
        && attr.value.trim() != "1"
    {
        report.warning(
            Code::Ignored,
            attr.span,
            "an alarm after several violations isn't imported as such: MDL limits apply to each \
             value",
        );
    }
    let ranges = alarm.child("StaticAlarmRanges")?;
    ranges.check(
        &Expect {
            attrs: &["rangeForm"],
            children: &["WarningRange", "CriticalRange"],
            ignored: &[
                "name",
                "shortDescription",
                "AncillaryDataSet",
                "WatchRange",
                "DistressRange",
                "SevereRange",
            ],
            text: false,
        },
        report,
    );
    if let Some(form) = ranges.attr("rangeForm")
        && form.value.trim() == "inside"
    {
        report.warning(
            Code::Ignored,
            form.span,
            "alarm ranges that are in alarm inside them aren't imported: MDL limits are \
             thresholds a value goes beyond",
        );
        return None;
    }
    let mut limits = Limits {
        span: alarm.span(),
        red_low: None,
        yellow_low: None,
        yellow_high: None,
        red_high: None,
    };
    if let Some(warning) = ranges.child("WarningRange") {
        (limits.yellow_low, limits.yellow_high) = alarm_range(warning, report)?;
    }
    if let Some(critical) = ranges.child("CriticalRange") {
        (limits.red_low, limits.red_high) = alarm_range(critical, report)?;
    }
    let any = [
        limits.red_low,
        limits.yellow_low,
        limits.yellow_high,
        limits.red_high,
    ]
    .iter()
    .any(Option::is_some);
    any.then_some(limits)
}

/// One end of an alarm range, and where it came from.
type End = Option<(f64, Span)>;

/// A range's low and high ends: the values beyond which it's in alarm.
fn alarm_range(el: El<'_>, report: &mut Report) -> Option<(End, End)> {
    el.check(
        &Expect {
            attrs: &[
                "minInclusive",
                "minExclusive",
                "maxInclusive",
                "maxExclusive",
            ],
            children: &[],
            ignored: &[],
            text: false,
        },
        report,
    );
    let end = |inclusive: &str, exclusive: &str, report: &mut Report| match (
        el.attr(inclusive),
        el.attr(exclusive),
    ) {
        (Some(attr), None) => Some(Some((xml::double(attr, report)?, attr.span))),
        (None, Some(attr)) => {
            report.warning(
                Code::Ignored,
                attr.span,
                format!(
                    "{exclusive} is imported as a limit, but not as exclusive: MDL limits \
                         don't say whether a value at the limit is in alarm"
                ),
            );
            Some(Some((xml::double(attr, report)?, attr.span)))
        }
        (None, None) => Some(None),
        (Some(_), Some(attr)) => {
            report.error(
                Code::Invalid,
                attr.span,
                format!("a range has {inclusive} or {exclusive}, not both"),
            );
            None
        }
    };
    let low = end("minInclusive", "minExclusive", report)?;
    let high = end("maxInclusive", "maxExclusive", report)?;
    Some((low, high))
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

fn string<'d>(el: El<'d>, space: usize, report: &mut Report) -> Option<Scalar<'d>> {
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
    let order = match encoding.attr("byteOrder") {
        Some(attr) => Some((attribute_order(attr, report)?, attr.span)),
        None => None,
    };
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
    let length = string_length(encoding, space, order, report)?;
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
        limits: None,
    })
}

/// A string's length: fixed, up to a terminator, both, a leading size,
/// or a size another parameter gives.
fn string_length<'d>(
    encoding: El<'d>,
    space: usize,
    order: Option<(Order, Span)>,
    report: &mut Report,
) -> Option<Length<'d>> {
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
        let fixed = size.child("Fixed");
        if let Some(leading) = size.child("LeadingSize") {
            if fixed.is_some() {
                report.error(
                    Code::Unsupported,
                    leading.span(),
                    "a leading size inside a fixed-size buffer isn't supported",
                );
                return None;
            }
            return leading_size(leading, order, report);
        }
        let terminator = size.child("TerminationChar");
        return match (fixed, terminator) {
            (Some(fixed), None) => {
                let (bytes, span) = fixed_bytes(fixed, report)?;
                Some(Length::Written(
                    json!({ "kind": "fixed", "bytes": bytes }),
                    span,
                ))
            }
            (None, Some(terminator)) => terminated(terminator, report),
            // A fixed-size buffer whose value ends at the terminator.
            (Some(fixed), Some(terminator)) => {
                let (bytes, span) = fixed_bytes(fixed, report)?;
                let (byte, _) = terminator_byte(terminator, report)?;
                Some(Length::Written(
                    json!({ "kind": "fixed", "bytes": bytes, "terminator": byte }),
                    span,
                ))
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
        if let Some(lookup) = variable.child("DiscreteLookupList") {
            report.error(
                Code::Unsupported,
                lookup.span(),
                "a size chosen from a lookup list isn't supported",
            );
            return None;
        }
        let buffer = match variable.child("DynamicValue") {
            Some(value) => dynamic(value, space, report)?,
            None => None,
        };
        // XTCE: with a LeadingSize, a TerminationChar has no meaning.
        let leading = variable.child("LeadingSize");
        let terminator = variable
            .child("TerminationChar")
            .filter(|_| leading.is_none());
        return match (buffer, leading, terminator) {
            (None, Some(leading), _) => leading_size(leading, order, report),
            (None, None, Some(terminator)) => terminated(terminator, report),
            (Some(buffer), None, None) => Some(Length::Counted(buffer)),
            (Some(buffer), ..) => {
                report.error(
                    Code::Unsupported,
                    buffer.span,
                    "a string ended by a terminator or a leading size inside a buffer whose size \
                     another parameter gives isn't supported",
                );
                None
            }
            (None, None, None) => {
                report.error(
                    Code::Invalid,
                    variable.span(),
                    "a variable string has a DynamicValue, a LeadingSize or a TerminationChar",
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

/// A `Fixed` size in whole bytes, and where its value is.
fn fixed_bytes(fixed: El<'_>, report: &mut Report) -> Option<(u32, Span)> {
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
    Some((whole_bytes(value, report)?, value.span()))
}

fn terminated<'d>(terminator: El<'_>, report: &mut Report) -> Option<Length<'d>> {
    let (byte, span) = terminator_byte(terminator, report)?;
    Some(Length::Written(
        json!({ "kind": "terminator", "byte": byte }),
        span,
    ))
}

/// A LeadingSize: a size tag of this many bits before the string.
fn leading_size<'d>(
    leading: El<'_>,
    order: Option<(Order, Span)>,
    report: &mut Report,
) -> Option<Length<'d>> {
    leading.check(
        &Expect {
            attrs: &["sizeInBitsOfSizeTag"],
            children: &[],
            ignored: &[],
            text: false,
        },
        report,
    );
    let (bits, span) = match leading.attr("sizeInBitsOfSizeTag") {
        Some(attr) => (xml::long(attr, report)?, attr.span),
        None => (16, leading.span()),
    };
    match u32::try_from(bits) {
        Ok(bits) if (1..=64).contains(&bits) => Some(Length::Leading { bits, order, span }),
        _ => {
            report.error(
                Code::Unsupported,
                span,
                format!("a {bits}-bit size tag isn't supported: MDL reads 1 to 64 bits"),
            );
            None
        }
    }
}

/// A DynamicValue, or `None` for Yamcs's `_yamcs_ignore`, which says the
/// size comes from elsewhere (a terminator, a leading size).
pub(crate) fn dynamic<'d>(
    el: El<'d>,
    space: usize,
    report: &mut Report,
) -> Option<Option<Dynamic<'d>>> {
    el.check(
        &Expect {
            attrs: &[],
            children: &["ParameterInstanceRef", "LinearAdjustment"],
            ignored: &[],
            text: false,
        },
        report,
    );
    let Some(instance) = el.child("ParameterInstanceRef") else {
        report.error(
            Code::Unsupported,
            el.span(),
            "a DynamicValue reads a parameter (an argument is for commands)",
        );
        return None;
    };
    instance.check(
        &Expect {
            attrs: &["parameterRef", "instance", "useCalibratedValue"],
            children: &[],
            ignored: &[],
            text: false,
        },
        report,
    );
    let reference = instance.required("parameterRef", report)?;
    if reference.value == "_yamcs_ignore" {
        return Some(None);
    }
    if let Some(attr) = instance.attr("instance")
        && xml::long(attr, report)? != 0
    {
        report.error(
            Code::Unsupported,
            attr.span,
            "a size from an earlier instance of a parameter isn't supported",
        );
        return None;
    }
    let calibrated = match instance.attr("useCalibratedValue") {
        Some(attr) => xml::boolean(attr, report)?,
        None => true,
    };
    let (slope, intercept) = match el.child("LinearAdjustment") {
        Some(adjustment) => {
            adjustment.check(
                &Expect {
                    attrs: &["slope", "intercept"],
                    children: &[],
                    ignored: &[],
                    text: false,
                },
                report,
            );
            let slope = match adjustment.attr("slope") {
                Some(attr) => whole(attr, report)?,
                None => 1,
            };
            let intercept = match adjustment.attr("intercept") {
                Some(attr) => whole(attr, report)?,
                None => 0,
            };
            (slope, intercept)
        }
        None => (1, 0),
    };
    Some(Some(Dynamic {
        space,
        reference: reference.value,
        span: reference.span,
        calibrated,
        slope,
        intercept,
    }))
}

/// A double that is a whole number, as MDL's counts and lengths are.
fn whole(attr: Attr<'_>, report: &mut Report) -> Option<i64> {
    let value = xml::double(attr, report)?;
    // Within ±2^53, where every whole double is exact.
    if value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_991.0 {
        return Some(value as i64);
    }
    report.error(
        Code::Unsupported,
        attr.span,
        format!(
            "{} {value} isn't a whole number, and MDL counts in whole numbers",
            attr.name
        ),
    );
    None
}

/// An array's one dimension, as a count of elements.
pub(crate) fn dimensions<'d>(list: El<'d>, space: usize, report: &mut Report) -> Option<Count<'d>> {
    list.check(
        &Expect {
            attrs: &[],
            children: &["Dimension"],
            ignored: &[],
            text: false,
        },
        report,
    );
    let all: Vec<El<'d>> = list.all("Dimension").collect();
    let [dimension] = all.as_slice() else {
        report.error(
            Code::Unsupported,
            list.span(),
            format!(
                "an array of {} dimensions isn't supported yet: MDL arrays have one",
                all.len()
            ),
        );
        return None;
    };
    dimension.check(
        &Expect {
            attrs: &[],
            children: &["StartingIndex", "EndingIndex"],
            ignored: &[],
            text: false,
        },
        report,
    );
    let (Some(starting), Some(ending)) = (
        dimension.child("StartingIndex"),
        dimension.child("EndingIndex"),
    ) else {
        report.error(
            Code::Invalid,
            dimension.span(),
            "a Dimension has a StartingIndex and an EndingIndex",
        );
        return None;
    };
    let start = match integer_value(starting, space, report)? {
        IntegerValue::Fixed(start, _) => start,
        IntegerValue::Dynamic(dynamic) => {
            report.error(
                Code::Unsupported,
                dynamic.span,
                "a starting index another parameter gives isn't supported",
            );
            return None;
        }
    };
    match integer_value(ending, space, report)? {
        IntegerValue::Fixed(end, span) => {
            match end
                .checked_sub(start)
                .and_then(|n| n.checked_add(1))
                .map(u32::try_from)
            {
                Some(Ok(count)) => Some(Count::Fixed(count, span)),
                _ => {
                    report.error(
                        Code::Invalid,
                        span,
                        format!("indexes {start} to {end} aren't a count of elements"),
                    );
                    None
                }
            }
        }
        IntegerValue::Dynamic(dynamic) => Some(Count::Counted { dynamic, start }),
    }
}

/// An IntegerValueType: a fixed value, or a parameter's.
pub(crate) enum IntegerValue<'d> {
    Fixed(i64, Span),
    Dynamic(Dynamic<'d>),
}

/// An IntegerValueType whose dynamic references are made from `space`.
pub(crate) fn integer_value<'d>(
    el: El<'d>,
    space: usize,
    report: &mut Report,
) -> Option<IntegerValue<'d>> {
    el.check(
        &Expect {
            attrs: &[],
            children: &["FixedValue", "DynamicValue", "DiscreteLookupList"],
            ignored: &[],
            text: false,
        },
        report,
    );
    if let Some(value) = el.child("FixedValue") {
        let (text, span) = value.text();
        return Some(IntegerValue::Fixed(
            xml::long_text(&text, "FixedValue", span, report)?,
            span,
        ));
    }
    if let Some(value) = el.child("DynamicValue") {
        return match dynamic(value, space, report)? {
            Some(dynamic) => Some(IntegerValue::Dynamic(dynamic)),
            None => {
                report.error(
                    Code::Unsupported,
                    value.span(),
                    "_yamcs_ignore gives no value here",
                );
                None
            }
        };
    }
    report.error(
        Code::Unsupported,
        el.span(),
        format!("{} chosen from a lookup list isn't supported", el.name()),
    );
    None
}

/// A TerminationChar: one byte, in hexadecimal.
fn terminator_byte(terminator: El<'_>, report: &mut Report) -> Option<(u8, Span)> {
    let (text, span) = terminator.text();
    match (text.len(), u8::from_str_radix(&text, 16)) {
        (2, Ok(byte)) => Some((byte, span)),
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

fn binary<'d>(el: El<'d>, space: usize, report: &mut Report) -> Option<Scalar<'d>> {
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
    let length = match size.child("FixedValue") {
        Some(value) => {
            size.check(
                &Expect {
                    attrs: &[],
                    children: &["FixedValue"],
                    ignored: &[],
                    text: false,
                },
                report,
            );
            let bytes = whole_bytes(value, report)?;
            Length::Written(json!({ "kind": "fixed", "bytes": bytes }), value.span())
        }
        None => match integer_value(size, space, report)? {
            IntegerValue::Dynamic(dynamic) => Length::Counted(dynamic),
            IntegerValue::Fixed(..) => return None,
        },
    };
    Some(Scalar {
        el,
        ty: ("bytes", encoding.span()),
        size_bits: None,
        order: None,
        length: Some(length),
        text: None,
        conversion: None,
        unit: None,
        param: "bytes",
        states: Vec::new(),
        raw: Raw::Other,
        limits: None,
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
            "DefaultAlarm",
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
            limits: None,
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
