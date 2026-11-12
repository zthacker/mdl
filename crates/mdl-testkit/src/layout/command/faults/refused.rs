//! Faults planted in valid requests, and the first error each request
//! reaches (`spec/encoding.md` §1 to §3).

use std::collections::BTreeSet;

use proptest::prelude::*;
use proptest::sample::{Index, select};
use proptest::strategy::Union;

use super::{argument_name, entry_name, kind_weight, list_place};
use crate::layout::command::requests::{Parts, parts};
use crate::layout::command::values::{list_value, list_values, spell, spell_list, value};
use crate::layout::command::{
    Arg, ArgType, Bounds, Counted, Elements, Encoder, Fault, Given, Layout, Len, Placed, Refused,
    Scale, Shape, Source, Stage, Target, Through,
};
use crate::layout::{Numeric, Text};

/// A fault to plant in a valid request.
#[derive(Debug, Clone)]
enum Plant {
    /// A set the family doesn't have.
    Set,
    /// A command the set doesn't encode.
    Command,
    /// An argument the command doesn't have, at a place in the request.
    Unknown { value: Given, place: Index },
    /// An argument given again, at a place in the request.
    Duplicate {
        argument: usize,
        value: Given,
        place: Index,
    },
    /// An argument without a default, left out.
    Missing { argument: usize },
    /// An argument given a value that fails at `stage` with `code`, the
    /// error naming `at`.
    Value {
        argument: usize,
        value: Given,
        stage: Stage,
        code: &'static str,
        at: String,
    },
    /// Lists given together, failing at `stage` with `code`, the error
    /// naming `at`: every list of an array, or of lists that go together,
    /// so that the planted fault is the only rule they break, but for
    /// `later` faults they hold too, caught after it. `argument` is the
    /// list an element's fault is in.
    Lists {
        lists: Vec<(usize, Given)>,
        stage: Stage,
        code: &'static str,
        at: String,
        argument: Option<usize>,
        later: Vec<Fault>,
    },
    /// The target replaced, or left out, failing at its field.
    Target {
        target: Option<Target>,
        entry: usize,
        code: &'static str,
    },
}

/// What a plant touches. Two plants that touch one argument, or the
/// target, aren't both planted; a set or command plant goes alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Site {
    Request,
    Argument(usize),
    Target,
}

impl Plant {
    fn sites(&self) -> Vec<Site> {
        match self {
            Plant::Set | Plant::Command => vec![Site::Request],
            Plant::Unknown { .. } => Vec::new(),
            Plant::Duplicate { argument, .. }
            | Plant::Missing { argument }
            | Plant::Value { argument, .. } => vec![Site::Argument(*argument)],
            Plant::Lists { lists, .. } => lists
                .iter()
                .map(|(argument, _)| Site::Argument(*argument))
                .collect(),
            Plant::Target { .. } => vec![Site::Target],
        }
    }

    fn clashes(&self, other: &Plant) -> bool {
        let (mine, theirs) = (self.sites(), other.sites());
        let request = |sites: &[Site]| sites.contains(&Site::Request);
        request(&mine) || request(&theirs) || mine.iter().any(|site| theirs.contains(site))
    }
}

/// A valid request for the layout's command with one or two faults
/// planted in it, and the error encoding gives. Each fault is of a kind
/// picked first, by its weight (`kind_weight`), however many of it the
/// command has. Half the time, a fault caught late
/// (a length field's or a count's) is paired with one caught before it
/// though its entry comes later, each kind of pair as likely as the
/// others.
pub(in super::super) fn refusal(layout: Layout) -> BoxedStrategy<Refused> {
    let mut staged: Vec<Staged> = Vec::new();
    let mut kinds: Vec<(&'static str, Vec<usize>)> = Vec::new();
    for (kind, plant) in plants(&layout) {
        let index = staged.len();
        staged.push(plant);
        match kinds.iter_mut().find(|(seen, _)| *seen == kind) {
            Some((_, indices)) => indices.push(index),
            None => kinds.push((kind, vec![index])),
        }
    }
    let count = staged.len();
    let mut pairs: Vec<Vec<(usize, usize)>> = vec![Vec::new(); 3];
    for first in 0..count {
        for second in 0..count {
            if let Some(kind) = pair_kind(staged[first].0, staged[second].0) {
                pairs[kind].push((first, second));
            }
        }
    }
    pairs.retain(|of_kind| !of_kind.is_empty());
    // A kind by its weight, then one of it. Two faults planted together,
    // but for the pairs below, are of kinds that don't hide the other.
    let pick = |kinds: &[(&'static str, Vec<usize>)], hidden: bool| {
        let arms: Vec<(u32, BoxedStrategy<usize>)> = kinds
            .iter()
            .filter(|(kind, _)| hidden || !hides_others(kind))
            .map(|(kind, indices)| (kind_weight(kind) as u32, select(indices.clone()).boxed()))
            .collect();
        (!arms.is_empty()).then(|| Union::new_weighted(arms).boxed())
    };
    let one = pick(&kinds, true).unwrap_or_else(|| Just(0).boxed());
    let any_picks = match pick(&kinds, false) {
        Some(shown) => prop_oneof![
            one.prop_map(|pick| vec![pick]),
            (shown.clone(), shown).prop_map(|(first, second)| vec![first, second]),
        ]
        .boxed(),
        None => one.prop_map(|pick| vec![pick]).boxed(),
    };
    let picks = if pairs.is_empty() {
        any_picks.boxed()
    } else {
        let pair = select(pairs)
            .prop_flat_map(select)
            .prop_map(|(first, second)| vec![first, second]);
        prop_oneof![1 => any_picks, 1 => pair].boxed()
    };
    let plants: Vec<BoxedStrategy<Plant>> = staged.into_iter().map(|(_, plant)| plant).collect();
    (Just(layout.clone()), parts(&layout), picks)
        .prop_flat_map(move |(layout, parts, picks)| {
            let chosen: Vec<BoxedStrategy<Plant>> =
                picks.iter().map(|pick| plants[*pick].clone()).collect();
            (Just(layout), Just(parts), chosen)
        })
        .prop_map(|(layout, parts, chosen)| refused(&layout, parts, chosen))
        .boxed()
}

/// The kind of pair two faults make when the one at `late` is caught
/// after the one at `early` though its entry comes first: a length field's
/// after a later entry's (§3.5), a count's after a later entry's, or a
/// count's after a later length field's (§3.6).
fn pair_kind(late: Stage, early: Stage) -> Option<usize> {
    match (late, early) {
        (Stage::Length(measured), Stage::Field { entry: later, .. }) if later > measured => Some(0),
        (Stage::Count(array), Stage::Field { entry: later, .. }) if later > array => Some(1),
        (Stage::Count(array), Stage::Length(later)) if later > array => Some(2),
        _ => None,
    }
}

/// The request with the plants that don't clash planted, and the first
/// error it reaches.
fn refused(layout: &Layout, parts: Parts, chosen: Vec<Plant>) -> Refused {
    let mut plants: Vec<Plant> = Vec::new();
    for plant in chosen {
        if !plants.iter().any(|kept| kept.clashes(&plant)) {
            plants.push(plant);
        }
    }
    let mut set = "fam_v1".to_string();
    let mut command = layout.command.clone();
    let mut named = parts.named;
    let mut target = parts.target;
    let mut faults: Vec<Fault> = Vec::new();
    let mut fault =
        |code: &'static str, stage: Stage, argument: Option<usize>, at: Option<String>| {
            faults.push(Fault {
                code,
                stage,
                argument,
                at,
            });
        };
    for plant in plants {
        match plant {
            Plant::Set => {
                set = "fam_v9".into();
                fault("unknown_set", Stage::Request, None, None);
            }
            Plant::Command => {
                command = "cmd.nope".into();
                fault("unknown_command", Stage::Request, None, None);
            }
            Plant::Unknown { value, place } => {
                named.insert(place.index(named.len() + 1), ("zz".into(), value));
            }
            Plant::Duplicate {
                argument,
                value,
                place,
            } => {
                let name = argument_name(argument);
                if !named.iter().any(|(given, _)| *given == name) {
                    named.push((name.clone(), value.clone()));
                }
                named.insert(place.index(named.len() + 1), (name, value));
            }
            Plant::Missing { argument } => {
                named.retain(|(given, _)| *given != argument_name(argument));
            }
            Plant::Value {
                argument,
                value,
                stage,
                code,
                at,
            } => {
                give(&mut named, argument, value);
                fault(code, stage, Some(argument), Some(at));
            }
            Plant::Lists {
                lists,
                stage,
                code,
                at,
                argument,
                later,
            } => {
                for (argument, value) in lists {
                    give(&mut named, argument, value);
                }
                fault(code, stage, argument, Some(at));
                for caught in later {
                    fault(caught.code, caught.stage, caught.argument, caught.at);
                }
            }
            Plant::Target {
                target: replaced,
                entry,
                code,
            } => {
                target = replaced;
                fault(
                    code,
                    Stage::Field {
                        entry,
                        element: None,
                    },
                    None,
                    entry_name(layout, entry),
                );
            }
        }
    }
    // The first pass: the given names, in the order given.
    let known: BTreeSet<String> = (0..layout.arguments.len()).map(argument_name).collect();
    let mut seen = BTreeSet::new();
    let index_of = |name: &str| (0..layout.arguments.len()).find(|i| argument_name(*i) == name);
    for (place, (name, _)) in named.iter().enumerate() {
        if !known.contains(name) {
            fault(
                "unknown_argument",
                Stage::Name(place),
                None,
                Some(name.clone()),
            );
        } else if !seen.insert(name.clone()) {
            let argument = index_of(name);
            fault(
                "duplicate_argument",
                Stage::Name(place),
                argument,
                Some(name.clone()),
            );
        }
    }
    // The second pass's arguments left out without a default.
    for (index, (encoder, _)) in layout.arguments.iter().enumerate() {
        let name = argument_name(index);
        if encoder.default.is_none() && !named.iter().any(|(given, _)| *given == name) {
            fault(
                "missing_argument",
                Stage::Argument(index),
                Some(index),
                Some(name),
            );
        }
    }
    let first = faults.iter().min_by_key(|fault| fault.stage);
    let (code, at) = first.map_or(("none", None), |fault| (fault.code, fault.at.clone()));
    Refused {
        set,
        command,
        arguments: named,
        target,
        code,
        at,
        faults,
    }
}

/// Gives the argument this value: in its place if the request gives it
/// already, else last.
fn give(named: &mut Vec<(String, Given)>, argument: usize, value: Given) {
    let name = argument_name(argument);
    match named.iter_mut().find(|(given, _)| *given == name) {
        Some(given) => given.1 = value,
        None => named.push((name, value)),
    }
}

/// The index of the field whose value comes from `source`.
fn entry_of(layout: &Layout, wanted: impl Fn(&Source) -> bool) -> Option<usize> {
    layout
        .entries
        .iter()
        .position(|entry| matches!(entry, Placed::Field { source, .. } if wanted(source)))
}

/// A plant of a value for one argument, failing at `stage` with `code`.
fn valued(
    argument: usize,
    values: BoxedStrategy<Given>,
    stage: Stage,
    code: &'static str,
    at: String,
) -> (Stage, BoxedStrategy<Plant>) {
    let plant = values
        .prop_map(move |value| Plant::Value {
            argument,
            value,
            stage,
            code,
            at: at.clone(),
        })
        .boxed();
    (stage, plant)
}

/// A fault to plant, with where it's caught.
type Staged = (Stage, BoxedStrategy<Plant>);

/// Every fault the layout's command can be given, each with its kind and
/// where it's caught (a name's place is only known once it's planted, and
/// an element's index once its list is drawn).
fn plants(layout: &Layout) -> Vec<(&'static str, Staged)> {
    let mut plants: Vec<(&'static str, Staged)> = vec![
        ("request", (Stage::Request, Just(Plant::Set).boxed())),
        ("request", (Stage::Request, Just(Plant::Command).boxed())),
        (
            "name",
            (
                Stage::Name(0),
                (any::<bool>(), any::<Index>())
                    .prop_map(|(flag, place)| Plant::Unknown {
                        value: Given::Bool(flag),
                        place,
                    })
                    .boxed(),
            ),
        ),
    ];
    for (argument, (encoder, encoded)) in layout.arguments.iter().enumerate() {
        let name = argument_name(argument);
        let valid = match encoder.list {
            Some(bounds) => list_value(encoder, bounds.min.unwrap_or(0) as usize),
            None => value(encoder),
        };
        let valid = (valid, any::<u8>()).prop_map(|(value, how)| spell(&value, how));
        let duplicate = (valid, any::<Index>())
            .prop_map(move |(value, place)| Plant::Duplicate {
                argument,
                value,
                place,
            })
            .boxed();
        plants.push(("name", (Stage::Name(0), duplicate)));
        if encoder.default.is_none() {
            let missing = Just(Plant::Missing { argument }).boxed();
            plants.push(("missing", (Stage::Argument(argument), missing)));
        }
        if encoder.list.is_some() {
            let faults = list_faults(layout, argument);
            plants.extend(faults.into_iter().map(|plant| ("list value", plant)));
            continue;
        }
        let at = Stage::Argument(argument);
        // A list, for an argument that takes one value (§2.5).
        let list = (value(encoder), any::<u8>())
            .prop_map(|(value, how)| Given::List(vec![spell(&value, how)]))
            .boxed();
        plants.push((
            "value",
            valued(argument, list, at, "wrong_type", name.clone()),
        ));
        for (values, code) in argument_faults(encoder) {
            plants.push(("value", valued(argument, values, at, code, name.clone())));
        }
        if !encoded {
            if let Some(values) = not_default(encoder) {
                let plant = valued(argument, values, at, "not_default", name.clone());
                plants.push(("value", plant));
            }
            continue;
        }
        let Some(entry) = entry_of(
            layout,
            |source| matches!(source, Source::Argument(a) if *a == argument),
        ) else {
            continue;
        };
        for (values, stage, code) in field_faults(layout, encoder, entry) {
            let (kind, named) = match stage {
                Stage::Length(entry) => ("length", entry_name(layout, entry)),
                Stage::Field { entry, .. } => ("field", entry_name(layout, entry)),
                _ => ("field", None),
            };
            let plant = valued(argument, values, stage, code, named.unwrap_or_default());
            plants.push((kind, plant));
        }
    }
    plants.extend(
        element_faults(layout)
            .into_iter()
            .map(|plant| ("element", plant)),
    );
    plants.extend(count_faults(layout));
    if let Some(entry) = entry_of(layout, |source| matches!(source, Source::Target))
        && let Some(Placed::Field { shape, .. }) = layout.entries.get(entry)
    {
        for (target, code) in target_faults(shape) {
            let plant = Just(Plant::Target {
                target,
                entry,
                code,
            })
            .boxed();
            plants.push((
                "target",
                (
                    Stage::Field {
                        entry,
                        element: None,
                    },
                    plant,
                ),
            ));
        }
    }
    plants
}

/// The lengths a list's bounds allow, the short ones.
fn allowed_lengths(bounds: Bounds) -> Vec<usize> {
    let min = bounds.min.unwrap_or(0);
    (min..=bounds.max.unwrap_or(min + 3))
        .map(|n| n as usize)
        .collect()
}

/// Faults in a list argument's value (§2.5, §2.4): one value rather than a
/// list; a length its bounds don't allow; a value the catalog refuses, at
/// some index of a list of a length they allow, named by that index; and,
/// for a list the set leaves out, a list other than its default.
fn list_faults(layout: &Layout, argument: usize) -> Vec<(Stage, BoxedStrategy<Plant>)> {
    let (encoder, encoded) = &layout.arguments[argument];
    let Some(bounds) = encoder.list else {
        return Vec::new();
    };
    let stage = Stage::Argument(argument);
    let name = argument_name(argument);
    let spelled = |values: BoxedStrategy<Vec<Arg>>| {
        (values, any::<u8>())
            .prop_map(|(values, how)| Given::List(spell_list(&values, how)))
            .boxed()
    };
    let mut faults = Vec::new();
    let single = (value(encoder), any::<u8>())
        .prop_map(|(value, how)| spell(&value, how))
        .boxed();
    faults.push(valued(argument, single, stage, "wrong_type", name.clone()));
    // One value short of the least, or one past the most.
    let mut wrong: Vec<usize> = Vec::new();
    if let Some(min) = bounds.min.filter(|min| *min >= 1) {
        wrong.push(min as usize - 1);
    }
    if let Some(max) = bounds.max {
        wrong.push(max as usize + 1);
    }
    if !wrong.is_empty() {
        let element = encoder.clone();
        let lists = select(wrong)
            .prop_flat_map(move |n| list_values(&element, n))
            .boxed();
        faults.push(valued(
            argument,
            spelled(lists),
            stage,
            "list_length",
            name.clone(),
        ));
    }
    let lengths: Vec<usize> = allowed_lengths(bounds)
        .into_iter()
        .filter(|n| *n >= 1)
        .collect();
    if !lengths.is_empty() {
        for (bad, code) in argument_faults(encoder) {
            let element = encoder.clone();
            let name = name.clone();
            let plant = (select(lengths.clone()), any::<Index>(), bad, any::<u8>())
                .prop_flat_map(move |(n, pick, bad, how)| {
                    (list_values(&element, n), Just((pick, bad, how)))
                })
                .prop_map(move |(values, (pick, bad, how))| {
                    let mut given = spell_list(&values, how);
                    let index = pick.index(given.len());
                    given[index] = bad;
                    Plant::Value {
                        argument,
                        value: Given::List(given),
                        stage,
                        code,
                        at: format!("{name}[{index}]"),
                    }
                })
                .boxed();
            faults.push((stage, plant));
        }
    }
    if !encoded && let Some(lists) = list_not_default(encoder) {
        faults.push(valued(argument, spelled(lists), stage, "not_default", name));
    }
    faults
}

/// Lists other than a left-out list's default that its bounds and element
/// allow (§2.4): the default with one value changed, or another length.
fn list_not_default(encoder: &Encoder) -> Option<BoxedStrategy<Vec<Arg>>> {
    let (Some(Arg::List(default)), Some(bounds)) = (encoder.default.clone(), encoder.list) else {
        return None;
    };
    let mut ways: Vec<BoxedStrategy<Vec<Arg>>> = Vec::new();
    if !default.is_empty() && has_other_values(encoder) {
        let element = encoder.clone();
        let list = default.clone();
        let changed = any::<Index>()
            .prop_flat_map(move |pick| {
                let index = pick.index(list.len());
                let kept = list[index].clone();
                let list = list.clone();
                value(&element)
                    .prop_filter("another value", move |other| !same_value(other, &kept))
                    .prop_map(move |other| {
                        let mut list = list.clone();
                        list[index] = other;
                        list
                    })
            })
            .boxed();
        ways.push(changed);
    }
    let others: Vec<usize> = allowed_lengths(bounds)
        .into_iter()
        .filter(|n| *n != default.len())
        .collect();
    if !others.is_empty() {
        let element = encoder.clone();
        ways.push(
            select(others)
                .prop_flat_map(move |n| list_values(&element, n))
                .boxed(),
        );
    }
    (!ways.is_empty()).then(|| Union::new(ways).boxed())
}

/// The list arguments an array's elements take, in record order.
fn array_lists(elements: &Elements) -> Vec<usize> {
    match elements {
        Elements::Scalar { argument, .. } => vec![*argument],
        Elements::Records { entries, .. } => entries
            .iter()
            .filter_map(|entry| match entry {
                Placed::Field {
                    source: Source::Argument(argument),
                    ..
                } => Some(*argument),
                _ => None,
            })
            .collect(),
    }
}

/// Valid lists for each of `members`, as many values as `lengths` gives
/// each, spelled as a request spells them.
fn lists_of(
    members: Vec<(usize, Encoder)>,
    lengths: impl Fn(usize) -> usize,
) -> BoxedStrategy<Vec<(usize, Given)>> {
    let lists: Vec<BoxedStrategy<(usize, Given)>> = members
        .into_iter()
        .map(|(member, encoder)| {
            (list_values(&encoder, lengths(member)), any::<u8>())
                .prop_map(move |(values, how)| (member, Given::List(spell_list(&values, how))))
                .boxed()
        })
        .collect();
    lists.boxed()
}

/// Values the catalog allows that a list's element field refuses (§3,
/// §3.6), each at some index of a list of a length its group allows. Every
/// list of the group is given, so that only the element is wrong; it's
/// caught at the element, named by its index and record field.
fn element_faults(layout: &Layout) -> Vec<(Stage, BoxedStrategy<Plant>)> {
    let mut faults = Vec::new();
    for group in &layout.groups {
        let members: Vec<(usize, Scale, Encoder)> = group
            .members
            .iter()
            .map(|(member, scale)| (*member, *scale, layout.arguments[*member].0.clone()))
            .collect();
        for &(argument, scale) in &group.members {
            let Some(place) = list_place(layout, argument) else {
                continue;
            };
            let counts: Vec<i64> = group
                .counts
                .iter()
                .copied()
                .filter(|count| scale.length(*count) >= 1)
                .collect();
            if counts.is_empty() {
                continue;
            }
            let encoder = &layout.arguments[argument].0;
            for (bad, stage, code) in field_faults(layout, encoder, place.entry) {
                if !matches!(stage, Stage::Field { .. }) {
                    continue;
                }
                let members = members.clone();
                let place = place.clone();
                let stage = Stage::Field {
                    entry: place.entry,
                    element: Some((0, place.position)),
                };
                let plant = (select(counts.clone()), any::<Index>(), bad)
                    .prop_flat_map(move |(count, pick, bad)| {
                        let given: Vec<(usize, Encoder)> = members
                            .iter()
                            .map(|(member, _, encoder)| (*member, encoder.clone()))
                            .collect();
                        let scales: Vec<(usize, Scale)> = members
                            .iter()
                            .map(|(member, scale, _)| (*member, *scale))
                            .collect();
                        let lengths = move |member: usize| {
                            scales
                                .iter()
                                .find(|(m, _)| *m == member)
                                .map_or(0, |(_, scale)| scale.length(count))
                        };
                        (lists_of(given, lengths), Just((count, pick, bad)))
                    })
                    .prop_map(move |(lists, (count, pick, bad))| {
                        let index = pick.index(scale.length(count));
                        let lists = lists
                            .into_iter()
                            .map(|(member, list)| match list {
                                Given::List(mut values) if member == argument => {
                                    values[index] = bad.clone();
                                    (member, Given::List(values))
                                }
                                other => (member, other),
                            })
                            .collect();
                        Plant::Lists {
                            lists,
                            stage: Stage::Field {
                                entry: place.entry,
                                element: Some((index, place.position)),
                            },
                            code,
                            at: place.path(index),
                            argument: Some(argument),
                            later: Vec::new(),
                        }
                    })
                    .boxed();
                faults.push((stage, plant));
            }
        }
    }
    faults
}

/// An array whose records or elements a group's lists fill: its entry,
/// its name, and those lists.
#[derive(Debug, Clone)]
struct Filled {
    entry: usize,
    name: String,
    members: Vec<usize>,
}

/// A value a list's element field refuses, put first in that list, and
/// the fault it is.
#[derive(Debug, Clone)]
struct Spoiled {
    member: usize,
    bad: Given,
    stage: Stage,
    code: &'static str,
    at: String,
}

/// Lists that go together given lengths that don't, each valid on its own
/// (§3.6): an array's records given lists of two lengths, caught at the
/// array before its records; a count field given a length its `multiply`
/// doesn't divide; two arrays that share a count field giving it two
/// values, caught on the later array. Counts are worked out after every
/// length field.
fn count_faults(layout: &Layout) -> Vec<(&'static str, Staged)> {
    let mut faults = Vec::new();
    let encoders = |members: &[usize]| -> Vec<(usize, Encoder)> {
        members
            .iter()
            .map(|member| (*member, layout.arguments[*member].0.clone()))
            .collect()
    };
    for (entry, placed) in layout.entries.iter().enumerate() {
        let Placed::Array {
            name,
            count,
            elements,
        } = placed
        else {
            continue;
        };
        let members = array_lists(elements);
        let Some(bounds) = members
            .first()
            .and_then(|first| layout.arguments[*first].0.list)
        else {
            continue;
        };
        let lengths = allowed_lengths(bounds);
        if members.len() >= 2 && lengths.len() >= 2 {
            let pairs: Vec<(usize, usize)> = lengths
                .iter()
                .flat_map(|a| lengths.iter().map(move |b| (*a, *b)))
                .filter(|(a, b)| a != b)
                .collect();
            let given = encoders(&members);
            let first = members[0];
            let name = name.clone();
            let stage = Stage::Field {
                entry,
                element: None,
            };
            // Three times in four, a list's first value is also one its
            // field refuses, which the comparison comes before.
            let refusable: Vec<BoxedStrategy<Option<Spoiled>>> = members
                .iter()
                .filter_map(|member| Some((*member, list_place(layout, *member)?)))
                .flat_map(|(member, place)| {
                    field_faults(layout, &layout.arguments[member].0, entry)
                        .into_iter()
                        .filter(|(_, stage, _)| matches!(stage, Stage::Field { .. }))
                        .map(move |(bad, _, code)| {
                            let place = place.clone();
                            bad.prop_map(move |bad| {
                                Some(Spoiled {
                                    member,
                                    bad,
                                    stage: Stage::Field {
                                        entry: place.entry,
                                        element: Some((0, place.position)),
                                    },
                                    code,
                                    at: place.path(0),
                                })
                            })
                            .boxed()
                        })
                })
                .collect();
            let spoiled = if refusable.is_empty() {
                Just(None).boxed()
            } else {
                prop_oneof![1 => Just(None), 3 => Union::new(refusable)].boxed()
            };
            let plant = (select(pairs), spoiled)
                .prop_flat_map(move |((a, b), spoiled)| {
                    let lists = lists_of(
                        given.clone(),
                        move |member| if member == first { a } else { b },
                    );
                    (lists, Just(spoiled))
                })
                .prop_map(move |(lists, spoiled)| {
                    let mut later = Vec::new();
                    let lists = lists
                        .into_iter()
                        .map(|(member, list)| match (list, &spoiled) {
                            (Given::List(mut values), Some(spoiled))
                                if spoiled.member == member && !values.is_empty() =>
                            {
                                values[0] = spoiled.bad.clone();
                                later.push(Fault {
                                    code: spoiled.code,
                                    stage: spoiled.stage,
                                    argument: Some(member),
                                    at: Some(spoiled.at.clone()),
                                });
                                (member, Given::List(values))
                            }
                            (list, _) => (member, list),
                        })
                        .collect();
                    Plant::Lists {
                        lists,
                        stage,
                        code: "bad_count",
                        at: name.clone(),
                        argument: None,
                        later,
                    }
                })
                .boxed();
            faults.push(("records", (stage, plant)));
        }
        if let Counted::Field { multiply, add, .. } = count
            && *multiply >= 2
        {
            let step = i64::from(*multiply);
            let add = *add;
            let undivided: Vec<usize> = lengths
                .iter()
                .copied()
                .filter(|n| (*n as i64 - add).rem_euclid(step) != 0)
                .collect();
            if !undivided.is_empty() {
                let given = encoders(&members);
                let name = name.clone();
                let stage = Stage::Count(entry);
                let plant = select(undivided)
                    .prop_flat_map(move |n| lists_of(given.clone(), move |_| n))
                    .prop_map(move |lists| Plant::Lists {
                        lists,
                        stage,
                        code: "bad_count",
                        at: name.clone(),
                        argument: None,
                        later: Vec::new(),
                    })
                    .boxed();
                faults.push(("multiply", (stage, plant)));
            }
        }
    }
    // Two arrays sharing a count field, given counts that differ.
    for group in &layout.groups {
        let mut arrays: Vec<Filled> = Vec::new();
        for &(member, _) in &group.members {
            let Some(place) = list_place(layout, member) else {
                continue;
            };
            match arrays.iter_mut().find(|array| array.entry == place.entry) {
                Some(array) => array.members.push(member),
                None => arrays.push(Filled {
                    entry: place.entry,
                    name: place.array,
                    members: vec![member],
                }),
            }
        }
        arrays.sort_by_key(|array| array.entry);
        let ([earlier, later], true) = (arrays.as_slice(), group.counts.len() >= 2) else {
            continue;
        };
        let (earlier, name, later) = (&earlier.members, &later.name, later.entry);
        let pairs: Vec<(i64, i64)> = group
            .counts
            .iter()
            .flat_map(|a| group.counts.iter().map(move |b| (*a, *b)))
            .filter(|(a, b)| a != b)
            .collect();
        let scales: Vec<(usize, Scale, bool)> = group
            .members
            .iter()
            .map(|(member, scale)| (*member, *scale, earlier.contains(member)))
            .collect();
        let given = encoders(&group.members.iter().map(|(m, _)| *m).collect::<Vec<_>>());
        let name = name.clone();
        let stage = Stage::Count(later);
        let plant = select(pairs)
            .prop_flat_map(move |(a, b)| {
                let scales = scales.clone();
                lists_of(given.clone(), move |member| {
                    scales
                        .iter()
                        .find(|(m, ..)| *m == member)
                        .map_or(0, |(_, scale, first)| {
                            scale.length(if *first { a } else { b })
                        })
                })
            })
            .prop_map(move |lists| Plant::Lists {
                lists,
                stage,
                code: "bad_count",
                at: name.clone(),
                argument: None,
                later: Vec::new(),
            })
            .boxed();
        faults.push(("shared", (stage, plant)));
    }
    faults
}

/// An integer spelled exactly when it can be, else as a double as far
/// past the type as it is.
fn spell_integer(n: i128) -> Given {
    match (i64::try_from(n), u64::try_from(n)) {
        (Ok(n), _) => Given::Int(n),
        (_, Ok(n)) => Given::Uint(n),
        _ if n > 0 => Given::Float(4e19),
        _ => Given::Float(-1e19),
    }
}

/// Values §2.2 and §2.3 refuse for the argument, each with its code.
fn argument_faults(encoder: &Encoder) -> Vec<(BoxedStrategy<Given>, &'static str)> {
    let mut faults: Vec<(BoxedStrategy<Given>, &'static str)> = Vec::new();
    let wrong = match &encoder.ty {
        ArgType::F64 => select(vec![Given::Text("1".into()), Given::Bool(true)]).boxed(),
        ArgType::U64 | ArgType::I64 => select(vec![
            Given::Float(0.5),
            Given::Text("1".into()),
            Given::Bool(true),
        ])
        .boxed(),
        ArgType::Bool => select(vec![Given::Int(1), Given::Text("true".into())]).boxed(),
        ArgType::Str => Just(Given::Bytes(vec![0x61])).boxed(),
        ArgType::Bytes => Just(Given::Text("ab".into())).boxed(),
        ArgType::Enum(_) => Just(Given::Int(0)).boxed(),
    };
    faults.push((wrong, "wrong_type"));
    match &encoder.ty {
        ArgType::F64 => {
            let infinite = select(vec![f64::NAN, f64::INFINITY, f64::NEG_INFINITY]);
            faults.push((infinite.prop_map(Given::Float).boxed(), "non_finite"));
            if let Some(max) = encoder.max {
                faults.push((Just(Given::Float(max.next_up())).boxed(), "out_of_range"));
            }
            if let Some(min) = encoder.min {
                faults.push((Just(Given::Float(min.next_down())).boxed(), "out_of_range"));
            }
        }
        ArgType::U64 | ArgType::I64 => {
            let types = encoder.ty.integers();
            let (min, max) = encoder.integer_range;
            let high = max.unwrap_or(*types.end());
            let low = min.unwrap_or(*types.start());
            faults.push((Just(spell_integer(high + 1)).boxed(), "out_of_range"));
            faults.push((Just(spell_integer(low - 1)).boxed(), "out_of_range"));
        }
        ArgType::Enum(_) => {
            faults.push((Just(Given::Text("NOPE".into())).boxed(), "unknown_state"));
        }
        _ => {}
    }
    faults
}

/// Values other than its default, for an argument the set leaves out
/// (§2.4), when it has any.
fn not_default(encoder: &Encoder) -> Option<BoxedStrategy<Given>> {
    let default = encoder.default.clone()?;
    has_other_values(encoder).then(|| {
        (value(encoder), any::<u8>())
            .prop_filter("a value other than the default", move |(value, _)| {
                !same_value(value, &default)
            })
            .prop_map(|(value, how)| spell(&value, how))
            .boxed()
    })
}

/// Whether the catalog allows an argument, or a list argument's element,
/// more than one value.
fn has_other_values(encoder: &Encoder) -> bool {
    match &encoder.ty {
        ArgType::Enum(states) => states.len() >= 2,
        ArgType::U64 | ArgType::I64 => {
            let types = encoder.ty.integers();
            let (min, max) = encoder.integer_range;
            min.unwrap_or(*types.start()) < max.unwrap_or(*types.end())
        }
        ArgType::F64 => encoder
            .min
            .zip(encoder.max)
            .is_none_or(|(min, max)| min < max),
        _ => true,
    }
}

/// Two values the same by value, as §2.2 compares them: -0.0 is 0.
fn same_value(a: &Arg, b: &Arg) -> bool {
    match (a, b) {
        (Arg::F64(a), Arg::F64(b)) => a == b,
        _ => a == b,
    }
}

/// Values the catalog allows that the argument's field refuses (§3), each
/// with where it's caught and its code.
fn field_faults(
    layout: &Layout,
    encoder: &Encoder,
    entry: usize,
) -> Vec<(BoxedStrategy<Given>, Stage, &'static str)> {
    let mut faults: Vec<(BoxedStrategy<Given>, Stage, &'static str)> = Vec::new();
    let at = Stage::Field {
        entry,
        element: None,
    };
    // Written as letters other than a terminator.
    let letters = |n: usize, avoid: Option<u8>| -> Vec<u8> {
        let letter = if avoid == Some(b'q') { b'r' } else { b'q' };
        vec![letter; n]
    };
    let given = |bytes: Vec<u8>, text: bool| {
        if text {
            Given::Text(bytes.into_iter().map(char::from).collect())
        } else {
            Given::Bytes(bytes)
        }
    };
    let invertible = matches!(
        encoder.through,
        Through::Plain | Through::Exact(Numeric::Linear { .. })
    );
    match (&encoder.ty, &encoder.shape) {
        // An unranged f64 past binary32's largest (§3.1).
        (ArgType::F64, Shape::Float(32))
            if encoder.min.is_none() && encoder.max.is_none() && invertible =>
        {
            faults.push((
                select(vec![1e300, -1e300]).prop_map(Given::Float).boxed(),
                at,
                "raw_range",
            ));
        }
        (ArgType::Str, Shape::Text(_, len)) | (ArgType::Bytes, Shape::Bytes(len))
            if !matches!(len, Len::Remaining) =>
        {
            let text_value = matches!(encoder.ty, ArgType::Str);
            let encoding = match &encoder.shape {
                Shape::Text(text, _) => Some(*text),
                _ => None,
            };
            // A character its encoding can't write (§3.4).
            match encoding {
                Some(Text::Ascii) => {
                    faults.push((Just(Given::Text("éa".into())).boxed(), at, "invalid_text"));
                }
                Some(Text::Latin1) => {
                    faults.push((Just(Given::Text("€a".into())).boxed(), at, "invalid_text"));
                }
                _ => {}
            }
            // Its terminator (§3.4), when the text can hold it.
            if let Some(terminator) = len.terminator()
                && (!text_value || encoding == Some(Text::Latin1) || terminator < 0x80)
            {
                faults.push((
                    Just(given(vec![terminator], text_value)).boxed(),
                    at,
                    "contains_terminator",
                ));
            }
            match len {
                Len::Fixed(bytes) => {
                    let mut lengths = vec![bytes + 1];
                    if *bytes >= 1 {
                        lengths.push(bytes - 1);
                    }
                    faults.push((
                        select(lengths)
                            .prop_map(move |n| given(letters(n, None), text_value))
                            .boxed(),
                        at,
                        "bad_length",
                    ));
                }
                Len::Padded(bytes, terminator) => {
                    faults.push((
                        Just(given(letters(bytes + 1, Some(*terminator)), text_value)).boxed(),
                        at,
                        "bad_length",
                    ));
                }
                Len::Field { multiply, add, .. } => {
                    // A length the length field can't hold (§3.5), caught
                    // with the length fields, after every entry.
                    let field_bits = length_field_bits(layout, entry);
                    let mut lengths: Vec<usize> = Vec::new();
                    if *add >= 1 {
                        lengths.push((add - 1) as usize);
                    }
                    if *multiply >= 2 && *add >= -1 {
                        lengths.push((add + 1) as usize);
                    }
                    if let Some(bits) = field_bits.filter(|bits| *bits <= 8) {
                        let past = i64::from(*multiply) * (1_i64 << bits) + add;
                        lengths.push(past as usize);
                    }
                    if !lengths.is_empty() {
                        faults.push((
                            select(lengths)
                                .prop_map(move |n| given(letters(n, None), text_value))
                                .boxed(),
                            Stage::Length(entry),
                            "bad_length",
                        ));
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
    faults
}

/// The bits of the field that holds an entry's length, as it's laid out.
fn length_field_bits(layout: &Layout, entry: usize) -> Option<u32> {
    let measured = entry_name(layout, entry)?;
    layout.entries.iter().find_map(|placed| match placed {
        Placed::Field {
            shape: Shape::Uint(bits),
            source: Source::Length(name),
            ..
        } if *name == measured => Some(*bits),
        _ => None,
    })
}

/// Targets the target field refuses (§3.3, §3.4), each with its code.
fn target_faults(shape: &Shape) -> Vec<(Option<Target>, &'static str)> {
    let mut faults = vec![(None, "missing_target")];
    match shape {
        Shape::Uint(bits) | Shape::Int(bits) => {
            faults.push((Some(Target::Text("SN-x".into())), "target_type"));
            let signed = matches!(shape, Shape::Int(_));
            let past = if signed {
                1_i128 << (bits - 1)
            } else {
                1_i128 << bits
            };
            if past <= i128::from(u64::MAX) {
                faults.push((Some(Target::Int(past)), "raw_range"));
            }
        }
        Shape::Text(text, len) => {
            match text {
                Text::Ascii => faults.push((Some(Target::Text("éa".into())), "invalid_text")),
                Text::Latin1 => faults.push((Some(Target::Text("€a".into())), "invalid_text")),
                Text::Utf8 => {}
            }
            let letter = if len.terminator() == Some(b'q') {
                'r'
            } else {
                'q'
            };
            if let Len::Fixed(bytes) | Len::Padded(bytes, _) = len {
                let long: String = std::iter::repeat_n(letter, bytes + 1).collect();
                faults.push((Some(Target::Text(long)), "bad_length"));
            }
        }
        _ => {}
    }
    faults
}

/// Whether faults of a kind are caught before every argument and entry, so
/// that planted with another, they'd hide it.
fn hides_others(kind: &str) -> bool {
    matches!(kind, "request" | "name")
}
