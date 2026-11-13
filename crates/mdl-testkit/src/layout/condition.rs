//! Conditions (`spec/decoding.md` §5.4): a test on a selector field's raw
//! value, which makes an entry present or absent. The binary and JSON
//! oracles both lay them out, as packets of either encoding have them.

use std::ops::RangeInclusive;

use proptest::collection::{btree_set, vec};
use proptest::prelude::*;
use proptest::sample::select;
use serde_json::{Value, json};

use super::{FieldSpec, Kind, Length, Raw, Text, integer_json, spec_of};

/// A condition on a selector field, before it's laid out.
#[derive(Debug, Clone)]
pub(super) struct WhenSpec {
    pub(super) selector: Selector,
    /// A new selector field: any meaning a field of its kind can have, so
    /// a condition compares its raw value, not what it converts to (§5.4).
    pub(super) field: FieldSpec,
    /// Whether it tests the container's last selector, when that's of its
    /// kind.
    pub(super) share: bool,
    pub(super) test: Test,
}

/// A selector field's kind: what conditions can compare (§5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Selector {
    Uint(u32),
    Int(u32),
    Bool,
    /// One ASCII letter.
    Text,
}

impl Selector {
    pub(super) fn kind(self) -> Kind {
        match self {
            Selector::Uint(bits) => Kind::Uint(bits),
            Selector::Int(bits) => Kind::Int(bits),
            Selector::Bool => Kind::Bool,
            Selector::Text => Kind::Text {
                length: Length::Fixed {
                    bytes: 1,
                    terminator: None,
                },
                text: Text::Ascii,
            },
        }
    }
}

/// A condition on a selector of any kind, new or the container's last.
pub(super) fn when_spec() -> BoxedStrategy<WhenSpec> {
    let selector = prop_oneof![
        3 => (2_u32..=6).prop_map(Selector::Uint),
        2 => (2_u32..=6).prop_map(Selector::Int),
        1 => Just(Selector::Bool),
        1 => Just(Selector::Text),
    ];
    (selector, proptest::bool::weighted(0.4))
        .prop_flat_map(|(selector, share)| {
            (spec_of(Just(selector.kind()).boxed()), test_for(selector)).prop_map(
                move |(field, test)| WhenSpec {
                    selector,
                    field,
                    share,
                    test,
                },
            )
        })
        .boxed()
}

/// A test a selector of this kind can be put to: a comparison, mostly,
/// else `not` one, or `all` or `any` of two.
fn test_for(selector: Selector) -> BoxedStrategy<Test> {
    let leaf = leaf_for(selector);
    prop_oneof![
        4 => leaf.clone(),
        1 => leaf.clone().prop_map(|test| Test::Not(Box::new(test))),
        1 => vec(leaf.clone(), 2).prop_map(Test::All),
        1 => vec(leaf, 2).prop_map(Test::Any),
    ]
    .boxed()
}

/// A comparison a selector of this kind takes (validation's rules): any
/// on an unsigned integer, no mask on a signed one, equality on a bool or
/// text; or `in` with values it holds.
fn leaf_for(selector: Selector) -> BoxedStrategy<Test> {
    let numbers = |values: RangeInclusive<i128>, ops: &'static [Op]| {
        prop_oneof![
            3 => (select(ops), values.clone())
                .prop_map(|(op, n)| Test::Compare(op, Operand::Int(n))),
            1 => btree_set(values, 1..=3)
                .prop_map(|set| Test::In(set.into_iter().map(Operand::Int).collect())),
        ]
        .boxed()
    };
    let all = &[
        Op::Eq,
        Op::Ne,
        Op::Lt,
        Op::Le,
        Op::Gt,
        Op::Ge,
        Op::MaskAny,
        Op::MaskAll,
    ];
    match selector {
        Selector::Uint(bits) => numbers(0..=(1 << bits) - 1, all),
        Selector::Int(bits) => numbers(-(1 << (bits - 1))..=(1 << (bits - 1)) - 1, &all[..6]),
        Selector::Bool => numbers(0..=1, &all[..2]),
        Selector::Text => {
            let letter = select(&["a", "b", "c", "d"][..]).prop_map(|t| Operand::Text(t.into()));
            prop_oneof![
                3 => (select(&all[..2]), letter.clone())
                    .prop_map(|(op, text)| Test::Compare(op, text)),
                1 => vec(letter, 1..=2).prop_map(Test::In),
            ]
            .boxed()
        }
    }
}

/// A condition (§5.4) on a selector field of the entry's container: every
/// comparison in it is on that field's raw value.
#[derive(Debug, Clone)]
pub(super) struct Condition {
    pub(super) field: String,
    pub(super) test: Test,
}

impl Condition {
    /// Whether it holds for its container's values.
    pub(super) fn holds(&self, record: &[(String, Raw)]) -> bool {
        record
            .iter()
            .find(|(name, _)| *name == self.field)
            .is_some_and(|(_, raw)| self.test.holds(raw))
    }
}

/// What a condition asks of its field's raw value.
#[derive(Debug, Clone)]
pub(super) enum Test {
    Compare(Op, Operand),
    In(Vec<Operand>),
    Not(Box<Test>),
    All(Vec<Test>),
    Any(Vec<Test>),
}

impl Test {
    fn holds(&self, raw: &Raw) -> bool {
        match self {
            Test::Compare(op, operand) => compare(raw, *op, operand),
            Test::In(operands) => operands.iter().any(|operand| compare(raw, Op::Eq, operand)),
            Test::Not(test) => !test.holds(raw),
            Test::All(tests) => tests.iter().all(|test| test.holds(raw)),
            Test::Any(tests) => tests.iter().any(|test| test.holds(raw)),
        }
    }

    /// Every operand it compares with, for a selector's values.
    pub(super) fn operands(&self, out: &mut Vec<(Op, Operand)>) {
        match self {
            Test::Compare(op, operand) => out.push((*op, operand.clone())),
            Test::In(operands) => out.extend(operands.iter().map(|o| (Op::Eq, o.clone()))),
            Test::Not(test) => test.operands(out),
            Test::All(tests) | Test::Any(tests) => {
                for test in tests {
                    test.operands(out);
                }
            }
        }
    }
}

/// A comparison (§5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// `field & value != 0`
    MaskAny,
    /// `field & value == value`
    MaskAll,
}

impl Op {
    /// Its name, as the IR spells it.
    fn name(self) -> &'static str {
        match self {
            Op::Eq => "eq",
            Op::Ne => "ne",
            Op::Lt => "lt",
            Op::Le => "le",
            Op::Gt => "gt",
            Op::Ge => "ge",
            Op::MaskAny => "mask_any",
            Op::MaskAll => "mask_all",
        }
    }
}

/// What a condition compares a field with: an integer (a bool is 0 or 1),
/// or text, exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Operand {
    Int(i128),
    Text(String),
}

/// A comparison of a raw value with an operand (§5.4): by value, a bool as
/// 0 or 1; text only for equality.
fn compare(raw: &Raw, op: Op, operand: &Operand) -> bool {
    let value = match (raw, operand) {
        (Raw::Str(text), Operand::Text(expected)) => {
            return match op {
                Op::Eq => text == expected,
                Op::Ne => text != expected,
                _ => false,
            };
        }
        (Raw::Uint(n), Operand::Int(_)) => i128::from(*n),
        (Raw::Int(n), Operand::Int(_)) => i128::from(*n),
        (Raw::Bool(b), Operand::Int(_)) => i128::from(*b),
        _ => return false,
    };
    let Operand::Int(expected) = *operand else {
        return false;
    };
    match op {
        Op::Eq => value == expected,
        Op::Ne => value != expected,
        Op::Lt => value < expected,
        Op::Le => value <= expected,
        Op::Gt => value > expected,
        Op::Ge => value >= expected,
        Op::MaskAny => value & expected != 0,
        Op::MaskAll => value & expected == expected,
    }
}

/// A condition as IR JSON, every comparison in it on `field`.
pub(super) fn condition_json(field: &str, test: &Test) -> Value {
    let operand = |operand: &Operand| match operand {
        Operand::Int(n) => integer_json(*n),
        Operand::Text(text) => json!(text),
    };
    match test {
        Test::Compare(op, value) => {
            json!({ "kind": "compare", "field": field, "op": op.name(), "value": operand(value) })
        }
        Test::In(values) => {
            let values: Vec<Value> = values.iter().map(operand).collect();
            json!({ "kind": "in", "field": field, "values": values })
        }
        Test::Not(test) => json!({ "kind": "not", "condition": condition_json(field, test) }),
        Test::All(tests) | Test::Any(tests) => {
            let kind = if matches!(test, Test::All(_)) {
                "all"
            } else {
                "any"
            };
            let conditions: Vec<Value> = tests.iter().map(|t| condition_json(field, t)).collect();
            json!({ "kind": kind, "conditions": conditions })
        }
    }
}

/// An integer as a raw value of a field of this kind, if it holds it: a
/// bool holds 0 and 1.
fn int_raw(kind: &Kind, value: i128) -> Option<Raw> {
    match kind {
        Kind::Bool => match value {
            0 => Some(Raw::Bool(false)),
            1 => Some(Raw::Bool(true)),
            _ => None,
        },
        Kind::Uint(_) if kind.integers()?.contains(&value) => {
            u64::try_from(value).ok().map(Raw::Uint)
        }
        Kind::Int(_) if kind.integers()?.contains(&value) => {
            i64::try_from(value).ok().map(Raw::Int)
        }
        _ => None,
    }
}

/// The values a selector field of `kind` leans on, for the comparisons its
/// conditions make: what they compare with and the values beside it (for
/// a mask, the mask, none of it, all but its lowest bit, and every other
/// bit), so each condition holds about half the time.
pub(super) fn leaning(kind: &Kind, compared: &[(Op, Operand)]) -> Vec<Raw> {
    let mut raws = Vec::new();
    for (op, operand) in compared {
        match operand {
            Operand::Text(text) => {
                raws.push(Raw::Str(text.clone()));
                raws.push(Raw::Str("z".to_string()));
            }
            Operand::Int(n) => {
                let largest = kind.integers().map_or(0, |range| *range.end());
                let near = match op {
                    Op::MaskAny | Op::MaskAll => [*n, 0, n & (n - 1), largest & !n],
                    _ => [n - 1, *n, n + 1, *n],
                };
                raws.extend(near.into_iter().filter_map(|value| int_raw(kind, value)));
            }
        }
    }
    raws
}
