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
use std::ops::Range;

use proptest::prelude::*;
use proptest::sample::Index;

use super::{Bound, Counted, ElementNode, Entry, Node, element_bits, fixed_checks, present};
use crate::checksum::Algorithm;
use crate::layout::time::received;
use crate::layout::{
    Damage, Draft, Failure, Kind, Length, Message, PacketLayout, Problem, Raw, Text, TimeSource,
    draft, encode, length_field, pack, write_message,
};

/// A kind of fault, as faults are picked: a kind first, each as likely as
/// the others. The variants are in the order picking sorts them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FaultKind {
    /// A checksum's value, or what it covers, not matching ([`Tamper`]).
    Checksum,
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
    pub const ALL: [FaultKind; 7] = [
        FaultKind::Checksum,
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
            FaultKind::Checksum
            | FaultKind::Count
            | FaultKind::Length
            | FaultKind::SharedCount
            | FaultKind::Text => false,
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
    /// A checksum's value.
    Checksum,
    /// A scalar element.
    Element,
    /// A field, at the top level or in a struct.
    Field,
    /// A header or time field.
    Header,
    /// The packet's `length`, which a shorter payload is `too_short` for,
    /// before any entry is read (§5.1).
    Length,
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
    pub const ALL: [Place; 12] = [
        Place::Array,
        Place::ArrayBitEach,
        Place::Checksum,
        Place::Element,
        Place::Field,
        Place::Header,
        Place::Length,
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

/// What a checksum fault changes. The variants are in the order picking
/// sorts them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tamper {
    /// A bit the checksum covers that nothing else depends on, flipped.
    Covered,
    /// A count, length or string changed as their faults change them,
    /// with each checksum at a static position holding the valid
    /// message's value: decoding verifies those first, so the error is
    /// `bad_checksum`, not the change's own (§5.6).
    Hidden,
    /// A bit of the checksum's value, flipped.
    Stored,
}

impl Tamper {
    pub const ALL: [Tamper; 3] = [Tamper::Covered, Tamper::Hidden, Tamper::Stored];
}

/// A fault's route: its kind, and what it reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Route {
    Checksum(Tamper),
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
            Route::Checksum(_) => FaultKind::Checksum,
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
    /// The packet's `length` (§5.1): a payload of fewer than `bytes` bytes
    /// is `too_short`; with `exact`, only its first `bytes` are the packet.
    Packet { bytes: u32, exact: bool },
    /// A checksum at a static position, verified before any entry is read,
    /// in order of position; one past the packet's end isn't a problem
    /// yet (§5.6).
    Verify(Check),
    /// A checksum the walk reaches: past the end if the packet ends inside
    /// its value, else verified then, unless it's at a static position,
    /// and so verified already.
    Checksum { check: Check, fixed: bool },
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

/// A checksum where a message has it (§5.6): it covers the bits from
/// `from` up to `at`, where its value starts.
#[derive(Debug, Clone)]
pub(in crate::layout) struct Check {
    pub(in crate::layout) path: String,
    pub(in crate::layout) from: u64,
    pub(in crate::layout) at: u64,
    pub(in crate::layout) algorithm: Algorithm,
    /// Whether the family is little-endian, which a CRC's or a sum's
    /// value follows.
    pub(in crate::layout) little: bool,
}

impl Check {
    /// Where its value ends.
    pub(in crate::layout) fn end(&self) -> u64 {
        self.at + u64::from(self.algorithm.bits())
    }

    /// Its value's bits.
    pub(in crate::layout) fn value(&self) -> Range<u64> {
        self.at..self.end()
    }

    /// `bad_checksum`, if the value the packet's `bytes` hold for it isn't
    /// one its algorithm accepts for the bytes it covers. `None` too if
    /// the bytes end before its value does.
    fn failure(&self, bytes: &[u8]) -> Option<Failure> {
        let span = |from: u64, to: u64| -> Option<&[u8]> {
            bytes.get(usize::try_from(from / 8).ok()?..usize::try_from(to / 8).ok()?)
        };
        let stored = self.algorithm.read(span(self.at, self.end())?, self.little);
        let computed = self.algorithm.compute(span(self.from, self.at)?);
        (!self.algorithm.accepts(stored, computed)).then(|| Failure {
            path: self.path.clone(),
            problem: Problem::BadChecksum { stored, computed },
        })
    }
}

/// The checksums among `steps`: each verified before the walk, and each
/// the walk reaches.
pub(in crate::layout) fn checks(steps: &[Step]) -> impl Iterator<Item = &Check> {
    steps.iter().filter_map(|step| match step {
        Step::Verify(check) | Step::Checksum { check, .. } => Some(check),
        _ => None,
    })
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

/// What decoding `payload` does, by the steps its values take.
fn outcome(steps: &[Step], payload: &[u8]) -> Outcome {
    // usize is at most 64 bits on every target Rust supports.
    let mut bits = 8 * payload.len() as u64;
    let mut packet = payload;
    for step in steps {
        match step {
            Step::Packet { bytes, exact } => {
                let needed = usize::try_from(*bytes).unwrap_or(usize::MAX);
                if payload.len() < needed {
                    let problem = Problem::TooShort {
                        needed_bytes: *bytes,
                        given_bytes: payload.len(),
                        exact: *exact,
                    };
                    let path = String::new();
                    return Outcome::Fails(Failure { path, problem }, Some(Place::Length));
                }
                if *exact {
                    packet = &payload[..needed];
                    bits = 8 * needed as u64;
                }
            }
            Step::Verify(check) if check.end() <= bits => {
                if let Some(failure) = check.failure(packet) {
                    return Outcome::Fails(failure, None);
                }
            }
            Step::Checksum { check, .. } if check.end() > bits => {
                let problem = Problem::PastEnd {
                    needed_bits: check.end(),
                    available_bits: bits,
                    padding: false,
                };
                let path = check.path.clone();
                return Outcome::Fails(Failure { path, problem }, Some(Place::Checksum));
            }
            Step::Checksum {
                check,
                fixed: false,
            } => {
                if let Some(failure) = check.failure(packet) {
                    return Outcome::Fails(failure, None);
                }
            }
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
    /// This bit of the payload flipped, once the message is written.
    Flip(u64),
    /// A change made, each checksum at a static position then holding
    /// the valid message's value.
    Hidden(Box<Change>),
}

/// A step of a place in a record.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seg {
    Name(String),
    Index(usize),
}

fn damage(packet: &PacketLayout, time: &TimeSource, draft: Draft, picks: &Picks) -> Message {
    let valid = pack(packet, time, draft.clone());
    let plants = plants(packet, time, &draft, &valid.payload, &picks.detail);
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
        let (payload, Outcome::Fails(failure, _)) =
            apply(packet, time, &draft, &valid.payload, &changes)
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

/// The payload with `changes` made to the message, cut to the `valid`
/// payload's length unless a change cuts it, and what decoding it does.
/// What a flipped bit changes is known only for a bit nothing depends on
/// but checksums in the message as changed: one of a checksum's value, or
/// one the writer finds inert. Two changes to one value aren't both made,
/// so what decoding does isn't known then either.
fn apply(
    packet: &PacketLayout,
    time: &TimeSource,
    draft: &Draft,
    valid: &[u8],
    changes: &[&Change],
) -> (Vec<u8>, Outcome) {
    let mut values = draft.values.clone();
    let mut noise = draft.noise.clone();
    let mut garbled = None;
    let mut bytes = valid.len();
    let mut flips = Vec::new();
    let mut hidden = false;
    let mut held: Vec<&[Seg]> = Vec::new();
    for &change in changes {
        let change = match change {
            Change::Hidden(change) => {
                hidden = true;
                change.as_ref()
            }
            change => change,
        };
        match change {
            Change::Cut(cut) => bytes = bytes.min(*cut),
            Change::More(cut) => {
                let Some(room) = one_more(&packet.body.entries, &mut values) else {
                    return (noise, Outcome::Unknown);
                };
                noise.resize(noise.len() + room, 0);
                bytes = *cut;
            }
            Change::Hold(spot, _) if held.contains(&spot.as_slice()) => {
                return (noise, Outcome::Unknown);
            }
            Change::Garble(..) if garbled.is_some() => return (noise, Outcome::Unknown),
            Change::Hold(spot, raw) => match value_at(&mut values, spot) {
                Some(value) => {
                    *value = Raw::Uint(*raw);
                    held.push(spot);
                }
                None => return (noise, Outcome::Unknown),
            },
            Change::Garble(path, planted) => garbled = Some((path.clone(), planted.clone())),
            Change::Flip(bit) => flips.push(*bit),
            Change::Hidden(_) => return (noise, Outcome::Unknown),
        }
    }
    let written = write_message(packet, time, &values, &mut noise, &draft.timing, garbled);
    // A static checksum is where it is in either message.
    if hidden {
        for step in &written.steps {
            if let Step::Verify(check) = step {
                let span = bytes_of(check.value());
                let Some(kept) = valid.get(span.clone()) else {
                    return (noise, Outcome::Unknown);
                };
                noise[span].copy_from_slice(kept);
            }
        }
    }
    for bit in flips {
        let mut inert = written
            .inert
            .iter()
            .cloned()
            .chain(checks(&written.steps).map(Check::value));
        if !inert.any(|span| span.contains(&bit)) {
            return (noise, Outcome::Unknown);
        }
        noise[(bit / 8) as usize] ^= 0x80 >> (bit % 8);
    }
    noise.truncate(bytes);
    let outcome = outcome(&written.steps, &noise);
    (noise, outcome)
}

/// The bytes that whole-byte `bits` are.
fn bytes_of(bits: Range<u64>) -> Range<usize> {
    (bits.start / 8) as usize..(bits.end / 8) as usize
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

/// Every fault the message, whose payload is `valid`, can be given that
/// decoding fails with.
fn plants(
    packet: &PacketLayout,
    time: &TimeSource,
    draft: &Draft,
    valid: &[u8],
    detail: &Index,
) -> Vec<Plant> {
    let length = valid.len();
    let mut plants = Vec::new();
    let mut noise = draft.noise.clone();
    let written = write_message(packet, time, &draft.values, &mut noise, &draft.timing, None);
    // Every shorter length that fails.
    for cut in 1..length {
        if let Outcome::Fails(_, Some(place)) = outcome(&written.steps, &noise[..cut]) {
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
        let more = write_message(packet, time, &values, &mut noise, &draft.timing, None);
        let end = more.end.map_or(0, |end| end.div_ceil(8) as usize);
        for cut in length + 1..end {
            if let Outcome::Fails(_, Some(place)) = outcome(&more.steps, &noise[..cut]) {
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
        if let (_, Outcome::Fails(..)) = apply(packet, time, draft, valid, &[&change]) {
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
            if let (_, Outcome::Fails(..)) = apply(packet, time, draft, valid, &[&change]) {
                plants.push(Plant {
                    route: Route::Text(form),
                    change,
                    planted,
                });
            }
        }
    }
    // The count, length and string faults again, where checksums at
    // static positions keep the valid message's values: kept where one of
    // them is the error.
    let mut hidden = Vec::new();
    if !fixed_checks(&packet.body.entries).is_empty() {
        for plant in &plants {
            if !matches!(plant.change, Change::Hold(..) | Change::Garble(..)) {
                continue;
            }
            let change = Change::Hidden(Box::new(plant.change.clone()));
            let failure = match apply(packet, time, draft, valid, &[&change]) {
                (_, Outcome::Fails(failure, _)) => failure,
                _ => continue,
            };
            if let Problem::BadChecksum { .. } = failure.problem {
                hidden.push(Plant {
                    route: Route::Checksum(Tamper::Hidden),
                    change,
                    planted: format!("{}, under checksums verified first", plant.planted),
                });
            }
        }
    }
    plants.extend(hidden);
    // A bit of each checksum's value flipped, and a bit it covers that
    // nothing but checksums depends on.
    for step in &written.steps {
        let Step::Checksum { check, .. } = step else {
            continue;
        };
        let width = check.algorithm.bits() as usize;
        let mut flips = vec![(
            Tamper::Stored,
            check.at + detail.index(width) as u64,
            "of its value",
        )];
        let covered: Vec<u64> = written
            .inert
            .iter()
            .flat_map(|span| span.start.max(check.from)..span.end.min(check.at))
            .collect();
        if !covered.is_empty() {
            flips.push((
                Tamper::Covered,
                covered[detail.index(covered.len())],
                "it covers",
            ));
        }
        for (tamper, bit, what) in flips {
            let change = Change::Flip(bit);
            if let (_, Outcome::Fails(..)) = apply(packet, time, draft, valid, &[&change]) {
                plants.push(Plant {
                    route: Route::Checksum(tamper),
                    change,
                    planted: format!("{}: bit {bit}, {what}, flipped", check.path),
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
