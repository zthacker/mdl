//! Comparisons: a BaseContainer's restriction criteria as identify keys,
//! and an entry's IncludeCondition as its condition.
//!
//! XTCE writes a comparison two ways: a `Comparison`'s attributes, or a
//! boolean expression's `Condition`, whose parameter, operator and value
//! are child elements. Both are read into one comparison.

use std::borrow::Cow;

use mdl::source::Span;
use mdl::validate::MAX_NESTING;

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

/// A comparison as XTCE writes it.
#[derive(Clone, Copy)]
enum Written<'d> {
    /// A `Comparison`: parameter, operator and value as attributes.
    Attributes(El<'d>),
    /// A `Condition`: a ParameterInstanceRef, a ComparisonOperator and a
    /// Value as children.
    Elements(El<'d>),
}

impl<'d> Written<'d> {
    fn el(self) -> El<'d> {
        match self {
            Written::Attributes(el) | Written::Elements(el) => el,
        }
    }
}

/// What a comparison says, however it's written.
struct Parts<'d> {
    reference: (&'d str, Span),
    op: Option<(Cow<'d, str>, Span)>,
    value: (Cow<'d, str>, Span),
    calibrated: bool,
}

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
        let written = self.identifying(criteria)?;
        let mut keys = Vec::new();
        for comparison in written {
            let el = comparison.el();
            let comparison = self.comparison(space, comparison, above, Use::Identify)?;
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

    /// The comparisons restriction criteria require, all of which must
    /// hold: a Comparison, a ComparisonList, or a boolean expression that
    /// ANDs its conditions.
    fn identifying(&mut self, criteria: El<'d>) -> Option<Vec<Written<'d>>> {
        let criterion = self.criterion(criteria, "identification")?;
        match criterion.name() {
            "Comparison" => Some(vec![Written::Attributes(criterion)]),
            "ComparisonList" => Some(
                self.comparison_list(criterion)?
                    .into_iter()
                    .map(Written::Attributes)
                    .collect(),
            ),
            _ => {
                let mut conditions = Vec::new();
                self.anded(criterion, 1, &mut conditions)?;
                Some(conditions)
            }
        }
    }

    /// The conditions an expression requires all of, for identification,
    /// which can't take an OR of two or more. An OR of one condition is
    /// that condition.
    fn anded(&mut self, el: El<'d>, depth: usize, out: &mut Vec<Written<'d>>) -> Option<()> {
        let members = self.members(el, depth)?;
        if el.name() == "ORedConditions" && members.len() > 1 {
            self.report.error(
                Code::Unsupported,
                el.span(),
                "identification by an OR isn't supported: MDL identifies a packet by values that \
                 must all match",
            );
            return None;
        }
        for member in members {
            match member.name() {
                "Condition" => out.push(Written::Elements(member)),
                _ => self.anded(member, depth + 1, out)?,
            }
        }
        Some(())
    }

    /// What a BooleanExpression, ANDedConditions or ORedConditions holds:
    /// one member of a BooleanExpression, at least one of the others.
    fn members(&mut self, el: El<'d>, depth: usize) -> Option<Vec<El<'d>>> {
        if depth > MAX_NESTING {
            self.report.error(
                Code::Unsupported,
                el.span(),
                format!(
                    "conditions nested more than {MAX_NESTING} deep aren't supported: MDL bounds \
                     condition nesting there"
                ),
            );
            return None;
        }
        el.check(&EXPRESSION, self.report);
        let members: Vec<El<'d>> = el
            .children()
            .filter(|c| EXPRESSION.children.contains(&c.name()))
            .collect();
        if el.name() == "BooleanExpression"
            && let Some(second) = members.get(1)
        {
            self.report.error(
                Code::Invalid,
                second.span(),
                "a BooleanExpression holds one Condition, ANDedConditions or ORedConditions",
            );
            return None;
        }
        if members.is_empty() {
            self.report.error(
                Code::Invalid,
                el.span(),
                format!("{} holds conditions", el.name()),
            );
            return None;
        }
        Some(members)
    }

    fn comparison_list(&mut self, list: El<'d>) -> Option<Vec<El<'d>>> {
        list.check(
            &Expect {
                attrs: &[],
                children: &["Comparison"],
                ignored: &[],
                text: false,
            },
            self.report,
        );
        let comparisons: Vec<El<'d>> = list.all("Comparison").collect();
        if comparisons.is_empty() {
            self.report.error(
                Code::Invalid,
                list.span(),
                "a ComparisonList holds comparisons",
            );
            return None;
        }
        Some(comparisons)
    }

    /// The one Comparison, ComparisonList, BooleanExpression or algorithm
    /// criteria hold, refusing an algorithm.
    fn criterion(&mut self, criteria: El<'d>, what: &str) -> Option<El<'d>> {
        let mut found = criteria.children().filter(|c| CRITERIA.contains(&c.name()));
        let first = found.next();
        if let Some(second) = found.next() {
            self.report.error(
                Code::Invalid,
                second.span(),
                format!(
                    "{} holds one Comparison, ComparisonList or BooleanExpression",
                    criteria.name()
                ),
            );
            return None;
        }
        match first {
            Some(algorithm) if algorithm.name() == "CustomAlgorithm" => {
                self.report.error(
                    Code::Unsupported,
                    algorithm.span(),
                    format!("{what} by an algorithm isn't supported"),
                );
                None
            }
            Some(criterion) => Some(criterion),
            None => {
                self.report.error(
                    Code::Invalid,
                    criteria.span(),
                    format!("{} holds a comparison", criteria.name()),
                );
                None
            }
        }
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
        let criterion = self.criterion(el, "inclusion")?;
        match criterion.name() {
            "Comparison" => {
                let comparison =
                    self.comparison(space, Written::Attributes(criterion), known, Use::Condition)?;
                Some(compare_object(&comparison))
            }
            "ComparisonList" => {
                let mut objects = Vec::new();
                for comparison in self.comparison_list(criterion)? {
                    let comparison = self.comparison(
                        space,
                        Written::Attributes(comparison),
                        known,
                        Use::Condition,
                    )?;
                    objects.push(compare_object(&comparison));
                }
                if objects.len() == 1 {
                    return objects.pop();
                }
                Some(group("all", objects, criterion.span()))
            }
            _ => self.expression(space, criterion, known, 1),
        }
    }

    /// A BooleanExpression, ANDedConditions, ORedConditions or Condition
    /// as an MDL condition: `all`, `any` or a comparison.
    fn expression(
        &mut self,
        space: usize,
        el: El<'d>,
        known: &[(ParamKey<'d>, Known)],
        depth: usize,
    ) -> Option<Object> {
        if el.name() == "Condition" {
            let comparison =
                self.comparison(space, Written::Elements(el), known, Use::Condition)?;
            return Some(compare_object(&comparison));
        }
        let mut objects = Vec::new();
        for member in self.members(el, depth)? {
            objects.push(self.expression(space, member, known, depth + 1)?);
        }
        match el.name() {
            "ANDedConditions" => Some(group("all", objects, el.span())),
            "ORedConditions" => Some(group("any", objects, el.span())),
            _ => objects.pop(),
        }
    }

    /// A comparison's parameter, operator and value, however written.
    fn parts(&mut self, written: Written<'d>) -> Option<Parts<'d>> {
        match written {
            Written::Attributes(el) => {
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
                instance_zero(el, self.report)?;
                Some(Parts {
                    reference: (reference.value, reference.span),
                    op: el
                        .attr("comparisonOperator")
                        .map(|a| (Cow::Borrowed(a.value), a.span)),
                    value: (Cow::Borrowed(value.value), value.span),
                    calibrated: calibrated(el, self.report)?,
                })
            }
            Written::Elements(el) => {
                el.check(
                    &Expect {
                        attrs: &[],
                        children: &["ParameterInstanceRef", "ComparisonOperator", "Value"],
                        ignored: &[],
                        text: false,
                    },
                    self.report,
                );
                let references: Vec<El<'d>> = el.all("ParameterInstanceRef").collect();
                let (Some(left), Some(op)) = (references.first(), el.child("ComparisonOperator"))
                else {
                    self.report.error(
                        Code::Invalid,
                        el.span(),
                        "a Condition has a ParameterInstanceRef and a ComparisonOperator",
                    );
                    return None;
                };
                if let Some(right) = references.get(1) {
                    self.report.error(
                        Code::Unsupported,
                        right.span(),
                        "a comparison of two parameters isn't supported: MDL compares a field \
                         with a value",
                    );
                    return None;
                }
                let Some(value) = el.child("Value") else {
                    self.report
                        .error(Code::Invalid, el.span(), "a Condition has a Value");
                    return None;
                };
                left.check(
                    &Expect {
                        attrs: &["parameterRef", "useCalibratedValue", "instance"],
                        children: &[],
                        ignored: &[],
                        text: false,
                    },
                    self.report,
                );
                let reference = left.required("parameterRef", self.report)?;
                instance_zero(*left, self.report)?;
                Some(Parts {
                    reference: (reference.value, reference.span),
                    op: Some(op.text()),
                    value: value.text(),
                    calibrated: calibrated(*left, self.report)?,
                })
            }
        }
    }

    fn comparison(
        &mut self,
        space: usize,
        written: Written<'d>,
        known: &[(ParamKey<'d>, Known)],
        purpose: Use,
    ) -> Option<Comparison<'d>> {
        let el = written.el();
        let parts = self.parts(written)?;
        let op = match &parts.op {
            None => "eq",
            Some((text, span)) => match text.trim() {
                "==" => "eq",
                "!=" => "ne",
                "<" => "lt",
                "<=" => "le",
                ">" => "gt",
                ">=" => "ge",
                other => {
                    self.report.error(
                        Code::Invalid,
                        *span,
                        format!("{other:?} isn't a comparison operator"),
                    );
                    return None;
                }
            },
        };
        let (reference, reference_span) = parts.reference;
        let found = match self.tree.resolve(space, reference, Kind::Parameter) {
            Ok(found) => found,
            Err(message) => {
                self.report.error(Code::Reference, reference_span, message);
                return None;
            }
        };
        let name = found.el.item_name()?;
        self.seen.insert((found.space, name));
        let key = (found.space, name, found.members);
        let Some((_, field)) = known.iter().find(|(k, _)| *k == key) else {
            let message = match purpose {
                Use::Identify => format!(
                    "{reference} isn't a field of the containers this one inherits from, and MDL \
                     identifies a packet by its own fields"
                ),
                Use::Condition => format!(
                    "{reference} isn't a field decoded earlier in the packet, and MDL conditions \
                     compare only those"
                ),
            };
            self.report
                .error(Code::Unsupported, reference_span, message);
            return None;
        };
        let labelled = matches!(field.raw, Raw::Labels(_) | Raw::Boolean { .. });
        if parts.calibrated && !matches!(op, "eq" | "ne") && labelled {
            self.report.error(
                Code::Unsupported,
                el.span(),
                "ordering comparisons of labelled values aren't supported",
            );
            return None;
        }
        let (value, value_span) = &parts.value;
        let raw = match raw_value(field.ty, &field.raw, value.trim(), parts.calibrated) {
            Ok(raw) => raw,
            Err(reason) => {
                self.report.error(
                    Code::Unsupported,
                    *value_span,
                    format!("this comparison of {reference} can't be imported: {reason}"),
                );
                return None;
            }
        };
        Some(Comparison {
            el,
            field: (field.name.clone(), reference_span),
            op,
            value: (raw, *value_span),
        })
    }
}

#[derive(Clone, Copy)]
enum Use {
    Identify,
    Condition,
}

/// What a boolean expression and its ANDs and ORs hold.
const EXPRESSION: Expect<'static> = Expect {
    attrs: &[],
    children: &["Condition", "ANDedConditions", "ORedConditions"],
    ignored: &[],
    text: false,
};

/// Refuses an instance other than the current one.
fn instance_zero(el: El<'_>, report: &mut Report) -> Option<()> {
    if let Some(instance) = el.attr("instance")
        && xml::long(instance, report)? != 0
    {
        report.error(
            Code::Unsupported,
            instance.span,
            "a comparison with an earlier instance of a parameter isn't supported",
        );
        return None;
    }
    Some(())
}

/// `useCalibratedValue`, true by default.
fn calibrated(el: El<'_>, report: &mut Report) -> Option<bool> {
    match el.attr("useCalibratedValue") {
        Some(attr) => xml::boolean(attr, report),
        None => Some(true),
    }
}

/// A resolved comparison as an MDL condition. A wide boolean is true for
/// any value but 0; a label on several values is any of them.
fn compare_object(comparison: &Comparison<'_>) -> Object {
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
    match (&comparison.value.0, comparison.op) {
        (Wanted::One(value), op) => compare(op, *value),
        (Wanted::NonZero, "eq") => compare("ne", 0),
        (Wanted::NonZero, _) => compare("eq", 0),
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
    }
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
