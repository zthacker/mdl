//! Parameter and argument types: what a field reads or writes, and what
//! its catalog param or argument holds.

use std::collections::{BTreeMap, BTreeSet};

use mdl::v2::source::Span;
use mdl::v2::validate::MAX_NESTING;
use serde_json::{Value, json};

use crate::Code;
use crate::calibration::{
    Empty, Holds, SAFE, ToRaw, argument_calibration, engineering_range, worked_out,
};
use crate::out::{List, Object};
use crate::report::Report;
use crate::space::{ARGUMENT_TYPES, Kind, Tree};
use crate::units::{Ucum, ucum};
use crate::xml::{self, Attr, El, Expect};

/// Whether a type is a parameter's or an argument's. Both read the same
/// encodings; a parameter type's alarms are its params' limits, and an
/// argument type's valid range and initial value are its argument's range
/// and default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Role {
    Parameter,
    Argument,
}

impl Role {
    fn of(el: &El<'_>) -> Role {
        if ARGUMENT_TYPES.contains(&el.name()) {
            Role::Argument
        } else {
            Role::Parameter
        }
    }
}

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
    /// An argument's conversion from engineering to raw, where its
    /// conversion doesn't invert exactly.
    pub(crate) inverse: Option<Object>,
    pub(crate) unit: Option<(String, Span)>,
    /// The param's type: `u64`, `i64`, `f64`, `bool`, `str`, `bytes` or
    /// `enum`.
    pub(crate) param: &'static str,
    pub(crate) states: Vec<(String, Span)>,
    /// How a comparison's engineering value becomes a raw value.
    pub(crate) raw: Raw,
    /// A number type's alarm ranges, as the param's limits.
    pub(crate) limits: Option<Limits>,
    /// A number argument type's valid range, as the argument's range.
    pub(crate) range: Option<ValidRange>,
    /// An argument type's initial value, as XTCE writes it.
    pub(crate) initial: Option<(&'d str, Span)>,
}

/// A valid range as MDL's: inclusive ends, each with where it came from.
#[derive(Debug, Clone)]
pub(crate) struct ValidRange {
    pub(crate) min: Option<(f64, Span)>,
    pub(crate) max: Option<(f64, Span)>,
    /// Where `validRangeAppliesToCalibrated="false"` says the range is on
    /// raw values.
    pub(crate) raw: Option<Span>,
}

/// A DefaultCalibrator, as XTCE writes it.
#[derive(Clone)]
pub(crate) struct Calibrator {
    /// The DefaultCalibrator element.
    pub(crate) span: Span,
    pub(crate) calibration: Calibration,
}

#[derive(Clone)]
pub(crate) enum Calibration {
    /// Coefficients by exponent, with no zero highest term past the first.
    Polynomial { coefficients: Vec<f64>, span: Span },
    Spline {
        points: Vec<SplinePoint>,
        order: (i64, Span),
        /// Where `extrapolate="true"` is.
        extrapolate: Option<Span>,
        span: Span,
    },
}

#[derive(Clone)]
pub(crate) struct SplinePoint {
    pub(crate) raw: (f64, Span),
    pub(crate) calibrated: (f64, Span),
    pub(crate) span: Span,
}

impl Calibrator {
    /// As a parameter's conversion, raw to engineering. A spline of order 0
    /// is a step table, of order 1 a linear one.
    fn conversion(&self) -> Object {
        match &self.calibration {
            Calibration::Polynomial { coefficients, span } => {
                let mut conversion = Object::at(*span);
                match coefficients.as_slice() {
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
                conversion
            }
            Calibration::Spline {
                points,
                order,
                extrapolate,
                span,
            } => {
                let mut list = List::default();
                for point in points {
                    let mut object = Object::at(point.span);
                    object.put("raw", point.raw.0, Some(point.raw.1));
                    object.put("value", point.calibrated.0, Some(point.calibrated.1));
                    list.push_object(object);
                }
                let mut conversion = Object::at(*span);
                conversion.put("kind", "table", None);
                conversion.put_list("points", list);
                if order.0 == 0 {
                    conversion.put("interpolation", "step", Some(order.1));
                }
                if let Some(at) = extrapolate {
                    conversion.put("extrapolate", true, Some(*at));
                }
                conversion
            }
        }
    }
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

/// A type: whose it is, its SpaceSystem and its name.
type TypeKey<'d> = (Role, usize, &'d str);

/// Shapes of the types used so far, each worked out once.
#[derive(Default)]
pub(crate) struct Types<'d> {
    shapes: BTreeMap<TypeKey<'d>, Option<Shape<'d>>>,
    /// Aggregates being worked out, to catch one that contains itself.
    open: BTreeSet<TypeKey<'d>>,
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
        let key = (Role::of(&el), space, el.item_name().unwrap_or_default());
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
                    format!("type {} contains itself", key.2),
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
        let role = Role::of(&el);
        let scalar = match el.name() {
            "IntegerParameterType" | "IntegerArgumentType" => integer(el, role, report),
            "FloatParameterType" | "FloatArgumentType" => float(el, role, report),
            "EnumeratedParameterType" | "EnumeratedArgumentType" => enumerated(el, role, report),
            "BooleanParameterType" | "BooleanArgumentType" => boolean(el, role, report),
            "StringParameterType" | "StringArgumentType" => string(el, space, role, report),
            "BinaryParameterType" | "BinaryArgumentType" => binary(el, space, role, report),
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
            "AbsoluteTimeArgumentType" => {
                report.error(
                    Code::Unsupported,
                    el.span(),
                    "absolute time arguments aren't supported: an MDL field holds a number, not \
                     an instant, so the encoded value would change",
                );
                None
            }
            "AggregateArgumentType" | "ArrayArgumentType" => {
                report.error(
                    Code::Unsupported,
                    el.span(),
                    "aggregate and array arguments aren't supported yet",
                );
                None
            }
            "RelativeTimeParameterType"
            | "RelativeTimeArgumentType"
            | "RelativeTimeAgumentType" => relative_time(el, role, report),
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

fn integer<'d>(el: El<'d>, role: Role, report: &mut Report) -> Option<Scalar<'d>> {
    let lists = Lists::numeric(role, &["name", "signed", "sizeInBits", "baseType"]);
    el.check(&lists.expect(), report);
    let signed = el.attr("signed").and_then(|a| xml::boolean(a, report));
    // The engineering value's size, which Yamcs checks an argument against.
    let mut type_bits = 32;
    if let Some(size) = el.attr("sizeInBits")
        && let Some(bits) = xml::long(size, report)
    {
        if !(1..=64).contains(&bits) {
            report.error(
                Code::Unsupported,
                size.span,
                "MDL's integers are at most 64 bits",
            );
        }
        type_bits = bits.clamp(1, 64);
    }
    let unit = unit(el, report);
    let encoding = only_encoding(el, &["IntegerDataEncoding"], report)?;
    let encoding = integer_encoding(encoding, report)?;
    if let Some(calibrator) = &encoding.calibrator {
        let instead = match role {
            Role::Parameter => "FloatParameterType",
            Role::Argument => "FloatArgumentType",
        };
        report.error(
            Code::Unsupported,
            calibrator.span,
            format!(
                "an integer type with a calibrator: MDL's calibrations give floats, so the value \
                 would change; make it a {instead}"
            ),
        );
        return None;
    }
    let signed = signed.unwrap_or(true);
    // An MDL command writes a u64 argument to a uint field only, so an
    // unsigned argument over a two's complement encoding is an i64, which
    // its range keeps at 0 and above.
    let param = if signed || (role == Role::Argument && encoding.ty == "int") {
        "i64"
    } else {
        "u64"
    };
    let (limits, range) = match role {
        Role::Parameter => (limits(el, report), None),
        Role::Argument => match valid_range(el, true, report)? {
            Some(range) => (None, Some(range)),
            None => {
                let held = Held {
                    signed,
                    bits: type_bits,
                    param,
                };
                (None, integer_holds(&encoding, held, report))
            }
        },
    };
    Some(Scalar {
        param,
        raw: Raw::Integer,
        unit,
        limits,
        range,
        initial: initial(el, role),
        ..encoding.scalar(el)
    })
}

fn float<'d>(el: El<'d>, role: Role, report: &mut Report) -> Option<Scalar<'d>> {
    let lists = Lists::numeric(role, &["name", "sizeInBits", "baseType"]);
    el.check(&lists.expect(), report);
    let unit = unit(el, report);
    let encoding_el = only_encoding(el, &["FloatDataEncoding", "IntegerDataEncoding"], report)?;
    let encoding = if encoding_el.name() == "FloatDataEncoding" {
        float_encoding(encoding_el, report)?
    } else {
        integer_encoding(encoding_el, report)?
    };
    let calibrated = encoding.calibrator.is_some();
    let mut scalar = encoding.scalar(el);
    scalar.param = "f64";
    scalar.unit = unit;
    scalar.raw = if calibrated || scalar.ty.0 == "float" {
        Raw::Other
    } else {
        Raw::Integer
    };
    match role {
        Role::Parameter => scalar.limits = limits(el, report),
        Role::Argument => {
            float_argument_differences(el, &encoding, report)?;
            let given = valid_range(el, false, report)?;
            scalar.range = match &encoding.calibrator {
                None if encoding.ty != "float" => Some(rounded_range(given, &encoding, report)),
                None => given,
                Some(calibrator) => {
                    let calibration = argument_calibration(calibrator, report)?;
                    let range = calibrated_range(given, &calibration.to_raw, &encoding, report)?;
                    scalar.conversion = Some(calibration.conversion);
                    scalar.inverse = calibration.inverse;
                    Some(range)
                }
            };
            scalar.initial = initial(el, role);
        }
    }
    Some(scalar)
}

/// What a number encoding holds, as raw values.
fn holds(encoding: &Encoding<'_>) -> Holds {
    match (encoding.ty, encoding.size) {
        ("float", 32) => Holds::Float32,
        ("float", _) => Holds::Float64,
        _ => {
            let (low, high) = encoding_integers(encoding);
            Holds::Integers(low, high)
        }
    }
}

/// A calibrated argument's range, which MDL needs whole. A valid range on
/// engineering values is taken as it is, within a spline's points if it
/// doesn't extrapolate; one on raw values (`validRangeAppliesToCalibrated`
/// false) is the engineering values whose raw values lie in it, as XTCE
/// says, though Yamcs checks it against the engineering value. Without
/// one, it's the engineering values whose raw values the encoding holds.
fn calibrated_range(
    given: Option<ValidRange>,
    to_raw: &ToRaw,
    encoding: &Encoding<'_>,
    report: &mut Report,
) -> Option<ValidRange> {
    let span = encoding.size_span;
    let worked = |raw: Option<(f64, f64)>, report: &mut Report| {
        derived_range(to_raw, holds(encoding), raw, span, report)
    };
    match given {
        Some(ValidRange {
            min,
            max,
            raw: Some(raw_span),
        }) => {
            let raw = (
                min.map_or(f64::NEG_INFINITY, |end| end.0),
                max.map_or(f64::INFINITY, |end| end.0),
            );
            let (low, high) = worked(Some(raw), report)?;
            report.warning(
                Code::Differs,
                raw_span,
                format!(
                    "the valid range is on raw values, as validRangeAppliesToCalibrated says, \
                     so the argument's range is {low} to {high}; Yamcs checks the range against \
                     the engineering value"
                ),
            );
            Some(ValidRange {
                min: Some((low, min.map_or(raw_span, |end| end.1))),
                max: Some((high, max.map_or(raw_span, |end| end.1))),
                raw: None,
            })
        }
        Some(mut range) => {
            // MDL keeps a spline that doesn't extrapolate to its points
            // (reported with the calibrator).
            if let Some((first, last)) = to_raw.domain() {
                range.min = range.min.map(|(value, at)| (value.max(first), at));
                range.max = range.max.map(|(value, at)| (value.min(last), at));
            }
            let (low, high) = worked(None, report)?;
            // A given end past everything the encoding holds leaves no
            // range once the other end is worked out.
            let past = range
                .min
                .filter(|(min, _)| *min > high)
                .or(range.max.filter(|(max, _)| *max < low));
            if let Some((end, at)) = past {
                report.error(
                    Code::Invalid,
                    at,
                    format!(
                        "the valid range's end, {end}, lies past every value whose raw value the \
                         encoding holds ({low} to {high})"
                    ),
                );
                return None;
            }
            range.min = range.min.or(Some((low, span)));
            range.max = range.max.or(Some((high, span)));
            Some(range)
        }
        None => worked(None, report).map(|range| worked_out(span, range)),
    }
}

/// The engineering values whose raw values the field holds (within `raw`,
/// when given), reported when there are none MDL's range can say.
fn derived_range(
    to_raw: &ToRaw,
    holds: Holds,
    raw: Option<(f64, f64)>,
    span: Span,
    report: &mut Report,
) -> Option<(f64, f64)> {
    match engineering_range(to_raw, holds, raw) {
        Ok((low, high, cut)) => {
            if cut {
                past_safe(span, report);
            }
            Some((low, high))
        }
        Err(Empty::NoValue) => {
            report.error(
                Code::Invalid,
                span,
                "no value of this argument gives a raw value its encoding holds in its range",
            );
            None
        }
        Err(Empty::PastSafe) => {
            report.error(
                Code::Unsupported,
                span,
                format!(
                    "every value of this argument whose raw value the encoding holds lies past \
                     ±{SAFE}, where an MDL range's ends can't reach"
                ),
            );
            None
        }
    }
}

/// The integers an integer encoding holds: unsigned, or two's complement.
fn encoding_integers(encoding: &Encoding<'_>) -> (i128, i128) {
    signed_or_not(encoding.ty == "int", encoding.size)
}

/// The integers of `bits` bits, signed (two's complement) or not.
fn signed_or_not(signed: bool, bits: u32) -> (i128, i128) {
    let bits = bits.clamp(1, 64);
    if signed {
        (-(1 << (bits - 1)), (1 << (bits - 1)) - 1)
    } else {
        (0, (1 << bits) - 1)
    }
}

/// An integer argument type: whether it's signed and how many bits it
/// has, as XTCE says, and its catalog type.
struct Held {
    signed: bool,
    bits: i64,
    param: &'static str,
}

/// What an integer argument with no valid range may be: what its encoding
/// holds, and its type too (`signed`, and `sizeInBits`, 32 by default),
/// which Yamcs checks a value against. An end the catalog type has anyway
/// is left out; one past ±(2^53 - 1) is cut there.
fn integer_holds(encoding: &Encoding<'_>, held: Held, report: &mut Report) -> Option<ValidRange> {
    let (field_low, field_high) = encoding_integers(encoding);
    let (type_low, type_high) = signed_or_not(held.signed, u32::try_from(held.bits).unwrap_or(64));
    let (own_low, own_high) = if held.param == "i64" {
        (i128::from(i64::MIN), i128::from(i64::MAX))
    } else {
        (0, i128::from(u64::MAX))
    };
    let (low, high) = (field_low.max(type_low), field_high.min(type_high));
    let span = encoding.size_span;
    if (low > own_low && low < -SAFE) || (high < own_high && high > SAFE) {
        past_safe(span, report);
    }
    let min = (low > own_low).then(|| (low.max(-SAFE) as f64, span));
    let max = (high < own_high).then(|| (high.min(SAFE) as f64, span));
    (min.is_some() || max.is_some()).then_some(ValidRange {
        min,
        max,
        raw: None,
    })
}

/// Values a range would hold but can't say, since its ends lie within
/// ±(2^53 - 1).
fn past_safe(span: Span, report: &mut Report) {
    report.warning(
        Code::Differs,
        span,
        format!(
            "MDL refuses this argument's values past ±{SAFE}, which the encoding holds and \
             Yamcs sends: an MDL range's ends lie within ±(2^53 - 1), where every JSON reader \
             agrees"
        ),
    );
}

/// The range of an uncalibrated float written as an integer, as MDL
/// rounds it: where the type gives no end, from the least value that
/// rounds into the encoding to the greatest (just above `low - 0.5` and
/// just below `high + 0.5`). A given end that Yamcs keeps inside the
/// encoding, since it drops a value's fraction, but that MDL's rounding
/// takes outside it is narrowed to the last value MDL keeps, with a
/// warning; an end outside the encoding either way is left for validation
/// to report.
fn rounded_range(
    given: Option<ValidRange>,
    encoding: &Encoding<'_>,
    report: &mut Report,
) -> ValidRange {
    let (low, high) = encoding_integers(encoding);
    let span = encoding.size_span;
    let as_written = ToRaw::Linear {
        scale: 1.0,
        offset: 0.0,
    };
    // The least and greatest values that round into the encoding, cut at
    // ±(2^53 - 1); an encoding always holds some.
    let (lowest, highest, cut) = engineering_range(&as_written, Holds::Integers(low, high), None)
        .unwrap_or((low as f64, high as f64, false));
    let given = given.unwrap_or(ValidRange {
        min: None,
        max: None,
        raw: None,
    });
    if cut && (given.min.is_none() || given.max.is_none()) {
        past_safe(span, report);
    }
    let mut narrowed = |end: &str, (value, at): (f64, Span), last: f64, beyond: i128| {
        report.warning(
            Code::Differs,
            at,
            format!(
                "the valid range's {end}, {value}, is narrowed to {last}: MDL rounds the values \
                 past it to {beyond}, which the encoding can't hold, where Yamcs drops their \
                 fraction"
            ),
        );
        (last, at)
    };
    // An end MDL rounds out of the encoding that Yamcs, dropping the
    // fraction, keeps in it.
    let min = match given.min {
        None => (lowest, span),
        Some(end) if (end.0.round() as i128) < low && end.0 > (low - 1) as f64 => {
            narrowed("minimum", end, lowest, low - 1)
        }
        Some(end) => end,
    };
    let max = match given.max {
        None => (highest, span),
        Some(end) if (end.0.round() as i128) > high && end.0 < (high + 1) as f64 => {
            narrowed("maximum", end, highest, high + 1)
        }
        Some(end) => end,
    };
    ValidRange {
        min: Some(min),
        max: Some(max),
        raw: None,
    }
}

/// Where Yamcs writes an uncalibrated float argument other than as MDL
/// does: it narrows the value to the type's `sizeInBits` (32 unless the
/// type says 64) before writing it, and drops the fraction of a value
/// written as an integer, where MDL writes the value whole and rounds it
/// to the nearest integer.
fn float_argument_differences(
    el: El<'_>,
    encoding: &Encoding<'_>,
    report: &mut Report,
) -> Option<()> {
    let narrow = match el.attr("sizeInBits") {
        Some(attr) => xml::long(attr, report)? == 32,
        None => true,
    };
    if encoding.ty != "float" && encoding.calibrator.is_none() {
        report.warning(
            Code::Differs,
            encoding.el.span(),
            "Yamcs drops the fraction of a float argument written as an integer (2.7 is 2, and \
             -2.7 is -2); MDL rounds it to the nearest integer, halves away from zero (3 and \
             -3)",
        );
    } else if encoding.ty == "float" && narrow && encoding.size == 64 {
        let span = el.attr("sizeInBits").map_or(el.span(), |a| a.span);
        report.warning(
            Code::Differs,
            span,
            "Yamcs narrows a value of this 32-bit float type to 32 bits before it writes it in \
             64 (0.1 becomes 0.10000000149011612); MDL keeps the whole value; give the type \
             sizeInBits=\"64\" to say so",
        );
    }
    Some(())
}

/// A relative time: a duration, as a number of its unit. The encoding's
/// scale and offset are a linear conversion. Yamcs doesn't encode a
/// relative time argument, so one is read as XTCE says, as a parameter is.
fn relative_time<'d>(el: El<'d>, role: Role, report: &mut Report) -> Option<Scalar<'d>> {
    let mut ignored = vec![
        "shortDescription",
        "LongDescription",
        "AliasSet",
        "AncillaryDataSet",
        "DefaultAlarm",
        "ContextAlarmList",
    ];
    let mut attrs = vec!["name", "baseType"];
    match role {
        Role::Parameter => ignored.push("initialValue"),
        Role::Argument => attrs.push("initialValue"),
    }
    el.check(
        &Expect {
            attrs: &attrs,
            children: &["Encoding", "ReferenceTime"],
            ignored: &ignored,
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
    let data = if data.name() == "FloatDataEncoding" {
        float_encoding(data, report)?
    } else {
        integer_encoding(data, report)?
    };
    let mut scalar = data.scalar(el);
    if let (Role::Argument, Some(calibrator)) = (role, &data.calibrator) {
        report.error(
            Code::Unsupported,
            calibrator.span,
            "a calibrated relative time argument isn't supported: XTCE and Yamcs disagree on \
             which way an argument's calibrator runs, and Yamcs doesn't encode relative times",
        );
        return None;
    }
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
    if role == Role::Argument {
        if scale.0 == 0.0 {
            report.error(
                Code::Unsupported,
                scale.1,
                "a relative time argument with a scale of 0 isn't supported: every value would \
                 be written as one raw value",
            );
            return None;
        }
        // MDL checks that the values an argument may take fit an integer
        // field, so the range is what the field holds.
        if data.ty != "float" {
            let to_raw = ToRaw::Linear {
                scale: scale.0,
                offset: offset.0,
            };
            let span = data.size_span;
            let range = derived_range(&to_raw, holds(&data), None, span, report)?;
            scalar.range = Some(worked_out(span, range));
        }
        scalar.initial = initial(el, role);
    }
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

fn enumerated<'d>(el: El<'d>, role: Role, report: &mut Report) -> Option<Scalar<'d>> {
    let lists = Lists::new(role, &["name", "baseType"], &["EnumerationList"], &[]);
    el.check(&lists.expect(), report);
    no_unit(el, report);
    let encoding = only_encoding(el, &["IntegerDataEncoding"], report)?;
    let encoding = integer_encoding(encoding, report)?;
    if let Some(calibrator) = &encoding.calibrator {
        report.error(
            Code::Unsupported,
            calibrator.span,
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
        let listed = states.iter().any(|(name, _)| name == label.value);
        // Yamcs encodes a label as its first value, and MDL decodes a
        // command through a states conversion that lists each once.
        if listed && role == Role::Argument {
            report.warning(
                Code::Ignored,
                value.span,
                format!(
                    "label {} stands for {raw} too, which isn't imported: an argument is \
                     encoded as its label's first value, as Yamcs encodes it",
                    label.value
                ),
            );
            continue;
        }
        let mut state = Object::at(entry.span());
        state.put("raw", raw, Some(value.span));
        state.put("name", label.value, Some(label.span));
        conversion_states.push_object(state);
        labels.push((label.value.to_string(), raw));
        if !listed {
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
        initial: initial(el, role),
        ..encoding.scalar(el)
    })
}

fn boolean<'d>(el: El<'d>, role: Role, report: &mut Report) -> Option<Scalar<'d>> {
    let lists = Lists::new(
        role,
        &["name", "baseType", "oneStringValue", "zeroStringValue"],
        &[],
        &[],
    );
    el.check(&lists.expect(), report);
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
            calibrator.span,
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
    scalar.initial = initial(el, role);
    Some(scalar)
}

fn string<'d>(el: El<'d>, space: usize, role: Role, report: &mut Report) -> Option<Scalar<'d>> {
    let lists = Lists::new(
        role,
        &["name", "baseType", "characterWidth"],
        &[],
        &["restrictionPattern", "SizeRangeInCharacters"],
    );
    el.check(&lists.expect(), report);
    if let Some(width) = el.attr("characterWidth") {
        if width.value.trim() == "8" {
            report.warning(
                Code::Ignored,
                width.span,
                format!(
                    "{}'s characterWidth isn't imported: MDL reads text by its encoding",
                    el.describe()
                ),
            );
        } else {
            report.error(
                Code::Unsupported,
                width.span,
                format!(
                    "characters of {} bits aren't supported: MDL reads text as UTF-8, US-ASCII \
                     or ISO-8859-1 bytes",
                    width.value.trim()
                ),
            );
            return None;
        }
    }
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
    let length = match role {
        Role::Parameter => string_length(encoding, space, order, report)?,
        Role::Argument => argument_string_length(encoding, report)?,
    };
    Some(Scalar {
        el,
        ty: ("string", encoding.span()),
        size_bits: None,
        order: None,
        length: Some(length),
        text,
        conversion: None,
        inverse: None,
        unit: None,
        param: "str",
        states: Vec::new(),
        raw: Raw::Other,
        limits: None,
        range: None,
        initial: initial(el, role),
    })
}

/// A string argument's length, written as Yamcs writes it: a fixed-size
/// buffer is padded with NULs (after one terminator, if the type has one),
/// which MDL's fixed buffer with a NUL terminator does too; a terminated
/// string is followed by its terminator. Sizes other arguments give, and
/// leading sizes, aren't supported yet.
fn argument_string_length<'d>(encoding: El<'d>, report: &mut Report) -> Option<Length<'d>> {
    let not_yet = |report: &mut Report, span: Span| {
        report.error(
            Code::Unsupported,
            span,
            "a string argument whose size another argument or a leading size gives isn't \
             supported yet",
        );
    };
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
            not_yet(report, leading.span());
            return None;
        }
        return match (size.child("Fixed"), size.child("TerminationChar")) {
            (Some(fixed), terminator) => {
                let (bytes, span) = fixed_bytes(fixed, report)?;
                if let Some(terminator) = terminator {
                    let (byte, at) = terminator_byte(terminator, report)?;
                    if byte != 0 {
                        report.error(
                            Code::Unsupported,
                            at,
                            "a fixed-size string argument ended by a terminator other than 00 \
                             isn't supported: Yamcs writes one terminator and then NULs to the \
                             end of the buffer, and MDL fills the buffer with the terminator",
                        );
                        return None;
                    }
                }
                Some(Length::Written(
                    json!({ "kind": "fixed", "bytes": bytes, "terminator": 0 }),
                    span,
                ))
            }
            (None, Some(terminator)) => terminated(terminator, report),
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
        for name in ["DynamicValue", "LeadingSize", "DiscreteLookupList"] {
            if let Some(size) = variable.child(name) {
                not_yet(report, size.span());
                return None;
            }
        }
        return match variable.child("TerminationChar") {
            Some(terminator) => terminated(terminator, report),
            None => {
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

fn binary<'d>(el: El<'d>, space: usize, role: Role, report: &mut Report) -> Option<Scalar<'d>> {
    let lists = Lists::new(role, &["name", "baseType"], &[], &[]);
    el.check(&lists.expect(), report);
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
        None if role == Role::Argument => {
            report.error(
                Code::Unsupported,
                size.span(),
                "a binary argument whose size another argument gives isn't supported yet",
            );
            return None;
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
        inverse: None,
        unit: None,
        param: "bytes",
        states: Vec::new(),
        raw: Raw::Other,
        limits: None,
        range: None,
        initial: initial(el, role),
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

/// Children every type may have that the reader reads: a unit and its data
/// encoding.
const ENCODED: &[&str] = &[
    "UnitSet",
    "IntegerDataEncoding",
    "FloatDataEncoding",
    "StringDataEncoding",
    "BinaryDataEncoding",
];

/// What a type element may hold: what every type of its kind may, and what
/// its role adds. A parameter type's alarms are read (by number types) and
/// its initial value is left out; an argument type's initial value and
/// valid range are read and its alarms (XTCE 1.0 allows them) left out.
struct Lists {
    attrs: Vec<&'static str>,
    children: Vec<&'static str>,
    ignored: Vec<&'static str>,
}

impl Lists {
    fn new(
        role: Role,
        attrs: &[&'static str],
        children: &[&'static str],
        ignored: &[&'static str],
    ) -> Lists {
        let mut lists = Lists {
            attrs: attrs.to_vec(),
            children: [children, ENCODED].concat(),
            ignored: [ignored, DESCRIBED, &["DefaultAlarm", "ContextAlarmList"]].concat(),
        };
        match role {
            Role::Parameter => lists.ignored.push("initialValue"),
            Role::Argument => lists.attrs.push("initialValue"),
        }
        lists
    }

    /// A number type's lists.
    fn numeric(role: Role, attrs: &[&'static str]) -> Lists {
        let mut lists = Lists::new(role, attrs, &[], &["ToString"]);
        match role {
            Role::Parameter => {
                lists.ignored.retain(|name| *name != "DefaultAlarm");
                lists.children.push("DefaultAlarm");
                lists
                    .ignored
                    .extend(["ValidRange", "validRangeAppliesToCalibrated"]);
            }
            Role::Argument => {
                lists.children.extend(["ValidRange", "ValidRangeSet"]);
                lists.attrs.push("validRangeAppliesToCalibrated");
            }
        }
        lists
    }

    fn expect(&self) -> Expect<'_> {
        Expect {
            attrs: &self.attrs,
            children: &self.children,
            ignored: &self.ignored,
            text: false,
        }
    }
}

/// An argument type's initial value, as XTCE writes it.
fn initial<'d>(el: El<'d>, role: Role) -> Option<(&'d str, Span)> {
    match role {
        Role::Parameter => None,
        Role::Argument => el.attr("initialValue").map(|a| (a.value, a.span)),
    }
}

/// A number argument type's valid range: one ValidRange, alone (XTCE 1.0
/// and 1.1) or in a ValidRangeSet (1.1 and 1.2). Yamcs checks it against
/// the engineering value whatever `validRangeAppliesToCalibrated` says,
/// and the two are the same value for an uncalibrated type.
fn valid_range(el: El<'_>, integer: bool, report: &mut Report) -> Option<Option<ValidRange>> {
    // XTCE 1.2 says it on the set, XTCE 1.0 and 1.1 on the range or the
    // type.
    let mut applies = el.attr("validRangeAppliesToCalibrated");
    let range = match (el.child("ValidRangeSet"), el.child("ValidRange")) {
        (Some(_), Some(direct)) => {
            report.error(
                Code::Invalid,
                direct.span(),
                "a type has a ValidRangeSet or a ValidRange, not both",
            );
            return None;
        }
        (Some(set), None) => {
            set.check(
                &Expect {
                    attrs: &["validRangeAppliesToCalibrated"],
                    children: &["ValidRange"],
                    ignored: &[],
                    text: false,
                },
                report,
            );
            applies = set.attr("validRangeAppliesToCalibrated").or(applies);
            let ranges: Vec<El<'_>> = set.all("ValidRange").collect();
            match ranges.as_slice() {
                [range] => *range,
                [] => {
                    report.error(
                        Code::Invalid,
                        set.span(),
                        "a ValidRangeSet holds a ValidRange",
                    );
                    return None;
                }
                [_, second, ..] => {
                    report.error(
                        Code::Unsupported,
                        second.span(),
                        "more than one valid range isn't supported: an MDL argument has one \
                         range, and Yamcs loads one",
                    );
                    return None;
                }
            }
        }
        (None, Some(direct)) => direct,
        (None, None) => return Some(None),
    };
    let mut attrs = vec![
        "minInclusive",
        "maxInclusive",
        "validRangeAppliesToCalibrated",
    ];
    if !integer {
        attrs.extend(["minExclusive", "maxExclusive"]);
    }
    range.check(
        &Expect {
            attrs: &attrs,
            children: &[],
            ignored: &[],
            text: false,
        },
        report,
    );
    let end =
        |inclusive: &str, exclusive: &str, report: &mut Report| -> Option<Option<(f64, Span)>> {
            if let Some(attr) = range.attr(inclusive) {
                if let Some(other) = range.attr(exclusive) {
                    report.warning(
                    Code::Ignored,
                    other.span,
                    format!(
                        "{exclusive} isn't imported: the range has {inclusive} too, which Yamcs \
                         takes"
                    ),
                );
                }
                let value = if integer {
                    exact_double(attr, report)?
                } else {
                    finite(attr, report)?
                };
                return Some(Some((value, attr.span)));
            }
            let Some(attr) = range.attr(exclusive) else {
                return Some(None);
            };
            // The nearest double inside the range, which MDL's inclusive end
            // holds exactly.
            let value = finite(attr, report)?;
            let inside = if exclusive == "minExclusive" {
                value.next_up()
            } else {
                value.next_down()
            };
            Some(Some((inside, attr.span)))
        };
    let min = end("minInclusive", "minExclusive", report)?;
    let max = end("maxInclusive", "maxExclusive", report)?;
    let applies = range.attr("validRangeAppliesToCalibrated").or(applies);
    let raw = match applies {
        Some(attr) => (!xml::boolean(attr, report)?).then_some(attr.span),
        None => None,
    };
    Some((min.is_some() || max.is_some()).then_some(ValidRange { min, max, raw }))
}

/// An integer end of a range, which MDL holds within ±(2^53 - 1), where
/// every JSON reader agrees.
fn exact_double(attr: Attr<'_>, report: &mut Report) -> Option<f64> {
    let value = xml::long(attr, report)?;
    if i128::from(value).abs() <= SAFE {
        return Some(value as f64);
    }
    report.error(
        Code::Unsupported,
        attr.span,
        format!(
            "{value} isn't supported as a range's end: an MDL range's ends lie within ±(2^53 - \
             1), where every JSON reader agrees"
        ),
    );
    None
}

/// A finite double.
fn finite(attr: Attr<'_>, report: &mut Report) -> Option<f64> {
    let value = xml::double(attr, report)?;
    if value.is_finite() {
        return Some(value);
    }
    report.error(
        Code::Invalid,
        attr.span,
        format!("{} {value} isn't a finite number", attr.name),
    );
    None
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
    calibrator: Option<Calibrator>,
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
            conversion: self.calibrator.as_ref().map(Calibrator::conversion),
            inverse: None,
            unit: None,
            param: "f64",
            states: Vec::new(),
            raw: Raw::Other,
            limits: None,
            range: None,
            initial: None,
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
fn default_calibrator(el: El<'_>, report: &mut Report) -> Option<Calibrator> {
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
        let calibration = polynomial_calibration(polynomial, report)?;
        return Some(Calibrator { span, calibration });
    }
    if let Some(spline) = el.child("SplineCalibrator") {
        let calibration = spline_calibration(spline, report)?;
        return Some(Calibrator { span, calibration });
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

fn polynomial_calibration(el: El<'_>, report: &mut Report) -> Option<Calibration> {
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
    if coefficients.is_empty() {
        report.error(Code::Invalid, el.span(), "a polynomial has terms");
        return None;
    }
    Some(Calibration::Polynomial {
        coefficients,
        span: el.span(),
    })
}

fn spline_calibration(el: El<'_>, report: &mut Report) -> Option<Calibration> {
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
    match order.0 {
        0 | 1 => {}
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
    let mut points = Vec::new();
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
        points.push(SplinePoint {
            raw: (xml::double(raw, report)?, raw.span),
            calibrated: (xml::double(calibrated, report)?, calibrated.span),
            span: point.span(),
        });
    }
    Some(Calibration::Spline {
        points,
        order,
        extrapolate,
        span: el.span(),
    })
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
