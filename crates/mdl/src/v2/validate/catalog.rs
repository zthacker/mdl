//! Catalog checks: every canonical param is well formed.

use std::collections::BTreeSet;

use super::rules::{check_finite, check_not_empty, is_canonical_name};
use crate::v2::diagnostics::{Code, Place, Report};
use crate::v2::ir::{Catalog, Frame, Limits, Param, ParamType};

pub(super) fn check(catalog: &Catalog, place: &Place, report: &mut Report) {
    for (name, param) in &catalog.params {
        check_param(
            name,
            param,
            &place.map_item("params", name, "param"),
            report,
        );
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
        check_not_empty(unit, "unit", &place.key("unit"), report);
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

    let is_enum = param.ty == ParamType::Enum;
    if is_enum && param.states.is_empty() {
        report.error(
            Code::Empty,
            &place.key("states"),
            "an enum param lists its states",
        );
    } else if !is_enum && !param.states.is_empty() {
        report.error(
            Code::ParamShape,
            &place.key("states"),
            "only enum params have states",
        );
    }
    let mut seen = BTreeSet::new();
    for (index, state) in param.states.iter().enumerate() {
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
