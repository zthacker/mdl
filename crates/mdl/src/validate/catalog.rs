//! Catalog checks: every canonical param is well formed.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use super::rules::{
    check_finite, check_integer_spelling, check_not_empty, check_scalar, check_unit_code,
    integer_value, is_canonical_name, is_identifier,
};
use crate::canonical::format_double;
use crate::diagnostics::{Code, Place, Report};
use crate::encode::show_end;
use crate::ir::{
    Argument, ArgumentRange, Catalog, Command, Frame, Limits, ListBounds, Literal, Param,
    ParamType, RangeEnds, Scalar,
};

pub(super) fn check(catalog: &Catalog, place: &Place, report: &mut Report) {
    for (name, param) in &catalog.params {
        check_param(
            name,
            param,
            &place.map_item("params", name, "param"),
            report,
        );
    }
    for (name, command) in &catalog.commands {
        let command_place = place.map_item("commands", name, "command");
        check_command(name, command, catalog, &command_place, report);
    }
}

fn check_param(name: &str, param: &Param, place: &Place, report: &mut Report) {
    if !is_canonical_name(name) {
        report.error(
            Code::InvalidName,
            place,
            "param names are two or more lowercase words joined by dots, like eps.bus_voltage",
        );
    }
    if let Some(unit) = &param.unit {
        check_unit_code(unit, &place.key("unit"), report);
    }
    if let Some(description) = &param.description {
        check_not_empty(
            description,
            "description",
            &place.key("description"),
            report,
        );
    }
    if let Some(deprecated) = &param.deprecated {
        check_not_empty(
            deprecated,
            "deprecated (say what replaces the param)",
            &place.key("deprecated"),
            report,
        );
    }

    check_states(param.ty, &param.states, "param", place, report);

    let is_vector = matches!(param.ty, ParamType::Vec3 | ParamType::Quat);
    match (&param.frame, is_vector) {
        (None, true) => report.error(
            Code::ParamShape,
            &place.key("frame"),
            "vec3 and quat params name the frame consumers receive",
        ),
        (Some(_), false) => report.error(
            Code::ParamShape,
            &place.key("frame"),
            "only vec3 and quat params have a frame",
        ),
        (Some(Frame::Geodetic), true) => report.error(
            Code::ParamShape,
            &place.key("frame"),
            "GEODETIC is a source frame (latitude, longitude, altitude); a catalog param uses \
             ECEF, ECI_J2000, BODY, NED or ENU, and sources convert",
        ),
        _ => {}
    }

    if let Some(limits) = &param.limits {
        check_limits(param.ty, limits, &place.key("limits"), report);
    }
}

/// Enums list their states, each once; nothing else has states.
fn check_states(ty: ParamType, states: &[String], what: &str, place: &Place, report: &mut Report) {
    let is_enum = ty == ParamType::Enum;
    if is_enum && states.is_empty() {
        report.error(
            Code::Empty,
            &place.key("states"),
            format!("an enum {what} lists its states"),
        );
    } else if !is_enum && !states.is_empty() {
        report.error(
            Code::ParamShape,
            &place.key("states"),
            format!("only enum {what}s have states"),
        );
    }
    let mut seen = BTreeSet::new();
    for (index, state) in states.iter().enumerate() {
        let state_place = place.at("states", index);
        check_not_empty(state, "a state name", &state_place, report);
        if !seen.insert(state) {
            report.error(
                Code::DuplicateName,
                &state_place,
                format!("state {state:?} is listed twice"),
            );
        }
    }
}

fn check_command(
    name: &str,
    command: &Command,
    catalog: &Catalog,
    place: &Place,
    report: &mut Report,
) {
    if !is_canonical_name(name) {
        report.error(
            Code::InvalidName,
            place,
            "command names are two or more lowercase words joined by dots, like eps.set_mode",
        );
    }
    if catalog.params.contains_key(name) {
        report.error(
            Code::DuplicateName,
            place,
            format!("{name} is also a param; a canonical name means one thing"),
        );
    }
    if let Some(description) = &command.description {
        check_not_empty(
            description,
            "description",
            &place.key("description"),
            report,
        );
    }
    if let Some(deprecated) = &command.deprecated {
        check_not_empty(
            deprecated,
            "deprecated (say what replaces the command)",
            &place.key("deprecated"),
            report,
        );
    }
    let mut seen = BTreeSet::new();
    for (index, argument) in command.arguments.iter().enumerate() {
        let argument_place = place.item("arguments", index, "argument", &argument.name);
        if !is_identifier(&argument.name) {
            report.error(
                Code::InvalidName,
                &argument_place.key("name"),
                "argument names are letters, digits and '_', not starting with a digit",
            );
        }
        if !seen.insert(&argument.name) {
            report.error(
                Code::DuplicateName,
                &argument_place.key("name"),
                format!("argument {} is listed twice", argument.name),
            );
        }
        check_argument(argument, &argument_place, report);
    }
}

fn check_argument(argument: &Argument, place: &Place, report: &mut Report) {
    if matches!(argument.ty, ParamType::Vec3 | ParamType::Quat) {
        report.error(
            Code::ParamShape,
            &place.key("type"),
            "command arguments are scalars: f64, i64, u64, bool, str, bytes or enum",
        );
    }
    if let Some(unit) = &argument.unit {
        check_unit_code(unit, &place.key("unit"), report);
    }
    if let Some(description) = &argument.description {
        check_not_empty(
            description,
            "description",
            &place.key("description"),
            report,
        );
    }
    check_states(argument.ty, &argument.states, "argument", place, report);
    if let Some(range) = &argument.range {
        check_range(argument, range, &place.key("range"), report);
    }
    if let Some(list) = &argument.list {
        check_list(list, &place.key("list"), report);
    }
    let at = place.key("default");
    match (&argument.default, argument.list) {
        (Some(Literal::List(values)), Some(list)) => {
            check_list_default(argument, list, values, place, report);
        }
        (Some(_), Some(_)) => report.error(
            Code::DefaultValue,
            &at,
            format!(
                "argument {} is a list, so its default is a list of values",
                argument.name
            ),
        ),
        (Some(Literal::List(_)), None) => report.error(
            Code::DefaultValue,
            &at,
            format!(
                "argument {} takes a single value, so its default is one, not a list",
                argument.name
            ),
        ),
        (Some(default), None) => check_default(argument, default, &at, report),
        (None, _) => {}
    }
}

/// A list argument's default (at the argument's `place`): as many values
/// as the list takes, each a single value its type, states and range allow.
fn check_list_default(
    argument: &Argument,
    list: ListBounds,
    values: &[Literal],
    place: &Place,
    report: &mut Report,
) {
    let count = values.len();
    let fits = |bound: Option<u32>, keep: fn(usize, usize) -> bool| {
        bound.is_none_or(|bound| usize::try_from(bound).is_ok_and(|bound| keep(count, bound)))
    };
    if !fits(list.min, |count, min| count >= min) || !fits(list.max, |count, max| count <= max) {
        report.error(
            Code::DefaultValue,
            &place.key("default"),
            format!(
                "argument {}'s default has {count} values, outside the {} its list takes",
                argument.name,
                bounds(list)
            ),
        );
    }
    for (index, value) in values.iter().enumerate() {
        let at = place.at("default", index);
        match value {
            Literal::List(_) => report.error(
                Code::DefaultValue,
                &at,
                format!(
                    "argument {}'s default is a list of single values, not of lists",
                    argument.name
                ),
            ),
            value => check_default(argument, value, &at, report),
        }
    }
}

/// A list's bounds in words.
fn bounds(list: ListBounds) -> String {
    match (list.min, list.max) {
        (Some(min), Some(max)) if min == max => format!("exactly {min}"),
        (Some(min), Some(max)) => format!("{min} to {max}"),
        (Some(min), None) => format!("at least {min}"),
        (None, Some(max)) => format!("at most {max}"),
        (None, None) => "any number".to_string(),
    }
}

/// A list's bounds: the least no more than the most, and 0 left implicit.
fn check_list(list: &ListBounds, place: &Place, report: &mut Report) {
    if list.min == Some(0) {
        report.error(
            Code::ExplicitDefault,
            &place.key("min"),
            "0 is the default min; omit it",
        );
    }
    if let (Some(min), Some(max)) = (list.min, list.max)
        && min > max
    {
        report.error(
            Code::LimitsOrder,
            place,
            format!("min {min} is greater than max {max}"),
        );
    }
}

/// A range's ends are values of the argument's type, written as a default
/// is, in order.
fn check_range(argument: &Argument, range: &ArgumentRange, place: &Place, report: &mut Report) {
    if !matches!(
        argument.ty,
        ParamType::F64 | ParamType::I64 | ParamType::U64
    ) {
        report.error(
            Code::ParamShape,
            place,
            "only numeric arguments (f64, i64, u64) have a range",
        );
        return;
    }
    let bounds = [("min", &range.min), ("max", &range.max)];
    if bounds.iter().all(|(_, bound)| bound.is_none()) {
        report.error(
            Code::Empty,
            place,
            "range gives min, max or both; omit range otherwise",
        );
        return;
    }
    for (name, bound) in bounds {
        if let Some(bound) = bound {
            check_end(argument, bound, name, &place.key(name), report);
        }
    }
    if let (Some(min), Some(max)) = (&range.min, &range.max)
        && min.compare_end(max) == Some(Ordering::Greater)
    {
        report.error(
            Code::LimitsOrder,
            place,
            format!(
                "min {} is greater than max {}",
                show_end(min),
                show_end(max)
            ),
        );
    }
}

/// One end of a range: a finite number for an f64 argument; for a u64 or
/// i64 one an integer of the type, a number within ±(2^53 − 1) and a
/// decimal string beyond, as a default is written.
fn check_end(argument: &Argument, end: &Literal, name: &str, place: &Place, report: &mut Report) {
    match (argument.ty, end) {
        (ParamType::F64, Literal::Int(_)) => {}
        (ParamType::F64, Literal::Float(x)) => check_finite(*x, name, place, report),
        (ParamType::F64, _) => report.error(
            Code::ValueRange,
            place,
            format!("an f64 argument's {name} is a number"),
        ),
        (_, Literal::Float(x)) if x.fract() != 0.0 => report.error(
            Code::ValueRange,
            place,
            format!(
                "an integer argument's {name} is an integer, not {}",
                format_double(*x)
            ),
        ),
        // A whole number past i64 reads as a double (`ir::Literal`).
        (_, Literal::Float(x)) => report.error(
            Code::UnsafeInteger,
            place,
            format!(
                "{name} {} is outside ±(2^53 - 1), so some JSON readers would change it; write \
                 it as a decimal string",
                format_double(*x)
            ),
        ),
        (_, Literal::Int(n)) => check_scalar(&Scalar::Int(*n), name, place, report),
        (_, Literal::Str(text)) if end.as_integer().is_some() => {
            check_integer_spelling(&Scalar::Str(text.clone()), name, place, report);
        }
        _ => {
            return report.error(
                Code::ValueRange,
                place,
                format!(
                    "an integer argument's {name} is an integer: a number, or a decimal string \
                     past ±(2^53 - 1)"
                ),
            );
        }
    }
    // An integer end is a value of the type, as a default is.
    if let (Some((low, high, what)), Some(n)) = (integer_type(argument.ty), end.as_integer())
        && !(low..=high).contains(&n)
    {
        report.error(
            Code::ValueRange,
            place,
            format!("argument {} is {what}, so its {name} is too", argument.name),
        );
    }
}

/// The integers a u64 or i64 argument can be, and what it is.
fn integer_type(ty: ParamType) -> Option<(i128, i128, &'static str)> {
    match ty {
        ParamType::U64 => Some((0, i128::from(u64::MAX), "an unsigned 64-bit integer")),
        ParamType::I64 => Some((
            i128::from(i64::MIN),
            i128::from(i64::MAX),
            "a signed 64-bit integer",
        )),
        _ => None,
    }
}

/// Whether an argument's range ends are values of its type, in order, so
/// a default can be judged against them.
fn usable(argument: &Argument) -> bool {
    match argument.range_ends() {
        Some(RangeEnds::Numbers(Some(min), Some(max))) => min <= max,
        Some(RangeEnds::Integers(Some(min), Some(max))) => min <= max,
        Some(_) => true,
        None => false,
    }
}

/// A default has the argument's type and lies in its range.
fn check_default(argument: &Argument, default: &Literal, place: &Place, report: &mut Report) {
    let wrong = |report: &mut Report, expected: &str| {
        report.error(
            Code::DefaultValue,
            place,
            format!(
                "argument {} is {expected}, so its default is too",
                argument.name
            ),
        );
    };
    let numeric = match (argument.ty, default) {
        (ParamType::Bool, Literal::Bool(_)) => false,
        (ParamType::Bool, _) => return wrong(report, "a bool"),
        (ParamType::F64, Literal::Int(_) | Literal::Float(_)) => true,
        (ParamType::F64, _) => return wrong(report, "a number"),
        // A whole number past i64 reads as a double (`ir::Literal`).
        (ParamType::I64 | ParamType::U64, Literal::Float(x)) if x.fract() == 0.0 => {
            report.error(
                Code::UnsafeInteger,
                place,
                format!(
                    "default {} is outside ±(2^53 - 1), so some JSON readers would change it; \
                     write it as a decimal string",
                    format_double(*x)
                ),
            );
            return;
        }
        (ParamType::I64 | ParamType::U64, Literal::Int(_) | Literal::Str(_)) => {
            let scalar = match default {
                Literal::Int(n) => Scalar::Int(*n),
                Literal::Str(text) => Scalar::Str(text.clone()),
                _ => return,
            };
            check_scalar(&scalar, "default", place, report);
            check_integer_spelling(&scalar, "default", place, report);
            let Some((low, high, what)) = integer_type(argument.ty) else {
                return;
            };
            match integer_value(&scalar) {
                Some(n) if (low..=high).contains(&n) => true,
                _ => return wrong(report, what),
            }
        }
        (ParamType::I64 | ParamType::U64, _) => return wrong(report, "an integer"),
        (ParamType::Str, Literal::Str(_)) => false,
        (ParamType::Str, _) => return wrong(report, "a string"),
        (ParamType::Bytes, Literal::Str(hex)) => {
            let lowercase_hex = hex.len() % 2 == 0
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            if !lowercase_hex {
                report.error(
                    Code::DefaultValue,
                    place,
                    format!(
                        "a bytes default is lowercase hexadecimal, two digits a byte, not {hex:?}"
                    ),
                );
            }
            false
        }
        (ParamType::Bytes, _) => return wrong(report, "bytes (lowercase hexadecimal)"),
        (ParamType::Enum, Literal::Str(state)) => {
            if !argument.states.is_empty() && !argument.states.contains(state) {
                report.error(
                    Code::DefaultValue,
                    place,
                    format!("{state:?} isn't one of argument {}'s states", argument.name),
                );
            }
            false
        }
        (ParamType::Enum, _) => return wrong(report, "an enum (a state name)"),
        // Reported on the argument's type.
        (ParamType::Vec3 | ParamType::Quat, _) => false,
    };
    // A default isn't judged against a range that's wrong itself.
    if let (true, Some(range)) = (numeric, &argument.range)
        && usable(argument)
    {
        let below = range
            .min
            .as_ref()
            .is_some_and(|min| default.compare_end(min) == Some(Ordering::Less));
        let above = range
            .max
            .as_ref()
            .is_some_and(|max| default.compare_end(max) == Some(Ordering::Greater));
        if below || above {
            report.error(
                Code::DefaultValue,
                place,
                format!(
                    "default {} is outside argument {}'s range",
                    show_end(default),
                    argument.name
                ),
            );
        }
    }
}

fn check_limits(ty: ParamType, limits: &Limits, place: &Place, report: &mut Report) {
    if !matches!(ty, ParamType::F64 | ParamType::I64 | ParamType::U64) {
        report.error(
            Code::ParamShape,
            place,
            "only numeric params (f64, i64, u64) have limits",
        );
    }
    if limits.red_low.is_none()
        && limits.yellow_low.is_none()
        && limits.yellow_high.is_none()
        && limits.red_high.is_none()
    {
        report.error(
            Code::Empty,
            place,
            "limits lists at least one limit; omit limits otherwise",
        );
        return;
    }
    let named = [
        ("red_low", limits.red_low),
        ("yellow_low", limits.yellow_low),
        ("yellow_high", limits.yellow_high),
        ("red_high", limits.red_high),
    ];
    let mut finite = true;
    for (name, value) in named {
        if let Some(value) = value {
            check_finite(value, name, &place.key(name), report);
            finite &= value.is_finite();
        }
    }
    if !finite {
        return;
    }
    let present: Vec<(&str, f64)> = named
        .iter()
        .filter_map(|(name, value)| value.map(|v| (*name, v)))
        .collect();
    if let Some(pair) = present.windows(2).find(|pair| pair[0].1 > pair[1].1) {
        report.error(
            Code::LimitsOrder,
            place,
            format!(
                "{} ({}) is above {} ({}); limits run red_low <= yellow_low <= yellow_high <= \
                 red_high",
                pair[0].0, pair[0].1, pair[1].0, pair[1].1
            ),
        );
    }
}
