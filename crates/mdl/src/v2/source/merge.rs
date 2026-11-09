//! Bundles from several frontends as one.
//!
//! A program may read definitions with more than one frontend: a catalog
//! written in TOML beside a vendor's packets imported from XTCE, whose
//! parameters map to the catalog's names. [`merge`] makes their bundles
//! one before validation, keeping where each value came from.

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::Value;

use super::{Parsed, SourceDiagnostic, SourceMap};
use crate::v2::diagnostics::{Code, escape_pointer_token};
use crate::v2::ir::{Argument, Bundle, Catalog, Command, Param};

/// Bundles as one, in the order given, which is also their precedence.
///
/// Families and sets are appended, so two with one name reach validation,
/// which reports them as it would in one bundle. Catalog params and
/// commands merge by name. Two definitions of one param merge when they
/// agree on what decoding depends on: type, unit, states (in order) and
/// frame. Two of one command merge when their arguments agree, one by one,
/// on everything but their descriptions. Any other pair is an error at the
/// later definition's value, naming the earlier's.
///
/// A value that changes neither decoding nor encoding (limits, a
/// description, a deprecation note) is taken from the later definition
/// when only it gives one. When both give one and they differ, the
/// earlier's is kept, with an [`overridden`](Code::Overridden) warning at
/// the later's.
///
/// The bundle's IR version is the newest any part has, each having been
/// read already. Every part's spans must index one
/// [`Sources`](super::Sources), the one each frontend was given its files
/// from: spans from separate ones would point into the wrong files. Where
/// the parts' maps record one pointer twice, the earlier part's span is
/// kept, and a list or map assembled from several parts (the catalog's
/// `params`, the bundle's `sets`) has no span of its own.
///
/// The diagnostics, each part's warnings and the merge's own, are in file
/// order, then by position: the order of the files in the shared
/// `Sources`, which needn't be the order of the parts. On failure, the
/// warnings are returned with the errors.
pub fn merge(parts: impl IntoIterator<Item = Parsed>) -> Result<Parsed, Vec<SourceDiagnostic>> {
    let mut parts = parts.into_iter();
    let Some(mut merged) = parts.next() else {
        return Err(vec![SourceDiagnostic::error(
            Code::Empty.as_str(),
            "there are no bundles to merge",
        )]);
    };
    let mut assembled = BTreeSet::new();
    let mut errors = Vec::new();
    for part in parts {
        merge_into(&mut merged, part, &mut assembled, &mut errors);
    }
    let failed = !errors.is_empty();
    let mut diagnostics = std::mem::take(&mut merged.warnings);
    diagnostics.extend(errors);
    diagnostics.sort_by_key(|d| d.span.map(|span| (span.source, span.start)));
    if failed {
        Err(diagnostics)
    } else {
        merged.warnings = diagnostics;
        Ok(merged)
    }
}

/// The bundle's own objects, lists and maps, whose spans cover what one
/// part gave.
const CONTAINERS: [&str; 6] = [
    "",
    "/catalog",
    "/catalog/params",
    "/catalog/commands",
    "/families",
    "/sets",
];

fn merge_into(
    merged: &mut Parsed,
    part: Parsed,
    assembled: &mut BTreeSet<&'static str>,
    errors: &mut Vec<SourceDiagnostic>,
) {
    let Parsed {
        bundle,
        map,
        warnings,
    } = part;
    let Bundle {
        mdl_version,
        catalog,
        families,
        sets,
    } = bundle;
    let Catalog { params, commands } = catalog;
    merged.bundle.mdl_version = merged.bundle.mdl_version.max(mdl_version);
    merged.warnings.extend(warnings);
    // For each container: whether earlier parts gave it items, and
    // whether this part does. The bundle and its catalog always hold
    // both parts' values.
    let contributions = [
        ("", true, true),
        ("/catalog", true, true),
        (
            "/catalog/params",
            !merged.bundle.catalog.params.is_empty(),
            !params.is_empty(),
        ),
        (
            "/catalog/commands",
            !merged.bundle.catalog.commands.is_empty(),
            !commands.is_empty(),
        ),
        (
            "/families",
            !merged.bundle.families.is_empty(),
            !families.is_empty(),
        ),
        ("/sets", !merged.bundle.sets.is_empty(), !sets.is_empty()),
    ];

    // Pointers to values this part gave that an earlier part's replaced:
    // their spans would locate values the bundle doesn't have.
    let mut dropped = Vec::new();
    for (name, param) in params {
        let Some(existing) = merged.bundle.catalog.params.get_mut(&name) else {
            merged.bundle.catalog.params.insert(name, param);
            continue;
        };
        let defined = Defined::new("param", &name, &map, &merged.map);
        match combine_param(existing, param) {
            Ok(overridden) => {
                for value in overridden {
                    dropped.push(defined.at(&value.field));
                    merged.warnings.push(defined.overridden(value));
                }
            }
            Err(conflict) => errors.push(defined.conflict(conflict)),
        }
    }
    for (name, command) in commands {
        let Some(existing) = merged.bundle.catalog.commands.get_mut(&name) else {
            merged.bundle.catalog.commands.insert(name, command);
            continue;
        };
        let defined = Defined::new("command", &name, &map, &merged.map);
        match combine_command(existing, command) {
            Ok(overridden) => {
                for value in overridden {
                    dropped.push(defined.at(&value.field));
                    merged.warnings.push(defined.overridden(value));
                }
            }
            Err(conflict) => errors.push(defined.conflict(conflict)),
        }
    }

    let families_before = merged.bundle.families.len();
    let sets_before = merged.bundle.sets.len();
    merged.bundle.families.extend(families);
    merged.bundle.sets.extend(sets);
    for (container, earlier, later) in contributions {
        if assembled.contains(container) || !later {
            continue;
        }
        if earlier {
            merged.map.spans.remove(container);
            assembled.insert(container);
        } else if let Some(span) = map.get(container) {
            merged.map.spans.insert(container.to_string(), span);
        } else {
            merged.map.spans.remove(container);
        }
    }
    for (pointer, span) in map.spans {
        if CONTAINERS.contains(&pointer.as_str()) {
            continue;
        }
        if dropped.iter().any(|d| within(&pointer, d)) {
            continue;
        }
        let pointer = moved(&pointer, "/families/", families_before)
            .or_else(|| moved(&pointer, "/sets/", sets_before))
            .unwrap_or(pointer);
        merged.map.spans.entry(pointer).or_insert(span);
    }
}

/// Is `pointer` the value at `at`, or inside it?
fn within(pointer: &str, at: &str) -> bool {
    pointer
        .strip_prefix(at)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// A pointer into a list (`/sets/3/...`) with the item's index moved on by
/// `by`, for a part whose items follow earlier parts'. Only an index
/// written as JSON Pointer writes one (no sign, no leading zeros) moves.
fn moved(pointer: &str, list: &str, by: usize) -> Option<String> {
    let rest = pointer.strip_prefix(list)?;
    let (index, tail) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let digits = !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit());
    if !digits || (index.starts_with('0') && index != "0") {
        return None;
    }
    let index: usize = index.parse().ok()?;
    Some(format!("{list}{}{tail}", index + by))
}

/// A param or command an earlier part defines and this one defines again.
struct Defined<'a> {
    what: &'static str,
    name: &'a str,
    pointer: String,
    later: &'a SourceMap,
    earlier: &'a SourceMap,
}

impl<'a> Defined<'a> {
    fn new(
        what: &'static str,
        name: &'a str,
        later: &'a SourceMap,
        earlier: &'a SourceMap,
    ) -> Self {
        Defined {
            what,
            name,
            pointer: format!("/catalog/{what}s/{}", escape_pointer_token(name)),
            later,
            earlier,
        }
    }

    /// The pointer to one of the definition's values (`unit`,
    /// `arguments/0/range`).
    fn at(&self, field: &str) -> String {
        format!("{}/{field}", self.pointer)
    }

    fn conflict(&self, conflict: Conflict) -> SourceDiagnostic {
        let message = format!(
            "{} {} is defined again with {} {}; the earlier definition has {}",
            self.what, self.name, conflict.about, conflict.later, conflict.earlier
        );
        self.locate(
            SourceDiagnostic::error(Code::DuplicateName.as_str(), message),
            &conflict.field,
        )
    }

    fn overridden(&self, value: Overridden) -> SourceDiagnostic {
        let message = format!(
            "{} {} has {} {} here, but the earlier definition's {} takes precedence",
            self.what, self.name, value.about, value.later, value.earlier
        );
        self.locate(
            SourceDiagnostic::warning(Code::Overridden.as_str(), message),
            &value.field,
        )
    }

    /// At the later definition's value, or where the definition is when
    /// it leaves the value out, with the earlier's as the first location.
    fn locate(&self, mut diagnostic: SourceDiagnostic, field: &str) -> SourceDiagnostic {
        let path = self.at(field);
        if let Some(span) = self.later.find(&path) {
            diagnostic = diagnostic.with_span(span);
        }
        let first = self
            .earlier
            .get(&path)
            .or_else(|| self.earlier.get(&self.pointer));
        if let Some(first) = first {
            diagnostic = diagnostic.with_first(first);
        }
        diagnostic.with_path(path)
    }
}

/// What two definitions disagree on.
#[derive(Debug, PartialEq)]
struct Conflict {
    /// Where, below the definition: `unit`, `arguments/1/range`.
    field: String,
    /// What, for people: `unit`, `argument mode's range`.
    about: String,
    earlier: String,
    later: String,
}

/// A value the later definition gives that the earlier's replaces, each
/// as JSON.
#[derive(Debug, PartialEq)]
struct Overridden {
    field: String,
    /// `description`, `argument n's description`.
    about: String,
    earlier: String,
    later: String,
}

/// One param from two definitions that agree on what decoding depends on,
/// with the later's values the earlier's replaced; or what they disagree
/// on, leaving `existing` as it was.
fn combine_param(existing: &mut Param, other: Param) -> Result<Vec<Overridden>, Conflict> {
    let Param {
        ty,
        unit,
        states,
        frame,
        limits,
        description,
        deprecated,
    } = other;
    agree("type", "type", &existing.ty, &ty)?;
    agree("unit", "unit", &existing.unit, &unit)?;
    agree_names("states", "states", &existing.states, &states)?;
    agree("frame", "frame", &existing.frame, &frame)?;
    let mut overridden = Vec::new();
    keep(
        &mut existing.limits,
        limits,
        "limits",
        "limits",
        &mut overridden,
    );
    keep(
        &mut existing.description,
        description,
        "description",
        "description",
        &mut overridden,
    );
    keep(
        &mut existing.deprecated,
        deprecated,
        "deprecated",
        "deprecation note",
        &mut overridden,
    );
    Ok(overridden)
}

/// One command from two definitions whose arguments agree on everything
/// but their descriptions, with the later's values the earlier's
/// replaced; or what they disagree on, leaving `existing` as it was.
fn combine_command(existing: &mut Command, other: Command) -> Result<Vec<Overridden>, Conflict> {
    let Command {
        arguments,
        description,
        deprecated,
    } = other;
    let names = |arguments: &[Argument]| -> Vec<String> {
        arguments.iter().map(|a| a.name.clone()).collect()
    };
    agree_names(
        "arguments",
        "arguments",
        &names(&existing.arguments),
        &names(&arguments),
    )?;
    let mut descriptions = Vec::with_capacity(arguments.len());
    for (index, (kept, argument)) in existing.arguments.iter().zip(arguments).enumerate() {
        let Argument {
            name,
            ty,
            unit,
            states,
            range,
            list,
            default,
            description,
        } = argument;
        let field = |key: &str| format!("arguments/{index}/{key}");
        let about = |what: &str| format!("argument {name}'s {what}");
        agree(&field("type"), &about("type"), &kept.ty, &ty)?;
        agree(&field("unit"), &about("unit"), &kept.unit, &unit)?;
        agree_names(&field("states"), &about("states"), &kept.states, &states)?;
        agree(&field("range"), &about("range"), &kept.range, &range)?;
        agree(&field("list"), &about("list bounds"), &kept.list, &list)?;
        agree(
            &field("default"),
            &about("default"),
            &kept.default,
            &default,
        )?;
        descriptions.push(description);
    }
    let mut overridden = Vec::new();
    for (index, (kept, description)) in existing.arguments.iter_mut().zip(descriptions).enumerate()
    {
        let about = format!("argument {}'s description", kept.name);
        keep(
            &mut kept.description,
            description,
            &format!("arguments/{index}/description"),
            &about,
            &mut overridden,
        );
    }
    keep(
        &mut existing.description,
        description,
        "description",
        "description",
        &mut overridden,
    );
    keep(
        &mut existing.deprecated,
        deprecated,
        "deprecated",
        "deprecation note",
        &mut overridden,
    );
    Ok(overridden)
}

fn agree<T: PartialEq + Serialize>(
    field: &str,
    about: &str,
    earlier: &T,
    later: &T,
) -> Result<(), Conflict> {
    if earlier == later {
        return Ok(());
    }
    Err(Conflict {
        field: field.to_string(),
        about: about.to_string(),
        earlier: shown(earlier),
        later: shown(later),
    })
}

/// Like [`agree`], for a list of names shown as `OFF, ON`.
fn agree_names(
    field: &str,
    about: &str,
    earlier: &[String],
    later: &[String],
) -> Result<(), Conflict> {
    if earlier == later {
        return Ok(());
    }
    let shown = |names: &[String]| {
        if names.is_empty() {
            "none".to_string()
        } else {
            names.join(", ")
        }
    };
    Err(Conflict {
        field: field.to_string(),
        about: about.to_string(),
        earlier: shown(earlier),
        later: shown(later),
    })
}

/// The earlier value, or the later one when there's no earlier one. A
/// later one that differs is recorded as overridden.
fn keep<T: PartialEq + Serialize>(
    existing: &mut Option<T>,
    other: Option<T>,
    field: &str,
    about: &str,
    overridden: &mut Vec<Overridden>,
) {
    match (existing.as_ref(), other) {
        (None, other) => *existing = other,
        (Some(kept), Some(given)) if *kept != given => overridden.push(Overridden {
            field: field.to_string(),
            about: about.to_string(),
            earlier: json_text(kept),
            later: json_text(&given),
        }),
        _ => {}
    }
}

/// A value as compact JSON: text in quotes, limits as an object.
fn json_text<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|err| format!("(unprintable: {err})"))
}

/// A value as the IR writes it: text as it is (`f64`, `V`), nothing as
/// `none`, anything else as compact JSON.
fn shown<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::Null) => "none".to_string(),
        Ok(Value::String(text)) => text,
        Ok(other) => other.to_string(),
        Err(err) => format!("(unprintable: {err})"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::v2::ir::IrVersion;
    use crate::v2::json;
    use crate::v2::source::{Sources, Span};
    use crate::v2::validate::validate;

    /// Each document read as a bundle from one `Sources`.
    fn read(documents: &[Value]) -> (Sources, Vec<Parsed>) {
        let mut sources = Sources::new();
        for (index, document) in documents.iter().enumerate() {
            let text = serde_json::to_string_pretty(document).unwrap();
            sources.add(format!("{index}.json"), text);
        }
        let parts = sources
            .iter()
            .map(|file| json::read(file).unwrap())
            .collect();
        (sources, parts)
    }

    fn merged(documents: &[Value]) -> (Sources, Result<Parsed, Vec<SourceDiagnostic>>) {
        let (sources, parts) = read(documents);
        (sources, merge(parts))
    }

    fn catalog(params: Value, commands: Value) -> Value {
        json!({ "mdl_version": "2.0", "catalog": { "params": params, "commands": commands } })
    }

    /// Each diagnostic as (code, message, path, file of its span, file of
    /// its first location).
    fn shown_all(
        sources: &Sources,
        diagnostics: &[SourceDiagnostic],
    ) -> Vec<(String, String, String, String, String)> {
        let file = |span: Option<Span>| {
            span.and_then(|s| sources.name(s.source))
                .unwrap_or("-")
                .to_string()
        };
        diagnostics
            .iter()
            .map(|d| {
                (
                    d.code.to_string(),
                    d.message.clone(),
                    d.path.clone(),
                    file(d.span),
                    file(d.first),
                )
            })
            .collect()
    }

    fn row(
        code: &str,
        message: &str,
        path: &str,
        at: &str,
        first: &str,
    ) -> (String, String, String, String, String) {
        (
            code.to_string(),
            message.to_string(),
            path.to_string(),
            at.to_string(),
            first.to_string(),
        )
    }

    #[test]
    fn agreeing_params_and_commands_merge_filling_what_one_leaves_out() {
        let (_, result) = merged(&[
            catalog(
                json!({ "eps.v": { "type": "f64", "unit": "V", "description": "Bus voltage" } }),
                json!({ "eps.go": { "arguments": [{ "name": "n", "type": "u64" }] } }),
            ),
            catalog(
                json!({ "eps.v": { "type": "f64", "unit": "V", "limits": { "red_high": 30.0 },
                                   "deprecated": "use eps.bus" },
                        "eps.i": { "type": "f64", "unit": "A" } }),
                json!({ "eps.go": { "arguments": [{ "name": "n", "type": "u64",
                                                    "description": "How many" }],
                                    "description": "Go", "deprecated": "use eps.run" } }),
            ),
        ]);
        let parsed = result.unwrap();
        assert!(parsed.warnings.is_empty(), "{:#?}", parsed.warnings);
        let bundle = parsed.bundle;
        let v = &bundle.catalog.params["eps.v"];
        assert_eq!(v.description.as_deref(), Some("Bus voltage"));
        assert_eq!(v.limits.and_then(|l| l.red_high), Some(30.0));
        assert_eq!(v.deprecated.as_deref(), Some("use eps.bus"));
        assert!(bundle.catalog.params.contains_key("eps.i"));
        let go = &bundle.catalog.commands["eps.go"];
        assert_eq!(go.description.as_deref(), Some("Go"));
        assert_eq!(go.deprecated.as_deref(), Some("use eps.run"));
        assert_eq!(go.arguments[0].description.as_deref(), Some("How many"));
    }

    #[test]
    fn values_both_give_differently_are_the_first_ones_with_a_warning_at_the_later() {
        let (sources, result) = merged(&[
            catalog(
                json!({ "eps.v": { "type": "f64", "description": "Main power bus voltage",
                                   "limits": { "red_high": 30.0 }, "deprecated": "old" } }),
                json!({ "eps.go": { "arguments": [{ "name": "n", "type": "u64",
                                                    "description": "Count" }],
                                    "description": "Go" } }),
            ),
            catalog(
                json!({ "eps.v": { "type": "f64", "description": "Main bus voltage",
                                   "limits": { "red_high": 31.0, "yellow_high": 28.0 },
                                   "deprecated": "older" } }),
                json!({ "eps.go": { "arguments": [{ "name": "n", "type": "u64",
                                                    "description": "Number" }],
                                    "description": "Run" } }),
            ),
        ]);
        let parsed = result.unwrap();
        let v = &parsed.bundle.catalog.params["eps.v"];
        assert_eq!(v.description.as_deref(), Some("Main power bus voltage"));
        assert_eq!(v.limits.and_then(|l| l.red_high), Some(30.0));
        assert_eq!(v.limits.and_then(|l| l.yellow_high), None);
        assert_eq!(v.deprecated.as_deref(), Some("old"));
        let go = &parsed.bundle.catalog.commands["eps.go"];
        assert_eq!(go.description.as_deref(), Some("Go"));
        assert_eq!(go.arguments[0].description.as_deref(), Some("Count"));
        let w = "overridden";
        assert_eq!(
            shown_all(&sources, &parsed.warnings),
            // In position order: the documents list commands before params.
            [
                row(
                    w,
                    "command eps.go has argument n's description \"Number\" here, but the earlier \
                     definition's \"Count\" takes precedence",
                    "/catalog/commands/eps.go/arguments/0/description",
                    "1.json",
                    "0.json"
                ),
                row(
                    w,
                    "command eps.go has description \"Run\" here, but the earlier definition's \"Go\" \
                     takes precedence",
                    "/catalog/commands/eps.go/description",
                    "1.json",
                    "0.json"
                ),
                row(
                    w,
                    "param eps.v has deprecation note \"older\" here, but the earlier definition's \
                     \"old\" takes precedence",
                    "/catalog/params/eps.v/deprecated",
                    "1.json",
                    "0.json"
                ),
                row(
                    w,
                    "param eps.v has description \"Main bus voltage\" here, but the earlier \
                     definition's \"Main power bus voltage\" takes precedence",
                    "/catalog/params/eps.v/description",
                    "1.json",
                    "0.json"
                ),
                row(
                    w,
                    "param eps.v has limits {\"yellow_high\":28.0,\"red_high\":31.0} here, but the \
                     earlier definition's {\"red_high\":30.0} takes precedence",
                    "/catalog/params/eps.v/limits",
                    "1.json",
                    "0.json"
                ),
            ]
        );
        // The replaced limits leave no spans behind.
        assert!(
            parsed
                .map
                .get("/catalog/params/eps.v/limits/yellow_high")
                .is_none()
        );
        let kept = parsed
            .map
            .get("/catalog/params/eps.v/limits/red_high")
            .unwrap();
        assert_eq!(sources.name(kept.source), Some("0.json"));
    }

    #[test]
    fn a_conflict_names_the_value_and_both_sides() {
        let first = catalog(
            json!({ "a": { "type": "f64" }, "b": { "type": "f64", "unit": "V" },
                    "c": { "type": "enum", "states": ["OFF", "ON"] },
                    "d": { "type": "vec3", "frame": "ECEF" } }),
            json!({ "go": { "arguments": [{ "name": "n", "type": "u64" }] },
                    "set": { "arguments": [{ "name": "level", "type": "u64", "default": 3 }] } }),
        );
        let second = catalog(
            json!({ "a": { "type": "i64" }, "b": { "type": "f64" },
                    "c": { "type": "enum", "states": ["ON", "OFF"] },
                    "d": { "type": "vec3", "frame": "ECI_J2000" } }),
            json!({ "go": { "arguments": [{ "name": "n", "type": "u64" },
                                          { "name": "delay", "type": "u64" }] },
                    "set": { "arguments": [{ "name": "level", "type": "u64", "default": 5 }] } }),
        );
        let (sources, result) = merged(&[first, second]);
        let e = "duplicate_name";
        assert_eq!(
            shown_all(&sources, &result.unwrap_err()),
            [
                row(
                    e,
                    "command go is defined again with arguments n, delay; the earlier definition has n",
                    "/catalog/commands/go/arguments",
                    "1.json",
                    "0.json"
                ),
                row(
                    e,
                    "command set is defined again with argument level's default 5; the earlier \
                        definition has 3",
                    "/catalog/commands/set/arguments/0/default",
                    "1.json",
                    "0.json"
                ),
                row(
                    e,
                    "param a is defined again with type i64; the earlier definition has f64",
                    "/catalog/params/a/type",
                    "1.json",
                    "0.json"
                ),
                row(
                    e,
                    "param b is defined again with unit none; the earlier definition has V",
                    "/catalog/params/b/unit",
                    "1.json",
                    "0.json"
                ),
                row(
                    e,
                    "param c is defined again with states ON, OFF; the earlier definition has OFF, ON",
                    "/catalog/params/c/states",
                    "1.json",
                    "0.json"
                ),
                row(
                    e,
                    "param d is defined again with frame ECI_J2000; the earlier definition has ECEF",
                    "/catalog/params/d/frame",
                    "1.json",
                    "0.json"
                ),
            ]
        );
    }

    #[test]
    fn a_third_definition_is_compared_with_where_each_kept_value_came_from() {
        let (sources, result) = merged(&[
            catalog(
                json!({ "eps.v": { "type": "f64", "unit": "V" } }),
                json!({}),
            ),
            catalog(
                json!({ "eps.v": { "type": "f64", "unit": "V", "limits": { "red_high": 30.0 } } }),
                json!({}),
            ),
            catalog(
                json!({ "eps.v": { "type": "f64", "unit": "V", "limits": { "red_high": 31.0 } } }),
                json!({}),
            ),
            catalog(
                json!({ "eps.v": { "type": "f64", "unit": "mV" } }),
                json!({}),
            ),
        ]);
        assert_eq!(
            shown_all(&sources, &result.unwrap_err()),
            [
                row(
                    "overridden",
                    "param eps.v has limits {\"red_high\":31.0} here, but the earlier definition's \
                     {\"red_high\":30.0} takes precedence",
                    "/catalog/params/eps.v/limits",
                    "2.json",
                    "1.json"
                ),
                row(
                    "duplicate_name",
                    "param eps.v is defined again with unit mV; the earlier definition has V",
                    "/catalog/params/eps.v/unit",
                    "3.json",
                    "0.json"
                ),
            ]
        );
    }

    #[test]
    fn a_filled_value_is_located_in_the_part_that_gave_it() {
        let (sources, result) = merged(&[
            catalog(json!({ "eps.v": { "type": "f64" } }), json!({})),
            catalog(
                json!({ "eps.v": { "type": "f64",
                                   "limits": { "red_low": 10.0, "red_high": 5.0 } } }),
                json!({}),
            ),
        ]);
        let parsed = result.unwrap();
        let diagnostics = validate(parsed.bundle).unwrap_err();
        let located = parsed.map.locate_all(&diagnostics);
        assert_eq!(located.len(), 1, "{located:#?}");
        assert_eq!(located[0].code, "limits_order");
        let span = located[0].span.unwrap();
        assert_eq!(sources.name(span.source), Some("1.json"));
    }

    #[test]
    fn a_later_bundles_sets_are_located_in_its_own_file() {
        let family = json!({ "name": "f", "encoding": "binary", "byte_order": "big",
                             "alias": { "kind": "fixed", "value": "X" } });
        let set = |name: &str, canonical: &str| {
            json!({ "name": name, "family": "f", "packets": [
                { "name": "P", "entries": [
                    { "kind": "field", "name": "a", "type": "uint", "size_bits": 8,
                      "canonical": canonical }
                ] }
            ] })
        };
        let first = json!({ "mdl_version": "2.0",
                            "catalog": { "params": { "eps.a": { "type": "u64" } } },
                            "families": [family], "sets": [set("s1", "eps.a")] });
        let second = json!({ "mdl_version": "2.0", "catalog": {},
                             "sets": [set("s2", "eps.nope")] });
        let (sources, result) = merged(&[first, second]);
        let parsed = result.unwrap();
        assert_eq!(parsed.bundle.sets.len(), 2);
        let diagnostics = validate(parsed.bundle).unwrap_err();
        let located = parsed.map.locate_all(&diagnostics);
        assert_eq!(located.len(), 1, "{located:#?}");
        let span = located[0].span.unwrap();
        assert_eq!(sources.name(span.source), Some("1.json"));
        assert_eq!(located[0].path, "/sets/1/packets/0/entries/0/canonical");
    }

    fn with_set(params: Value) -> Value {
        json!({ "mdl_version": "2.0", "catalog": { "params": params },
                "sets": [{ "name": "s", "family": "f", "packets": [] }] })
    }

    #[test]
    fn a_list_or_map_from_several_parts_has_no_span() {
        let (sources, result) = merged(&[
            catalog(json!({ "a": { "type": "f64" } }), json!({})),
            with_set(json!({ "b": { "type": "f64" } })),
            catalog(json!({ "c": { "type": "f64" } }), json!({})),
        ]);
        let map = result.unwrap().map;
        for pointer in ["", "/catalog", "/catalog/params"] {
            assert_eq!(map.get(pointer), None, "{pointer}");
        }
        // Only the second part gives sets.
        let sets = map.get("/sets").unwrap();
        assert_eq!(sources.name(sets.source), Some("1.json"));
        let c = map.get("/catalog/params/c").unwrap();
        assert_eq!(sources.name(c.source), Some("2.json"));

        // Sets from two parts, the first without a span for its list (as
        // a frontend gives a list assembled from several files).
        let (_, mut parts) = read(&[
            with_set(json!({ "a": { "type": "f64" } })),
            with_set(json!({ "b": { "type": "f64" } })),
        ]);
        parts[0].map.spans.remove("/sets");
        assert_eq!(merge(parts).unwrap().map.get("/sets"), None);
    }

    #[test]
    fn warnings_are_kept_in_file_order_on_success_and_on_failure() {
        let with_warnings = |second: Value| {
            let (sources, mut parts) = read(&[
                catalog(json!({ "a": { "type": "f64" } }), json!({})),
                second,
            ]);
            let warning = |part: &Parsed, pointer: &str| {
                SourceDiagnostic::warning("test.left_out", format!("at {pointer}"))
                    .with_span(part.map.find(pointer).unwrap())
            };
            let late = warning(&parts[1], "/catalog/params/a");
            let early = warning(&parts[0], "/mdl_version");
            parts[1].warnings.push(late);
            parts[0].warnings.push(early);
            (sources, merge(parts))
        };
        let codes = |sources: &Sources, diagnostics: &[SourceDiagnostic]| {
            shown_all(sources, diagnostics)
                .into_iter()
                .map(|(code, _, _, at, _)| format!("{code} {at}"))
                .collect::<Vec<_>>()
        };
        let (sources, result) =
            with_warnings(catalog(json!({ "a": { "type": "i64" } }), json!({})));
        assert_eq!(
            codes(&sources, &result.unwrap_err()),
            [
                "test.left_out 0.json",
                "test.left_out 1.json",
                "duplicate_name 1.json"
            ]
        );
        let (sources, result) =
            with_warnings(catalog(json!({ "a": { "type": "f64" } }), json!({})));
        assert_eq!(
            codes(&sources, &result.unwrap().warnings),
            ["test.left_out 0.json", "test.left_out 1.json"]
        );
    }

    #[test]
    fn the_merged_version_is_the_newest() {
        let documents = [catalog(json!({}), json!({})), catalog(json!({}), json!({}))];
        let (_, mut parts) = read(&documents);
        parts[1].bundle.mdl_version = IrVersion::new(2, 3);
        assert_eq!(
            merge(parts.clone()).unwrap().bundle.mdl_version,
            IrVersion::new(2, 3)
        );
        parts.reverse();
        assert_eq!(
            merge(parts).unwrap().bundle.mdl_version,
            IrVersion::new(2, 3)
        );
    }

    #[test]
    fn list_items_move_past_earlier_parts_and_nothing_merges_to_an_error() {
        assert_eq!(
            moved("/sets/3/packets/1", "/sets/", 2).as_deref(),
            Some("/sets/5/packets/1")
        );
        assert_eq!(moved("/sets/0", "/sets/", 1).as_deref(), Some("/sets/1"));
        assert_eq!(
            moved("/sets/10/x", "/sets/", 5).as_deref(),
            Some("/sets/15/x")
        );
        assert_eq!(
            moved("/families/2", "/families/", 1).as_deref(),
            Some("/families/3")
        );
        for unmoved in [
            "/families/0",
            "/sets",
            "/sets/",
            "/setsx/1",
            "/sets/1x",
            "/sets/01",
            "/sets/+1",
        ] {
            assert_eq!(moved(unmoved, "/sets/", 1), None, "{unmoved}");
        }
        assert!(within("/a/limits", "/a/limits"));
        assert!(within("/a/limits/red_high", "/a/limits"));
        assert!(!within("/a/limits_x", "/a/limits"));
        let errors = merge(Vec::new()).unwrap_err();
        assert_eq!(errors[0].code, "empty");
    }
}
