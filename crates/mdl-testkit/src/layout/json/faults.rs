//! Faults planted in valid JSON messages, and the error each gives
//! (`spec/decoding.md` §5.3, §9): a required member gone, a value of
//! another kind than its entry takes, a number an integer field can't
//! take. Decoding reads a packet's entries in order, a struct's or a
//! record's entries inside it, and stops at the first that fails (§3.3),
//! so of two faults the error is the one whose entry it reads first,
//! whatever order the document's members are in. Extraction comes before
//! the event time, so a fault is the error even where `/t` can't be read.

use proptest::prelude::*;
use proptest::sample::Index;

use super::doc::{self, Doc, Spell};
use super::{
    Element, Fill, JsonDraft, JsonElement, JsonEntry, JsonNode, JsonSet, JsonTime, document, draft,
    pack, pointer,
};
use crate::layout::faults::{Gone, JsonKind, Picks, Route, Unfit, Wanted, pick};
use crate::layout::time::{Count, TimeSource};
use crate::layout::{Damage, Failure, Kind, Message, Problem};

impl Unfit {
    /// A number of this kind a field takes none of, signed or not.
    fn literal(self, signed: bool) -> &'static str {
        match self {
            Unfit::Fraction => "1.5",
            Unfit::Large => "1e20",
            Unfit::Literal if signed => "9223372036854775808",
            Unfit::Literal => "18446744073709551616",
            Unfit::Negative => "-1",
            Unfit::PastSafe => "9007199254740992.0",
        }
    }
}

impl JsonKind {
    /// A value of the kind.
    fn value(self) -> Doc {
        match self {
            JsonKind::Array => Doc::Array(vec![Doc::Int(7)]),
            JsonKind::Bool => Doc::Bool(true),
            JsonKind::Null => Doc::Null,
            JsonKind::Number => Doc::Int(7),
            JsonKind::Object => Doc::Object(vec![("v".into(), Doc::Int(7))]),
            JsonKind::String => Doc::Str("7".into()),
        }
    }
}

impl Wanted {
    /// What a field of this kind takes.
    fn of(kind: &Kind) -> Wanted {
        match kind {
            Kind::Uint(_) => Wanted::Uint,
            Kind::Int(_) => Wanted::Int,
            Kind::Float(_) => Wanted::Float,
            Kind::Bool => Wanted::Bool,
            Kind::Text { .. } | Kind::Bytes(_) => Wanted::String,
        }
    }

    /// The kinds of value it doesn't take. `null` is a missing value, not
    /// one of another kind.
    fn refused(self) -> &'static [JsonKind] {
        match self {
            Wanted::Uint | Wanted::Int | Wanted::Float => &[
                JsonKind::Array,
                JsonKind::Bool,
                JsonKind::Object,
                JsonKind::String,
            ],
            Wanted::Bool => &[
                JsonKind::Array,
                JsonKind::Number,
                JsonKind::Object,
                JsonKind::String,
            ],
            Wanted::String => &[
                JsonKind::Array,
                JsonKind::Bool,
                JsonKind::Number,
                JsonKind::Object,
            ],
            Wanted::Array => &[
                JsonKind::Bool,
                JsonKind::Number,
                JsonKind::Object,
                JsonKind::String,
            ],
            Wanted::Object => &[
                JsonKind::Array,
                JsonKind::Bool,
                JsonKind::Number,
                JsonKind::String,
            ],
        }
    }

    /// Whether it's an integer field's, and signed, if so.
    fn signed(self) -> Option<bool> {
        match self {
            Wanted::Uint => Some(false),
            Wanted::Int => Some(true),
            _ => None,
        }
    }
}

/// A message of `set`'s packet `index` with a fault planted in it, or two
/// of different kinds a third of the time. A kind is picked first, then a
/// route of it, then one of the route's faults.
pub(super) fn damaged_message(set: JsonSet, index: usize) -> BoxedStrategy<Message> {
    (draft(&set, index), Picks::arbitrary())
        .prop_map(move |(draft, picks)| damage(&set, index, &draft, &picks))
        .boxed()
}

/// A fault: what it changes, where decoding meets it, and its error.
#[derive(Debug, Clone)]
struct Plant {
    route: Route,
    edit: Edit,
    /// How many entries and elements decoding reads before its own.
    at: usize,
    failure: Failure,
    planted: String,
}

#[derive(Debug, Clone)]
enum Edit {
    /// The member at this place removed.
    Remove(Vec<Step>),
    /// The value at this place replaced.
    Replace(Vec<Step>, Doc),
}

/// A step into a document: a member's name, or an element's index.
#[derive(Debug, Clone)]
enum Step {
    Member(String),
    Index(usize),
}

impl Edit {
    /// Makes the change; `false` where the place isn't there.
    fn apply(&self, document: &mut Doc) -> bool {
        match self {
            Edit::Remove(steps) => {
                let Some((Step::Member(name), parent)) = steps.split_last() else {
                    return false;
                };
                let Some(Doc::Object(members)) = find(document, parent) else {
                    return false;
                };
                let before = members.len();
                members.retain(|(member, _)| member != name);
                members.len() < before
            }
            Edit::Replace(steps, value) => match find(document, steps) {
                Some(slot) => {
                    *slot = value.clone();
                    true
                }
                None => false,
            },
        }
    }
}

/// The value at `steps` into `document`.
fn find<'d>(document: &'d mut Doc, steps: &[Step]) -> Option<&'d mut Doc> {
    let Some((step, rest)) = steps.split_first() else {
        return Some(document);
    };
    let next = match (step, document) {
        (Step::Member(name), Doc::Object(members)) => members
            .iter_mut()
            .find(|(member, _)| member == name)
            .map(|(_, value)| value)?,
        (Step::Index(index), Doc::Array(items)) => items.get_mut(*index)?,
        _ => return None,
    };
    find(next, rest)
}

fn damage(set: &JsonSet, index: usize, draft: &JsonDraft, picks: &Picks) -> Message {
    let valid = pack(set, index, draft);
    let plants = plants(set, index, draft, &picks.detail);
    let Some(first) = pick(&plants, |plant| plant.route, |_| true, &picks.first) else {
        return valid;
    };
    // A second fault of another kind, at another entry.
    let kind = first.route.kind();
    let second = picks
        .second
        .as_ref()
        .and_then(|second| pick(&plants, |plant| plant.route, |other| other != kind, second))
        .filter(|second| second.at != first.at);
    let chosen: Vec<&Plant> = std::iter::once(first).chain(second).collect();
    let Some(failing) = chosen.iter().min_by_key(|plant| plant.at) else {
        return valid;
    };
    let mut spell = Spell::new(draft.seed);
    let mut document = document(set, &set.packets[index], draft, &mut spell);
    if !failing.edit.apply(&mut document) {
        return valid;
    }
    // A later fault is the error only where nothing before it fails; one
    // inside what the first removes isn't there to make.
    for plant in &chosen {
        if plant.at != failing.at {
            plant.edit.apply(&mut document);
        }
    }
    let payload = doc::write(&document, &mut spell).into_bytes();
    let routes = chosen.iter().map(|plant| plant.route).collect();
    let planted: Vec<&str> = chosen.iter().map(|plant| plant.planted.as_str()).collect();
    Message {
        payload,
        damage: Some(Damage {
            routes,
            planted: planted.join("; "),
            failure: failing.failure.clone(),
        }),
        ..valid
    }
}

/// Every fault the message can be given, for each entry and element in
/// the order decoding reads them: its time fields, `kind` (a key, never
/// damaged: a message that matched no packet would be another error),
/// then its entries.
fn plants(set: &JsonSet, index: usize, draft: &JsonDraft, detail: &Index) -> Vec<Plant> {
    let packet = &set.packets[index];
    let mut walk = Walk {
        at: 0,
        detail,
        plants: Vec::new(),
    };
    if let (JsonTime::Fields(source), Some(count)) = (&set.time, packet.timing.count) {
        if matches!(source, TimeSource::Gps { .. }) {
            walk.field("week", Wanted::Uint);
        }
        let wanted = match count {
            Count::Uint(_) => Wanted::Uint,
            Count::Int(_) => Wanted::Int,
            Count::Float(_) => Wanted::Float,
        };
        walk.field("t", wanted);
    }
    if packet.kind_field {
        walk.next();
    }
    walk.entries(&packet.entries, &draft.fills, &[], "");
    walk.plants
}

/// Walks a message's entries in the order decoding reads them, noting the
/// faults each can be given.
struct Walk<'d> {
    /// How many entries and elements decoding reads before the next.
    at: usize,
    detail: &'d Index,
    plants: Vec<Plant>,
}

impl Walk<'_> {
    fn next(&mut self) -> usize {
        let at = self.at;
        self.at += 1;
        at
    }

    /// A top-level time field, at its own name.
    fn field(&mut self, name: &str, wanted: Wanted) {
        let at = self.next();
        let steps = vec![Step::Member(name.to_string())];
        self.gone(at, name, &steps, None, &format!("/{name}"));
        self.value(at, name, &steps, wanted);
    }

    /// A container's present entries, its object at `steps`, its record
    /// at `path`.
    fn entries(&mut self, entries: &[JsonEntry], fills: &[Fill], steps: &[Step], path: &str) {
        for (entry, fill) in entries.iter().zip(fills) {
            if matches!(fill, Fill::Absent { .. } | Fill::Skipped(_)) {
                continue;
            }
            let path = if path.is_empty() {
                entry.name.clone()
            } else {
                format!("{path}.{}", entry.name)
            };
            let within = |tokens: &[String]| -> Vec<Step> {
                let mut place = steps.to_vec();
                place.extend(tokens.iter().map(|token| Step::Member(token.clone())));
                place
            };
            let leaf = within(&entry.tokens);
            let parent = (entry.tokens.len() > 1).then(|| within(&entry.tokens[..1]));
            let at = self.next();
            // An optional entry with nothing at its pointer is absent.
            if !entry.optional {
                self.gone(at, &path, &leaf, parent, &pointer(&entry.tokens));
            }
            let wanted = match &entry.node {
                JsonNode::Field(field) => Wanted::of(&field.kind),
                JsonNode::Array { .. } => Wanted::Array,
                JsonNode::Struct(_) => Wanted::Object,
            };
            self.value(at, &path, &leaf, wanted);
            match (&entry.node, fill) {
                (JsonNode::Struct(ty), Fill::Struct(fills)) => {
                    self.entries(&ty.entries, fills, &leaf, &path);
                }
                (JsonNode::Array { element, .. }, Fill::Array(items)) => {
                    for (position, item) in items.iter().enumerate() {
                        let at = self.next();
                        let path = format!("{path}[{position}]");
                        let mut place = leaf.clone();
                        place.push(Step::Index(position));
                        match (element, item) {
                            (JsonElement::Scalar(field), Some(Element::Scalar(_))) => {
                                self.value(at, &path, &place, Wanted::of(&field.kind));
                            }
                            (JsonElement::Record(ty), Some(Element::Record(fills))) => {
                                self.value(at, &path, &place, Wanted::Object);
                                self.entries(&ty.entries, fills, &place, &path);
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// A required entry's member removed, `null` in its place, or the
    /// object its pointer goes through removed: `missing_value`, which
    /// reports the entry's pointer.
    fn gone(
        &mut self,
        at: usize,
        path: &str,
        leaf: &[Step],
        parent: Option<Vec<Step>>,
        pointer: &str,
    ) {
        let failure = Failure {
            path: path.to_string(),
            problem: Problem::Missing {
                pointer: pointer.to_string(),
            },
        };
        let mut ways = vec![
            (
                Gone::Removed,
                Edit::Remove(leaf.to_vec()),
                "its member removed",
            ),
            (Gone::Null, Edit::Replace(leaf.to_vec(), Doc::Null), "null"),
        ];
        if let Some(parent) = parent {
            ways.push((
                Gone::Parent,
                Edit::Remove(parent),
                "the object its pointer goes through removed",
            ));
        }
        for (gone, edit, what) in ways {
            self.plants.push(Plant {
                route: Route::Missing(gone),
                edit,
                at,
                failure: failure.clone(),
                planted: format!("{path}: {what}"),
            });
        }
    }

    /// A value of another kind than the entry or element takes:
    /// `wrong_type`; and for an integer field, each kind of number it
    /// takes none of: `out_of_range`.
    fn value(&mut self, at: usize, path: &str, place: &[Step], wanted: Wanted) {
        let refused = wanted.refused();
        let found = refused[self.detail.index(refused.len())];
        self.plants.push(Plant {
            route: Route::WrongType(wanted),
            edit: Edit::Replace(place.to_vec(), found.value()),
            at,
            failure: Failure {
                path: path.to_string(),
                problem: Problem::WrongType { wanted, found },
            },
            planted: format!("{path}: {found:?} where it takes {wanted:?}"),
        });
        let Some(signed) = wanted.signed() else {
            return;
        };
        for unfit in Unfit::ALL {
            if signed && unfit == Unfit::Negative {
                continue;
            }
            let literal = unfit.literal(signed);
            self.plants.push(Plant {
                route: Route::OutOfRange(unfit),
                edit: Edit::Replace(place.to_vec(), Doc::Literal(literal.to_string())),
                at,
                failure: Failure {
                    path: path.to_string(),
                    problem: Problem::OutOfRange { signed },
                },
                planted: format!("{path}: {literal}"),
            });
        }
    }
}
