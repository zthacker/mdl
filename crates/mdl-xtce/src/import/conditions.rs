//! Comparisons: a BaseContainer's restriction criteria as identify keys,
//! and an entry's IncludeCondition as its condition.

use mdl::v2::source::Span;

use super::{Import, Known, ParamKey};
use crate::Code;
use crate::out::{List, Object};
use crate::report::Report;
use crate::space::Kind;
use crate::types::{Raw, Wanted, raw_value};
use crate::xml::{self, El, Expect};

const CRITERIA: &[&str] = &[
    "Comparison",
    "ComparisonList",
    "BooleanExpression",
    "CustomAlgorithm",
];

/// One comparison, resolved.
struct Comparison<'d> {
    el: El<'d>,
    field: (String, Span),
    op: &'static str,
    value: (Wanted, Span),
}

impl<'d> Import<'d, '_> {
    /// A BaseContainer's restriction criteria as identify keys.
    pub(super) fn restriction(
        &mut self,
        space: usize,
        base: El<'d>,
        above: &[(ParamKey<'d>, Known)],
    ) -> Option<Vec<(String, i64, Span)>> {
        base.check(
            &Expect {
                attrs: &["containerRef"],
                children: &["RestrictionCriteria"],
                ignored: &[],
                text: false,
            },
            self.report,
        );
        let Some(criteria) = base.child("RestrictionCriteria") else {
            return Some(Vec::new());
        };
        let mut children = CRITERIA.to_vec();
        children.push("NextContainer");
        criteria.check(
            &Expect {
                attrs: &[],
                children: &children,
                ignored: &[],
                text: false,
            },
            self.report,
        );
        if let Some(next) = criteria.child("NextContainer") {
            self.report.error(
                Code::Unsupported,
                next.span(),
                "identification by the next container isn't supported",
            );
            return None;
        }
        let comparisons = self.comparisons(criteria, "identification")?;
        let mut keys = Vec::new();
        for el in comparisons {
            let comparison = self.comparison(space, el, above, Use::Identify)?;
            let field = &comparison.field.0;
            let problem = match (comparison.op, &comparison.value.0) {
                ("eq", Wanted::One(value)) => {
                    keys.push((field.clone(), *value, el.span()));
                    continue;
                }
                ("eq", Wanted::Any(_)) => format!(
                    "this label of {field} stands for several values, and MDL identifies a packet \
                     by one value of each key"
                ),
                ("eq", Wanted::NonZero) => format!(
                    "{field} is a boolean wider than a bit, true for any value but 0, and MDL \
                     identifies a packet by one value of each key"
                ),
                _ => "MDL identifies packets by equal values; this comparison isn't one".into(),
            };
            self.report.error(Code::Unsupported, el.span(), problem);
            return None;
        }
        Some(keys)
    }

    /// The comparisons of a match criteria that holds a Comparison or a
    /// ComparisonList; anything else is reported.
    fn comparisons(&mut self, criteria: El<'d>, what: &str) -> Option<Vec<El<'d>>> {
        if let Some(comparison) = criteria.child("Comparison") {
            return Some(vec![comparison]);
        }
        if let Some(list) = criteria.child("ComparisonList") {
            list.check(
                &Expect {
                    attrs: &[],
                    children: &["Comparison"],
                    ignored: &[],
                    text: false,
                },
                self.report,
            );
            return Some(list.all("Comparison").collect());
        }
        if let Some(expression) = criteria.child("BooleanExpression") {
            self.report.error(
                Code::Unsupported,
                expression.span(),
                format!("{what} by a boolean expression isn't supported yet"),
            );
            return None;
        }
        if let Some(algorithm) = criteria.child("CustomAlgorithm") {
            self.report.error(
                Code::Unsupported,
                algorithm.span(),
                format!("{what} by an algorithm isn't supported"),
            );
            return None;
        }
        self.report.error(
            Code::Invalid,
            criteria.span(),
            format!("{} holds a comparison", criteria.name()),
        );
        None
    }

    fn comparison(
        &mut self,
        space: usize,
        el: El<'d>,
        known: &[(ParamKey<'d>, Known)],
        purpose: Use,
    ) -> Option<Comparison<'d>> {
        el.check(
            &Expect {
                attrs: &[
                    "parameterRef",
                    "value",
                    "comparisonOperator",
                    "useCalibratedValue",
                    "instance",
                ],
                children: &[],
                ignored: &[],
                text: false,
            },
            self.report,
        );
        let reference = el.required("parameterRef", self.report)?;
        let value = el.required("value", self.report)?;
        if let Some(instance) = el.attr("instance")
            && xml::long(instance, self.report)? != 0
        {
            self.report.error(
                Code::Unsupported,
                instance.span,
                "a comparison with an earlier instance of a parameter isn't supported",
            );
            return None;
        }
        let op = match el.attr("comparisonOperator") {
            None => "eq",
            Some(attr) => match attr.value.trim() {
                "==" => "eq",
                "!=" => "ne",
                "<" => "lt",
                "<=" => "le",
                ">" => "gt",
                ">=" => "ge",
                other => {
                    self.report.error(
                        Code::Invalid,
                        attr.span,
                        format!("{other:?} isn't a comparison operator"),
                    );
                    return None;
                }
            },
        };
        let calibrated = match el.attr("useCalibratedValue") {
            Some(attr) => xml::boolean(attr, self.report)?,
            None => true,
        };
        let found = match self.tree.resolve(space, reference.value, Kind::Parameter) {
            Ok(found) => found,
            Err(message) => {
                self.report.error(Code::Reference, reference.span, message);
                return None;
            }
        };
        let name = found.el.item_name()?;
        self.seen.insert((found.space, name));
        let key = (found.space, name, found.members);
        let Some((_, field)) = known.iter().find(|(k, _)| *k == key) else {
            let message = match purpose {
                Use::Identify => format!(
                    "{} isn't a field of the containers this one inherits from, and MDL \
                     identifies a packet by its own fields",
                    reference.value
                ),
                Use::Condition => format!(
                    "{} isn't a field decoded earlier in the packet, and MDL conditions compare \
                     only those",
                    reference.value
                ),
            };
            self.report
                .error(Code::Unsupported, reference.span, message);
            return None;
        };
        let labelled = matches!(field.raw, Raw::Labels(_) | Raw::Boolean { .. });
        if calibrated && !matches!(op, "eq" | "ne") && labelled {
            self.report.error(
                Code::Unsupported,
                el.span(),
                "ordering comparisons of labelled values aren't supported",
            );
            return None;
        }
        let raw = match raw_value(field.ty, &field.raw, value.value, calibrated) {
            Ok(raw) => raw,
            Err(reason) => {
                self.report.error(
                    Code::Unsupported,
                    value.span,
                    format!(
                        "this comparison of {} can't be imported: {reason}",
                        reference.value
                    ),
                );
                return None;
            }
        };
        Some(Comparison {
            el,
            field: (field.name.clone(), reference.span),
            op,
            value: (raw, value.span),
        })
    }

    /// An IncludeCondition as an MDL condition.
    pub(super) fn condition(
        &mut self,
        space: usize,
        el: El<'d>,
        known: &[(ParamKey<'d>, Known)],
    ) -> Option<Object> {
        el.check(
            &Expect {
                attrs: &[],
                children: CRITERIA,
                ignored: &[],
                text: false,
            },
            self.report,
        );
        let comparisons = self.comparisons(el, "inclusion")?;
        let mut objects = Vec::new();
        for comparison in comparisons {
            let comparison = self.comparison(space, comparison, known, Use::Condition)?;
            let at = comparison.el.span();
            let compare = |op: &str, value: i64| {
                let mut object = Object::at(at);
                object.put("kind", "compare", None);
                object.put(
                    "field",
                    comparison.field.0.as_str(),
                    Some(comparison.field.1),
                );
                object.put("op", op, None);
                object.put("value", value, Some(comparison.value.1));
                object
            };
            objects.push(match (&comparison.value.0, comparison.op) {
                (Wanted::One(value), op) => compare(op, *value),
                // A wide boolean is true for any value but 0.
                (Wanted::NonZero, "eq") => compare("ne", 0),
                (Wanted::NonZero, _) => compare("eq", 0),
                // A label on several values: one of them, or none.
                (Wanted::Any(values), "eq") => group(
                    "any",
                    values.iter().map(|v| compare("eq", *v)).collect(),
                    at,
                ),
                (Wanted::Any(values), _) => group(
                    "all",
                    values.iter().map(|v| compare("ne", *v)).collect(),
                    at,
                ),
            });
        }
        if objects.len() == 1 {
            return objects.pop();
        }
        Some(group("all", objects, el.span()))
    }
}

#[derive(Clone, Copy)]
enum Use {
    Identify,
    Condition,
}

/// Refuses a TimeAssociation: it times a value by another parameter, and
/// an MDL sample takes its packet's time.
pub(super) fn time_association(el: El<'_>, report: &mut Report) -> Option<()> {
    match el.child("TimeAssociation") {
        Some(association) => {
            report.error(
                Code::Unsupported,
                association.span(),
                "a time association isn't supported: it times the value by another parameter, \
                 and an MDL sample takes its packet's time",
            );
            None
        }
        None => Some(()),
    }
}

/// An `all` or `any` condition of these.
fn group(kind: &str, conditions: Vec<Object>, span: Span) -> Object {
    let mut object = Object::at(span);
    object.put("kind", kind, None);
    let mut list = List::default();
    for condition in conditions {
        list.push_object(condition);
    }
    object.put_list("conditions", list);
    object
}
