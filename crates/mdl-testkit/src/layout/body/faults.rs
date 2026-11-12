//! Faults planted in valid telemetry messages, and the error each one
//! gives (`spec/decoding.md` §5, §9).
//!
//! The writer notes each step decoding takes and the bits it needs
//! ([`Step`]). Decoding reads a packet's entries in order and stops at
//! the first failure (§3.3), so a damaged message's error is the first of
//! its steps that fails for its payload's length. Where two faults are
//! planted, the one decoding reaches first is the error; and a fault in a
//! message whose event time can't be read is still the error, since
//! extraction comes before the event time.

use std::collections::BTreeMap;

use proptest::prelude::*;
use proptest::sample::Index;

use super::{Bound, Counted, ElementNode, Entry, Node, element_bits, present};
use crate::layout::time::received;
use crate::layout::{
    Damage, Draft, Failure, Kind, Length, Message, PacketLayout, Problem, Raw, Text, TimeSource,
    draft, encode, length_field, pack, write_message,
};

/// A kind of fault, as faults are picked: a kind first, each as likely as
/// the others. The variants are in the order picking sorts them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FaultKind {
    /// A count field holding a count below 0, or its largest.
    Count,
    /// The payload cut short.
    Cut,
    /// A length field holding a length below 0, or its largest.
    Length,
    /// Part of one more element after an array that runs to the end.
    More,
    /// A count field shared by arrays, holding a count the first can have
    /// and a later one can't.
    SharedCount,
    /// A string's bytes that aren't text in its encoding.
    Text,
}

impl FaultKind {
    pub const ALL: [FaultKind; 6] = [
        FaultKind::Count,
        FaultKind::Cut,
        FaultKind::Length,
        FaultKind::More,
        FaultKind::SharedCount,
        FaultKind::Text,
    ];

    /// Whether faults of the kind give the payload its length. Of two
    /// faults planted together, at most one does.
    fn cuts(self) -> bool {
        match self {
            FaultKind::Cut | FaultKind::More => true,
            FaultKind::Count | FaultKind::Length | FaultKind::SharedCount | FaultKind::Text => {
                false
            }
        }
    }
}

/// What a step reads, and so where a payload that ends too soon fails. The
/// variants are in the order picking sorts them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Place {
    /// An array's count, checked as `count × size` (§5.5).
    Array,
    /// An array's count, checked a bit per element.
    ArrayBitEach,
    /// A scalar element.
    Element,
    /// A field, at the top level or in a struct.
    Field,
    /// A header or time field.
    Header,
    /// Padding at a packet's top level.
    Padding,
    /// Padding in a struct entry.
    PaddingInStruct,
    /// Padding in an array's element.
    PaddingInElement,
    /// A value up to a terminator.
    Terminator,
    /// A CUC time code, padding to decoding.
    TimeCode,
}

impl Place {
    pub const ALL: [Place; 10] = [
        Place::Array,
        Place::ArrayBitEach,
        Place::Element,
        Place::Field,
        Place::Header,
        Place::Padding,
        Place::PaddingInStruct,
        Place::PaddingInElement,
        Place::Terminator,
        Place::TimeCode,
    ];
}

/// Bytes that aren't text in an encoding. The variants are in the order
/// picking sorts them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Garbling {
    /// A UTF-8 sequence cut off by the value's end.
    CutOff,
    /// An ASCII byte of 0x80 or more.
    High,
    /// A byte UTF-8 never has.
    Never,
    /// An overlong UTF-8 form.
    Overlong,
    /// A UTF-8 code point past U+10FFFF.
    PastMax,
    /// A stray UTF-8 continuation byte.
    Stray,
    /// A UTF-8 surrogate.
    Surrogate,
}

impl Garbling {
    pub const ALL: [Garbling; 7] = [
        Garbling::CutOff,
        Garbling::High,
        Garbling::Never,
        Garbling::Overlong,
        Garbling::PastMax,
        Garbling::Stray,
        Garbling::Surrogate,
    ];

    /// Its name, for a failing case's report.
    fn name(self) -> &'static str {
        match self {
            Garbling::CutOff => "cut_off",
            Garbling::High => "high",
            Garbling::Never => "never",
            Garbling::Overlong => "overlong",
            Garbling::PastMax => "past_max",
            Garbling::Stray => "stray",
            Garbling::Surrogate => "surrogate",
        }
    }

    fn sequence(self) -> &'static [u8] {
        match self {
            Garbling::CutOff => &[0xe2, 0x82],
            Garbling::High => &[0xe9],
            Garbling::Never => &[0xff],
            Garbling::Overlong => &[0xc0, 0xaf],
            Garbling::PastMax => &[0xf4, 0x90, 0x80, 0x80],
            Garbling::Stray => &[0x80],
            Garbling::Surrogate => &[0xed, 0xa0, 0x80],
        }
    }
}

/// What a count or length field is made to hold: its largest raw value,
/// or 0 where that gives a count or length below 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Held {
    Largest,
    Negative,
}

impl Held {
    pub const ALL: [Held; 2] = [Held::Largest, Held::Negative];

    /// The raw value it puts in a field of `bits`.
    fn raw(self, bits: u32) -> u64 {
        match self {
            Held::Largest if bits >= 64 => u64::MAX,
            Held::Largest => (1 << bits) - 1,
            Held::Negative => 0,
        }
    }
}

/// A fault's route: its kind, and what it reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Route {
    Count(Held),
    /// A cut, by what decoding then reads short.
    Cut(Place),
    Length(Held),
    /// One more element, by what in it decoding reads short.
    More(Place),
    SharedCount,
    Text(Garbling),
}

impl Route {
    pub fn kind(self) -> FaultKind {
        match self {
            Route::Count(_) => FaultKind::Count,
            Route::Cut(_) => FaultKind::Cut,
            Route::Length(_) => FaultKind::Length,
            Route::More(_) => FaultKind::More,
            Route::SharedCount => FaultKind::SharedCount,
            Route::Text(_) => FaultKind::Text,
        }
    }
}

/// A step decoding takes, as the writer notes it.
#[derive(Debug, Clone)]
pub(in crate::layout) enum Step {
    /// Reading `place` needs the packet's bits up to `needs`: a packet
    /// that ends before fails with `short`, named by `path`.
    Reach {
        path: String,
        needs: u64,
        short: Short,
        place: Place,
    },
    /// Fails whatever the packet's length.
    Fail(Failure),
    /// An array that runs to the end reads another element only if the
    /// packet goes past `at` (§5.2).
    More { at: u64 },
    /// A packet that ends before `needs` leaves what decoding does
    /// unknown.
    Opaque { needs: u64 },
    /// What decoding does from here can't be told from the values: a
    /// count or length past those they hold.
    Unknown,
}

/// What a packet that ends too soon fails with.
#[derive(Debug, Clone, Copy)]
pub(in crate::layout) enum Short {
    PastEnd,
    /// Padding: `past_end`, named by its container (§5.2).
    Padding,
    /// A value up to a terminator the packet doesn't reach.
    NoTerminator(u8),
}

/// What decoding a payload does.
#[derive(Debug, Clone)]
enum Outcome {
    Decodes,
    /// Fails, reading `place` short if that's how.
    Fails(Failure, Option<Place>),
    Unknown,
}

/// What decoding a payload of `bits` does, by the steps its values take.
fn outcome(steps: &[Step], bits: u64) -> Outcome {
    for step in steps {
        match step {
            Step::Reach {
                path,
                needs,
                short,
                place,
            } if *needs > bits => {
                let problem = match short {
                    Short::NoTerminator(byte) => Problem::NoTerminator { byte: *byte },
                    Short::PastEnd | Short::Padding => Problem::PastEnd {
                        needed_bits: *needs,
                        available_bits: bits,
                        padding: matches!(short, Short::Padding),
                    },
                };
                let path = path.clone();
                return Outcome::Fails(Failure { path, problem }, Some(*place));
            }
            Step::Fail(failure) => return Outcome::Fails(failure.clone(), None),
            Step::More { at } if bits <= *at => return Outcome::Decodes,
            Step::Opaque { needs } if *needs > bits => return Outcome::Unknown,
            Step::Unknown => return Outcome::Unknown,
            _ => {}
        }
    }
    Outcome::Decodes
}

// ---- Planting ---------------------------------------------------------------

/// A message of `packet` with a fault planted in it, or two. A kind of
/// fault is picked first, each kind as likely as the others, then a route
/// of that kind, then one of the route's faults. A message nothing can be
/// planted in is left as it is.
pub(in crate::layout) fn damaged_message(
    packet: PacketLayout,
    time: TimeSource,
) -> BoxedStrategy<Message> {
    received()
        .prop_flat_map(move |ground| {
            let picks = Picks::arbitrary();
            (
                draft(&packet, &time, ground, Vec::new()),
                picks,
                Just(packet.clone()),
                Just(time.clone()),
            )
        })
        .prop_map(|(draft, picks, packet, time)| damage(&packet, &time, draft, &picks))
        .boxed()
}

/// What a damaged message's faults are picked by: a kind, a route of it
/// and one of the route's faults, for the first fault and for a second,
/// planted a third of the time; and where a garbled value's bytes go.
#[derive(Debug, Clone)]
struct Picks {
    first: [Index; 3],
    second: Option<[Index; 3]>,
    detail: Index,
}

impl Picks {
    fn arbitrary() -> BoxedStrategy<Picks> {
        (
            any::<[Index; 3]>(),
            proptest::option::weighted(0.35, any::<[Index; 3]>()),
            any::<Index>(),
        )
            .prop_map(|(first, second, detail)| Picks {
                first,
                second,
                detail,
            })
            .boxed()
    }
}

/// A fault, and what was planted, for a failing case's report.
#[derive(Debug, Clone)]
struct Plant {
    route: Route,
    change: Change,
    planted: String,
}

#[derive(Debug, Clone)]
enum Change {
    /// The payload cut to this many bytes.
    Cut(usize),
    /// One more element in the array that runs to the end, the payload
    /// cut to this many bytes inside it.
    More(usize),
    /// A count or length field, at this place in the record, holding this
    /// raw value.
    Hold(Vec<Seg>, u64),
    /// A string's bytes, at this path, replaced by these.
    Garble(String, Vec<u8>),
}

/// A step of a place in a record.
#[derive(Debug, Clone)]
enum Seg {
    Name(String),
    Index(usize),
}

fn damage(packet: &PacketLayout, time: &TimeSource, draft: Draft, picks: &Picks) -> Message {
    let valid = pack(packet, time, draft.clone());
    let length = valid.payload.len();
    let plants = plants(packet, time, &draft, length, &picks.detail);
    let Some(first) = pick(&plants, |_| true, &picks.first) else {
        return valid;
    };
    // A second fault of another kind; at most one of them cuts the
    // payload to a length.
    let kind = first.route.kind();
    let second = picks.second.as_ref().and_then(|second| {
        pick(
            &plants,
            |other| other != kind && !(other.cuts() && kind.cuts()),
            second,
        )
    });
    let mut attempts = vec![vec![first]];
    if let Some(second) = second {
        attempts.insert(0, vec![first, second]);
    }
    for attempt in attempts {
        let changes: Vec<&Change> = attempt.iter().map(|plant| &plant.change).collect();
        let (payload, Outcome::Fails(failure, _)) = apply(packet, time, &draft, length, &changes)
        else {
            continue;
        };
        let routes: Vec<Route> = attempt.iter().map(|plant| plant.route).collect();
        let planted: Vec<&str> = attempt.iter().map(|plant| plant.planted.as_str()).collect();
        return Message {
            payload,
            damage: Some(Damage {
                routes,
                planted: planted.join("; "),
                failure,
            }),
            ..valid
        };
    }
    valid
}

/// A plant among those `allowed` by kind: a kind, a route of it and a
/// plant of the route, by `picks`.
fn pick<'p>(
    plants: &'p [Plant],
    allowed: impl Fn(FaultKind) -> bool,
    picks: &[Index; 3],
) -> Option<&'p Plant> {
    let [kind, route, which] = picks;
    let mut kinds: Vec<FaultKind> = plants
        .iter()
        .map(|plant| plant.route.kind())
        .filter(|kind| allowed(*kind))
        .collect();
    kinds.sort_unstable();
    kinds.dedup();
    let kind = *kinds.get(kind.index(kinds.len().max(1)))?;
    let mut routes: Vec<Route> = plants
        .iter()
        .map(|plant| plant.route)
        .filter(|route| route.kind() == kind)
        .collect();
    routes.sort_unstable();
    routes.dedup();
    let route = *routes.get(route.index(routes.len().max(1)))?;
    let of_route: Vec<&Plant> = plants.iter().filter(|plant| plant.route == route).collect();
    of_route.get(which.index(of_route.len().max(1))).copied()
}

/// The payload with `changes` made to the message, cut to its length
/// (`length` bytes, unless a change cuts it), and what decoding it does.
fn apply(
    packet: &PacketLayout,
    time: &TimeSource,
    draft: &Draft,
    length: usize,
    changes: &[&Change],
) -> (Vec<u8>, Outcome) {
    let mut values = draft.values.clone();
    let mut noise = draft.noise.clone();
    let mut garbled = None;
    let mut bytes = length;
    for change in changes {
        match change {
            Change::Cut(cut) => bytes = bytes.min(*cut),
            Change::More(cut) => {
                let Some(room) = one_more(&packet.body.entries, &mut values) else {
                    return (noise, Outcome::Unknown);
                };
                noise.resize(noise.len() + room, 0);
                bytes = *cut;
            }
            Change::Hold(spot, raw) => match value_at(&mut values, spot) {
                Some(value) => *value = Raw::Uint(*raw),
                None => return (noise, Outcome::Unknown),
            },
            Change::Garble(path, planted) => garbled = Some((path.clone(), planted.clone())),
        }
    }
    let (_, steps, _) = write_message(packet, time, &values, &mut noise, &draft.timing, garbled);
    noise.truncate(bytes);
    (noise, outcome(&steps, 8 * bytes as u64))
}

/// Writes the last element of the array the body ends with once more, if
/// it runs to the end and has one; returns the most bytes the element
/// takes.
fn one_more(entries: &[Entry], values: &mut [(String, Raw)]) -> Option<usize> {
    let Some(Node::Array(array)) = entries.last().map(|entry| &entry.node) else {
        return None;
    };
    if !matches!(array.count, Counted::Remaining) {
        return None;
    }
    let Some((_, Raw::Array(items))) = values.last_mut() else {
        return None;
    };
    let last = items.last()?.clone();
    items.push(last);
    usize::try_from(element_bits(&array.element, Bound::Most).div_ceil(8)).ok()
}

/// The value at a place in a record.
fn value_at<'r>(record: &'r mut [(String, Raw)], spot: &[Seg]) -> Option<&'r mut Raw> {
    let (Seg::Name(first), rest) = spot.split_first()? else {
        return None;
    };
    let mut value = &mut record.iter_mut().find(|(name, _)| name == first)?.1;
    for seg in rest {
        value = match (seg, value) {
            (Seg::Index(i), Raw::Array(items)) => items.get_mut(*i)?,
            (Seg::Name(name), Raw::Record(fields)) => {
                &mut fields.iter_mut().find(|(field, _)| field == name)?.1
            }
            _ => return None,
        };
    }
    Some(value)
}

// ---- What can be planted ----------------------------------------------------

/// Every fault the message can be given that decoding fails with.
fn plants(
    packet: &PacketLayout,
    time: &TimeSource,
    draft: &Draft,
    length: usize,
    detail: &Index,
) -> Vec<Plant> {
    let mut plants = Vec::new();
    let mut noise = draft.noise.clone();
    let (_, steps, _) = write_message(packet, time, &draft.values, &mut noise, &draft.timing, None);
    // Every shorter length that fails.
    for cut in 1..length {
        if let Outcome::Fails(_, Some(place)) = outcome(&steps, 8 * cut as u64) {
            plants.push(Plant {
                route: Route::Cut(place),
                change: Change::Cut(cut),
                planted: format!("cut to {cut} bytes"),
            });
        }
    }
    // Part of one more element after an array that runs to the end.
    let mut values = draft.values.clone();
    if let Some(room) = one_more(&packet.body.entries, &mut values) {
        let mut noise = draft.noise.clone();
        noise.resize(noise.len() + room, 0);
        let (_, steps, end) = write_message(packet, time, &values, &mut noise, &draft.timing, None);
        let end = end.map_or(0, |end| end.div_ceil(8) as usize);
        for cut in length + 1..end {
            if let Outcome::Fails(_, Some(place)) = outcome(&steps, 8 * cut as u64) {
                plants.push(Plant {
                    route: Route::More(place),
                    change: Change::More(cut),
                    planted: format!("one more element, the payload cut to {cut} bytes"),
                });
            }
        }
    }
    let mut found = Targets::default();
    targets(&packet.body.entries, &draft.values, &[], "", &mut found);
    let mut held = Vec::new();
    for count in &found.counts {
        // A raw value that gives the first array it counts a count below
        // 0; and one that gives the first a count of 0 or more and a later
        // one a count below 0, which fails after the arrays before it have
        // read their elements.
        let counts = |raw: u64| {
            let raw = i128::from(raw);
            count
                .scales
                .iter()
                .map(move |(multiply, add)| raw * i128::from(*multiply) + i128::from(*add))
        };
        if counts(Held::Negative.raw(count.bits))
            .next()
            .is_some_and(|first| first < 0)
        {
            held.push((
                Route::Count(Held::Negative),
                count.spot.clone(),
                &count.path,
                Held::Negative.raw(count.bits),
            ));
        }
        let shared = (0..=Held::Largest.raw(count.bits).min(8)).find(|&raw| {
            counts(raw).next().is_some_and(|first| first >= 0) && counts(raw).any(|n| n < 0)
        });
        if let Some(raw) = shared {
            held.push((Route::SharedCount, count.spot.clone(), &count.path, raw));
        }
        held.push((
            Route::Count(Held::Largest),
            count.spot.clone(),
            &count.path,
            Held::Largest.raw(count.bits),
        ));
    }
    for length in &found.lengths {
        if length.add < 0 {
            held.push((
                Route::Length(Held::Negative),
                length.spot.clone(),
                &length.path,
                Held::Negative.raw(length.bits),
            ));
        }
        held.push((
            Route::Length(Held::Largest),
            length.spot.clone(),
            &length.path,
            Held::Largest.raw(length.bits),
        ));
    }
    for (route, spot, path, raw) in held {
        let change = Change::Hold(spot, raw);
        if let (_, Outcome::Fails(..)) = apply(packet, time, draft, length, &[&change]) {
            plants.push(Plant {
                route,
                change,
                planted: format!("{path} holding {raw}"),
            });
        }
    }
    for text in &found.texts {
        for &form in garblings(text.text) {
            let Some(bytes) = garble(text, form, detail) else {
                continue;
            };
            let planted = format!("{} as {bytes:02x?} ({})", text.path, form.name());
            let change = Change::Garble(text.path.clone(), bytes);
            if let (_, Outcome::Fails(..)) = apply(packet, time, draft, length, &[&change]) {
                plants.push(Plant {
                    route: Route::Text(form),
                    change,
                    planted,
                });
            }
        }
    }
    plants
}

/// The ways bytes can fail to be text in an encoding. Latin-1 has none.
fn garblings(text: Text) -> &'static [Garbling] {
    match text {
        Text::Utf8 => &[
            Garbling::Stray,
            Garbling::Overlong,
            Garbling::Surrogate,
            Garbling::PastMax,
            Garbling::Never,
            Garbling::CutOff,
        ],
        Text::Ascii => &[Garbling::High],
        Text::Latin1 => &[],
    }
}

/// A string's value garbled: as many bytes as it had, letters but for
/// `form`'s sequence, at a place `detail` picks (at the very end, for a
/// sequence the end cuts off). `None` if the value is too short, or the
/// sequence holds its terminator.
fn garble(text: &TextTarget, form: Garbling, detail: &Index) -> Option<Vec<u8>> {
    let sequence = form.sequence();
    let length = text.bytes.len();
    if length < sequence.len() || text.terminator.is_some_and(|t| sequence.contains(&t)) {
        return None;
    }
    let letter = if text.terminator == Some(b'a') {
        b'b'
    } else {
        b'a'
    };
    let mut bytes = vec![letter; length];
    let at = if form == Garbling::CutOff {
        length - sequence.len()
    } else {
        detail.index(length - sequence.len() + 1)
    };
    bytes[at..at + sequence.len()].copy_from_slice(sequence);
    Some(bytes)
}

/// What a message's values hold that faults can be planted in.
#[derive(Debug, Default)]
struct Targets {
    counts: Vec<CountTarget>,
    lengths: Vec<LengthTarget>,
    texts: Vec<TextTarget>,
}

/// A count field, and the `(multiply, add)` of each array it counts, in
/// order.
#[derive(Debug)]
struct CountTarget {
    spot: Vec<Seg>,
    path: String,
    bits: u32,
    scales: Vec<(u32, i64)>,
}

/// A length field.
#[derive(Debug)]
struct LengthTarget {
    spot: Vec<Seg>,
    path: String,
    bits: u32,
    add: i64,
}

/// A string's value, in its encoding.
#[derive(Debug)]
struct TextTarget {
    path: String,
    text: Text,
    bytes: Vec<u8>,
    terminator: Option<u8>,
}

/// The targets among a container's values, at `spot` and `path`.
fn targets(
    entries: &[Entry],
    record: &[(String, Raw)],
    spot: &[Seg],
    path: &str,
    found: &mut Targets,
) {
    let place = |name: &str| {
        let mut place = spot.to_vec();
        place.push(Seg::Name(name.to_string()));
        let path = if path.is_empty() {
            name.to_string()
        } else {
            format!("{path}.{name}")
        };
        (place, path)
    };
    let mut scales: BTreeMap<&str, Vec<(u32, i64)>> = BTreeMap::new();
    for entry in entries {
        if let Node::Array(array) = &entry.node
            && let Counted::Field {
                field,
                multiply,
                add,
            } = &array.count
        {
            scales.entry(field).or_default().push((*multiply, *add));
        }
    }
    let bits_of = |name: &str| {
        entries.iter().find_map(|entry| match &entry.node {
            Node::Field(field) if field.name == name => Some(field.kind.bits()),
            _ => None,
        })
    };
    for (entry, (name, raw)) in present(entries, record) {
        let (here, at) = place(name);
        match (&entry.node, raw) {
            (Node::Field(field), raw) => {
                if let Some(scales) = scales.get(name.as_str()) {
                    found.counts.push(CountTarget {
                        spot: here,
                        path: at.clone(),
                        bits: field.kind.bits(),
                        scales: scales.clone(),
                    });
                }
                if let Some(Length::Field { add, .. }) = field.kind.length() {
                    let name = length_field(name);
                    let (spot, path) = place(&name);
                    found.lengths.push(LengthTarget {
                        spot,
                        path,
                        bits: bits_of(&name).unwrap_or(0),
                        add,
                    });
                }
                text_target(&field.kind, raw, &at, found);
            }
            (Node::Array(array), Raw::Array(items)) => {
                for (index, item) in items.iter().enumerate() {
                    let mut element = here.clone();
                    element.push(Seg::Index(index));
                    let path = format!("{at}[{index}]");
                    match (&array.element, item) {
                        (ElementNode::Scalar(scalar), raw) => {
                            text_target(&scalar.kind, raw, &path, found);
                        }
                        (ElementNode::Struct(ty), Raw::Record(fields)) => {
                            targets(&ty.entries, fields, &element, &path, found);
                        }
                        _ => {}
                    }
                }
            }
            (Node::Struct(node), Raw::Record(fields)) => {
                targets(&node.ty.entries, fields, &here, &at, found);
            }
            _ => {}
        }
    }
}

fn text_target(kind: &Kind, raw: &Raw, path: &str, found: &mut Targets) {
    if let (Kind::Text { length, text }, Raw::Str(value)) = (kind, raw) {
        found.texts.push(TextTarget {
            path: path.to_string(),
            text: *text,
            bytes: encode(value, *text),
            terminator: length.terminator(),
        });
    }
}
