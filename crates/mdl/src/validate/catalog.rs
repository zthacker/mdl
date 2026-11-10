//! Catalog checks: every canonical param is well formed.

use std::collections::BTreeSet;

use super::rules::{
    MAX_SAFE_INTEGER, check_finite, check_integer_spelling, check_not_empty, check_scalar,
    check_unit_code, integer_value, is_canonical_name, is_identifier,
};
use crate::diagnostics::{Code, Place, Report};
use crate::ir::{
    Argument, ArgumentRange, Catalog, Command, Frame, Limits, ListBounds, Literal, Param,
    ParamType, Scalar,
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
        check_range(argument.ty, range, &place.key("range"), report);
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

fn is_integer_type(ty: ParamType) -> bool {
    matches!(ty, ParamType::I64 | ParamType::U64)
}

fn check_range(ty: ParamType, range: &ArgumentRange, place: &Place, report: &mut Report) {
    if !matches!(ty, ParamType::F64 | ParamType::I64 | ParamType::U64) {
        report.error(
            Code::ParamShape,
            place,
            "only numeric arguments (f64, i64, u64) have a range",
        );
        return;
    }
    let bounds = [("min", range.min), ("max", range.max)];
    if bounds.iter().all(|(_, bound)| bound.is_none()) {
        report.error(
            Code::Empty,
            place,
            "range gives min, max or both; omit range otherwise",
        );
        return;
    }
    for (name, bound) in bounds {
        let Some(bound) = bound else {
            continue;
        };
        let bound_place = place.key(name);
        check_finite(bound, name, &bound_place, report);
        if is_integer_type(ty) && bound.is_finite() {
            if bound.fract() != 0.0 {
                report.error(
                    Code::ValueRange,
                    &bound_place,
                    format!("an integer argument's {name} is an integer, not {bound}"),
                );
            } else if bound.abs() > MAX_SAFE_INTEGER as f64 {
                report.error(
                    Code::UnsafeInteger,
                    &bound_place,
                    format!(
                        "{name} {bound} is outside ±(2^53 - 1), where every JSON reader agrees"
                    ),
                );
            }
        }
    }
    if let (Some(min), Some(max)) = (range.min, range.max)
        && min > max
    {
        report.error(
            Code::LimitsOrder,
            place,
            format!("min {min} is greater than max {max}"),
        );
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
    let number = match (argument.ty, default) {
        (ParamType::Bool, Literal::Bool(_)) => None,
        (ParamType::Bool, _) => return wrong(report, "a bool"),
        (ParamType::F64, Literal::Int(n)) => Some(*n as f64),
        (ParamType::F64, Literal::Float(x)) => Some(*x),
        (ParamType::F64, _) => return wrong(report, "a number"),
        (ParamType::I64 | ParamType::U64, Literal::Int(_) | Literal::Str(_)) => {
            let scalar = match default {
                Literal::Int(n) => Scalar::Int(*n),
                Literal::Str(text) => Scalar::Str(text.clone()),
                _ => return,
            };
            check_scalar(&scalar, "default", place, report);
            check_integer_spelling(&scalar, "default", place, report);
            let (low, high) = if argument.ty == ParamType::U64 {
                (0, i128::from(u64::MAX))
            } else {
                (i128::from(i64::MIN), i128::from(i64::MAX))
            };
            match integer_value(&scalar) {
                Some(n) if (low..=high).contains(&n) => Some(n as f64),
                _ => {
                    return wrong(
                        report,
                        if argument.ty == ParamType::U64 {
                            "an unsigned 64-bit integer"
                        } else {
                            "a signed 64-bit integer"
                        },
                    );
                }
            }
        }
        (ParamType::I64 | ParamType::U64, _) => return wrong(report, "an integer"),
        (ParamType::Str, Literal::Str(_)) => None,
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
            None
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
            None
        }
        (ParamType::Enum, _) => return wrong(report, "an enum (a state name)"),
        // Reported on the argument's type.
        (ParamType::Vec3 | ParamType::Quat, _) => None,
    };
    // A default isn't judged against a range that's wrong itself.
    let usable = argument.range.filter(|range| {
        let finite = range.min.is_none_or(f64::is_finite) && range.max.is_none_or(f64::is_finite);
        let ordered = match (range.min, range.max) {
            (Some(min), Some(max)) => min <= max,
            _ => true,
        };
        finite && ordered
    });
    if let (Some(value), Some(range)) = (number, usable) {
        let below = range.min.is_some_and(|min| value < min);
        let above = range.max.is_some_and(|max| value > max);
        if below || above {
            report.error(
                Code::DefaultValue,
                place,
                format!(
                    "default {value} is outside argument {}'s range",
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
