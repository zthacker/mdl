//! Faults planted in valid JSON messages, and the error each gives
//! (`spec/decoding.md` §2.3 to §5.3, §9). Before any entry: the message
//! delivered as a binary payload, a payload that isn't JSON MDL reads
//! (`malformed.rs`), a `path` alias that can't be read or names no bound
//! asset, a `kind` no packet matches, a document that isn't an object.
//! Then at entries: a required member gone, a value of another kind than
//! its entry takes, a number an integer field can't take.
//!
//! The checks run in §3.3's order, then decoding reads a packet's entries
//! in order, a struct's or a record's entries inside it, and stops at the
//! first that fails, so of two faults the error is the one met first,
//! whatever order the document's members are in. Extraction comes before
//! the event time, so a fault is the error even where `/t` can't be read.

mod malformed;

use proptest::prelude::*;
use proptest::sample::Index;

use super::doc::{self, Doc, Spell};
use super::{
    Alias, Element, Fill, JsonDraft, JsonElement, JsonEntry, JsonNode, JsonPacket, JsonSet,
    JsonTime, Key, document, draft, element_token, index_of, leading_zero, pack, pointer,
};
use crate::layout::faults::{
    FaultKind, Gone, JsonKind, Malformed, Picks, Route, Unfit, Unmet, Unusable, Wanted, pick,
};
use crate::layout::time::{Count, TimeSource};
use crate::layout::{Damage, Encoding, Failure, Kind, LocatorRole, Message, Meta, Problem};
use malformed::TextEdit;

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

    /// The literal as `out_of_range` reports it: a 64-bit integer's
    /// digits, or the double's canonical text (`spec/canonical-json.md`).
    fn reported(self, signed: bool) -> &'static str {
        match self {
            Unfit::Fraction => "1.5",
            Unfit::Large => "100000000000000000000",
            Unfit::Literal if signed => "9223372036854775808",
            Unfit::Literal => "18446744073709552000",
            Unfit::Negative => "-1",
            Unfit::PastSafe => "9007199254740992",
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

    /// The kind of value it takes.
    fn kind(self) -> JsonKind {
        match self {
            Wanted::Uint | Wanted::Int | Wanted::Float => JsonKind::Number,
            Wanted::Bool => JsonKind::Bool,
            Wanted::String => JsonKind::String,
            Wanted::Array => JsonKind::Array,
            Wanted::Object => JsonKind::Object,
        }
    }

    /// The kinds of value it doesn't take: all but its own and `null`,
    /// which is a missing value, not one of another kind.
    fn refused(self) -> Vec<JsonKind> {
        JsonKind::ALL
            .into_iter()
            .filter(|&kind| kind != self.kind() && kind != JsonKind::Null)
            .collect()
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
    /// Where decoding meets it: a check before any entry (§3.3), or after
    /// them, how many entries and elements decoding reads before its own.
    at: usize,
    failure: Failure,
    planted: String,
}

/// Where decoding meets each fault before any entry, in §3.3's order: the
/// encoding, the parse (the alias reads the document, or decoding parses
/// it first), the alias, the asset, identification, then the document as
/// the packet's container. Entries come after. With a fixed alias the
/// parse comes after the asset and set checks, which nothing here fails:
/// this oracle's one asset is always bound.
const ENCODING: usize = 0;
const PARSE: usize = 1;
const ALIAS: usize = 2;
const ASSET: usize = 3;
const IDENTIFY: usize = 4;
const CONTAINER: usize = 5;
const ENTRIES: usize = 6;

#[derive(Debug, Clone)]
enum Edit {
    /// The member at this place removed.
    Remove(Vec<Step>),
    /// The value at this place replaced; with no steps, the document.
    Replace(Vec<Step>, Doc),
    /// A member added to the document.
    Add(String, Doc),
    /// One of the document's members, by its position (wrapping), added
    /// again; two members of one name if it has none.
    Twice(usize),
    /// The object at this place, holding the value at this token, made an
    /// array with the value where a reader taking the token for an index
    /// would look: at `n` for `0n`, last for `-`.
    Arrayed(Vec<Step>, String),
    /// The written text changed.
    Text(TextEdit),
    /// The message delivered as a binary payload.
    AsBinary,
    /// The metadata `kind` taken away, given twice, or holding this value.
    Metadata(Unmet, Meta),
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
                match find(document, parent) {
                    Some(Doc::Object(members)) => {
                        let before = members.len();
                        members.retain(|(member, _)| member != name);
                        members.len() < before
                    }
                    // An array's element goes with those after it, so none
                    // takes its index.
                    Some(Doc::Array(items)) => match index_of(name) {
                        Some(index) if index < items.len() => {
                            items.truncate(index);
                            true
                        }
                        _ => false,
                    },
                    _ => false,
                }
            }
            Edit::Replace(steps, value) => match find(document, steps) {
                Some(slot) => {
                    *slot = value.clone();
                    true
                }
                None => false,
            },
            Edit::Add(name, value) => match document {
                Doc::Object(members) => {
                    members.push((name.clone(), value.clone()));
                    true
                }
                _ => false,
            },
            Edit::Twice(position) => match document {
                Doc::Object(members) if members.is_empty() => {
                    members.push(("d".into(), Doc::Int(1)));
                    members.push(("d".into(), Doc::Int(2)));
                    true
                }
                Doc::Object(members) => {
                    let (name, _) = members[position % members.len()].clone();
                    members.insert(position % (members.len() + 1), (name, Doc::Int(0)));
                    true
                }
                _ => false,
            },
            Edit::Arrayed(steps, token) => {
                let Some(container) = find(document, steps) else {
                    return false;
                };
                let Doc::Object(members) = container else {
                    return false;
                };
                let Some(value) = members
                    .iter()
                    .find(|(member, _)| member == token)
                    .map(|(_, value)| value.clone())
                else {
                    return false;
                };
                let at = token.parse::<usize>().unwrap_or(0);
                let mut items: Vec<Doc> = (0..at).map(|_| Doc::Null).collect();
                items.push(value);
                *container = Doc::Array(items);
                true
            }
            Edit::Text(_) | Edit::AsBinary | Edit::Metadata(..) => true,
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
        // A pointer's token is an array's index where it meets one.
        (Step::Member(name), Doc::Array(items)) => items.get_mut(index_of(name)?)?,
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
    // A second fault of another kind, met elsewhere.
    let kind = first.route.kind();
    let second = picks
        .second
        .as_ref()
        .and_then(|second| {
            let allowed = |other| other != kind && !conflict(kind, other);
            pick(&plants, |plant| plant.route, allowed, second)
        })
        .filter(|second| second.at != first.at);
    let chosen: Vec<&Plant> = std::iter::once(first).chain(second).collect();
    let Some(failing) = chosen.iter().min_by_key(|plant| plant.at) else {
        return valid;
    };
    let mut spell = Spell::new(draft.seed);
    let mut document = document(set, &set.packets[index], draft, &mut spell);
    // The fault met later goes in first, so the one met first is made
    // whatever the later one changed: a member put in twice stays twice
    // where the later fault removes that member. A later fault inside
    // what the first removes is then gone with it.
    for plant in &chosen {
        if plant.at != failing.at {
            plant.edit.apply(&mut document);
        }
    }
    if !failing.edit.apply(&mut document) {
        return valid;
    }
    let mut payload = doc::write(&document, &mut spell).into_bytes();
    let empty = matches!(&document, Doc::Object(members) if members.is_empty());
    for plant in &chosen {
        if let Edit::Text(edit) = &plant.edit {
            payload = edit.apply(payload, empty);
        }
    }
    let as_binary = chosen
        .iter()
        .any(|plant| matches!(plant.edit, Edit::AsBinary));
    let mut metadata = valid.metadata.clone();
    for plant in &chosen {
        if let Edit::Metadata(unmet, value) = &plant.edit {
            match unmet {
                Unmet::Missing => metadata.retain(|(name, _)| name != "kind"),
                Unmet::Twice => metadata.push(("kind".to_string(), value.clone())),
                Unmet::Unknown => {
                    metadata.retain(|(name, _)| name != "kind");
                    metadata.push(("kind".to_string(), value.clone()));
                }
            }
        }
    }
    let routes = chosen.iter().map(|plant| plant.route).collect();
    let planted: Vec<&str> = chosen.iter().map(|plant| plant.planted.as_str()).collect();
    Message {
        payload,
        encoding: if as_binary {
            Encoding::Binary
        } else {
            Encoding::Json
        },
        metadata,
        damage: Some(Damage {
            routes,
            planted: planted.join("; "),
            failure: failing.failure.clone(),
        }),
        ..valid
    }
}

/// Whether faults of two kinds can't be planted together: a document that
/// isn't an object leaves nothing for another fault to change, and both
/// alias faults change the alias's member.
fn conflict(a: FaultKind, b: FaultKind) -> bool {
    let both = |x, y| (a == x && b == y) || (a == y && b == x);
    a == FaultKind::Document
        || b == FaultKind::Document
        || both(FaultKind::Locator, FaultKind::Unbound)
}

/// Every fault the message can be given: those met before any entry, in
/// §3.3's order, then for each entry and element in the order decoding
/// reads them: its time fields, `kind` (a key, damaged only before any
/// entry, as one no packet matches), then its entries.
fn plants(set: &JsonSet, index: usize, draft: &JsonDraft, detail: &Index) -> Vec<Plant> {
    let packet = &set.packets[index];
    let mut plants = whole_message(set, packet, detail);
    let mut walk = Walk {
        at: ENTRIES,
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
    plants.append(&mut walk.plants);
    plants
}

/// The faults met before any entry (§3.3): the encoding, the parse
/// (§2.3), a `path` alias that can't be read or names no bound asset,
/// a `kind` no packet matches (where there's no catch-all to match
/// instead), and a document that isn't an object.
fn whole_message(set: &JsonSet, packet: &JsonPacket, detail: &Index) -> Vec<Plant> {
    let failure = |problem| Failure {
        path: String::new(),
        problem,
    };
    let member = |name: &str, value: Option<Doc>| {
        let steps = vec![Step::Member(name.to_string())];
        match value {
            Some(value) => Edit::Replace(steps, value),
            None => Edit::Remove(steps),
        }
    };
    let mut plants = vec![Plant {
        route: Route::AsBinary,
        edit: Edit::AsBinary,
        at: ENCODING,
        failure: failure(Problem::WrongEncoding {
            expected: Encoding::Json,
            given: Encoding::Binary,
        }),
        planted: "delivered as a binary payload".to_string(),
    }];
    for malformed in Malformed::ALL {
        let (edit, planted) = malformed::plant(malformed, detail);
        plants.push(Plant {
            route: Route::Malformed(malformed),
            edit,
            at: PARSE,
            failure: failure(Problem::InvalidJson),
            planted,
        });
    }
    let unreadable_alias = failure(Problem::UnreadableLocator {
        role: LocatorRole::Alias,
    });
    if set.alias != Alias::Fixed {
        for unusable in Unusable::ALL {
            plants.push(Plant {
                route: Route::Locator(unusable),
                edit: member("sat", unusable.value()),
                at: ALIAS,
                failure: unreadable_alias.clone(),
                planted: format!("{unusable:?} at the alias's /sat"),
            });
        }
        // Text no binding names: not the integer's one spelling (§3.2),
        // or another text or integer than the bound one.
        let text = |texts: [&str; 4]| texts.map(|text| Doc::Str(text.to_string())).to_vec();
        let mut strangers = match set.alias {
            Alias::Number => text(["017", "+17", "17 ", "17.0"]),
            _ => text(["a1", "A1 ", " A1", "A01"]),
        };
        strangers.push(Doc::Int(if set.alias == Alias::Number { 18 } else { 17 }));
        let stranger = strangers[detail.index(strangers.len())].clone();
        plants.push(Plant {
            route: Route::Unbound,
            planted: format!("{stranger:?} at the alias's /sat"),
            edit: member("sat", Some(stranger)),
            at: ASSET,
            failure: failure(Problem::UnknownAsset),
        });
    }
    let catch_all = set.packets.iter().any(|packet| packet.keys.is_empty());
    if !catch_all && set.by_metadata {
        if let Some((_, key)) = packet.keys.first() {
            let given = match key {
                Key::Text(text) => Meta::Text(text.clone()),
                Key::Int(n) => Meta::Int(*n),
            };
            // Text no packet has; or a number, which no text key equals.
            let unknown = match key {
                Key::Int(n) => Meta::Text(leading_zero(*n)),
                Key::Text(_) if detail.index(2) == 0 => Meta::Uint(7),
                Key::Text(_) => Meta::Text("NONE".into()),
            };
            for (unmet, value) in [
                (Unmet::Missing, given.clone()),
                (Unmet::Twice, given),
                (Unmet::Unknown, unknown),
            ] {
                plants.push(Plant {
                    route: Route::Unmet(unmet),
                    planted: format!("the metadata kind {unmet:?} ({value:?})"),
                    edit: Edit::Metadata(unmet, value),
                    at: IDENTIFY,
                    failure: failure(Problem::NoPacket),
                });
            }
        }
    } else if !catch_all && !packet.keys.is_empty() {
        for unusable in Unusable::ALL {
            plants.push(Plant {
                route: Route::Unmatched(unusable),
                edit: member("kind", unusable.value()),
                at: IDENTIFY,
                failure: failure(Problem::NoPacket),
                planted: format!("{unusable:?} at /kind"),
            });
        }
        if let Some((_, Key::Int(n))) = packet.keys.first() {
            plants.push(Plant {
                route: Route::KindAsText,
                edit: member("kind", Some(Doc::Str(n.to_string()))),
                at: IDENTIFY,
                failure: failure(Problem::NoPacket),
                planted: format!("the kind {n} written as text"),
            });
        }
    }
    // Neither `/sat` nor `/kind` finds anything in a document that isn't
    // an object, so the alias can't be read, or no keyed packet matches; a
    // catch-all is the message's, and then the document is `wrong_type`
    // (§5.3). Keyed by metadata, a packet matches all the same, the one a
    // nested packet nests on if `/sub` can't be read, and its document is
    // `wrong_type`.
    for kind in [
        JsonKind::Array,
        JsonKind::Bool,
        JsonKind::Null,
        JsonKind::Number,
        JsonKind::String,
    ] {
        let (at, problem) = if set.alias != Alias::Fixed {
            (
                ALIAS,
                Problem::UnreadableLocator {
                    role: LocatorRole::Alias,
                },
            )
        } else if catch_all || set.by_metadata {
            (
                CONTAINER,
                Problem::WrongType {
                    expected: JsonKind::Object,
                    found: kind,
                },
            )
        } else {
            (IDENTIFY, Problem::NoPacket)
        };
        plants.push(Plant {
            route: Route::Document(kind),
            edit: Edit::Replace(Vec::new(), kind.value()),
            at,
            failure: failure(problem),
            planted: format!("the document {kind:?}"),
        });
    }
    plants
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
        self.gone(at, name, &steps, None, None, &format!("/{name}"));
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
            // A token into a container of its own that isn't an index.
            let arrayed = element_token(&entry.tokens)
                .filter(|token| index_of(token).is_none())
                .map(|token| (within(&entry.tokens[..1]), token.to_string()));
            let at = self.next();
            // An optional entry with nothing at its pointer is absent.
            if !entry.optional {
                self.gone(at, &path, &leaf, parent, arrayed, &pointer(&entry.tokens));
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
        arrayed: Option<(Vec<Step>, String)>,
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
        if let Some((container, token)) = arrayed {
            ways.push((
                Gone::NotIndex,
                Edit::Arrayed(container, token),
                "its container an array, which a token that isn't an index finds nothing in",
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
                problem: Problem::WrongType {
                    expected: wanted.kind(),
                    found,
                },
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
                    problem: Problem::OutOfRange {
                        signed,
                        value: unfit.reported(signed).to_string(),
                    },
                },
                planted: format!("{path}: {literal}"),
            });
        }
    }
}
