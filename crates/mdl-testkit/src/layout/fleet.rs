//! Families of several sets and assets, and a stream of messages for them
//! in arrival order: routing by alias, version and bindings that change,
//! retire and reuse native ids over time (`spec/decoding.md` §3,
//! `spec/bindings.md` §4), identification by more than one key with the
//! more specific packet winning (§4), boot time tied to instants by
//! correlation (§8.2), and the errors a message can route to.
//!
//! A fleet is binary or JSON. A JSON fleet's sets are the JSON oracle's
//! (`json.rs`), its alias and version read at pointers or from metadata,
//! and a JSON payload is parsed where §3.3 says: by the first locator
//! that reads it, or as decoding starts.

use std::collections::BTreeMap;

use proptest::collection::{btree_set, vec};
use proptest::prelude::*;
use proptest::sample::select;
use serde_json::{Map, Value, json};

use super::json::{self, Doc, JsonSet};
use super::{
    BodySpec, Correlation, EventTime, FieldSpec, Message, PacketLayout, PacketShape, SECOND,
    TimeFields, TimeSource, Unusable, epoch, field_spec, instant_text, label, lay_out, message_at,
    time_fields, time_json, time_scale, to_utc, unit,
};

/// Families, their sets and bindings, and messages for them.
#[derive(Debug, Clone)]
pub struct Fleet {
    pub bundle: Value,
    /// The families, by name: `fam`, and with a mirror `fam2`, whose sets
    /// and bindings are `fam`'s, under the same asset names.
    pub families: Vec<String>,
    /// The assets, by name.
    pub assets: Vec<String>,
    /// The bindings file, in TOML.
    pub bindings: String,
    /// The sets a consumer hasn't loaded, by name.
    pub unloaded: Vec<String>,
    /// In arrival order.
    pub messages: Vec<Sent>,
}

/// A metadata value of a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Meta {
    Uint(u64),
    Int(i64),
    Text(String),
}

/// A message, where it routes and how it correlates.
#[derive(Debug, Clone)]
pub struct Sent {
    pub family: String,
    /// Its metadata, by name.
    pub metadata: Vec<(String, Meta)>,
    pub message: Message,
    /// The error it decodes to, by code, if it doesn't decode.
    pub error: Option<&'static str>,
    pub asset: String,
    pub set: String,
    /// Whether a correlator learns the asset's boot instant from it.
    pub teaches: bool,
    /// The instant a correlator gives it: its event time if absolute, its
    /// boot time after the asset's boot instant once one is known.
    pub absolute: Option<i128>,
    /// Whether its JSON payload was spoiled, so it isn't JSON MDL reads;
    /// with the error it would give unspoiled, to show what `invalid_json`
    /// comes before and after (§3.3).
    pub spoiled: bool,
    pub unspoiled_error: Option<&'static str>,
}

/// Where a family's messages say which asset sent them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Alias {
    Fixed,
    Bits,
    Metadata,
    /// The JSON value at `/sat`.
    Path,
}

/// Where they say which set decodes them, if they do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Version {
    None,
    Bits,
    Metadata,
    /// The JSON value at `/v`, compared exactly (§3.2).
    Path,
}

/// A native id: an integer, a negative one, the text of one (which reads
/// as that integer), or other text.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Native {
    Int(u64),
    Negative(i64),
    IntText(u64),
    /// A negative integer's text, which reads as that integer too.
    NegativeText(i64),
    Text(String),
}

/// A native id as a reader compares it: its integer, or its text.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Int(i128),
    Text(String),
}

/// The largest integer a bindings file writes as a number, 2^53 − 1.
const MAX_SAFE: u64 = (1 << 53) - 1;

impl Native {
    /// How a JSON message carries it at `/sat`: an integer as a number in
    /// any spelling, text as a string.
    fn doc(&self) -> Doc {
        match self {
            Native::Int(n) => Doc::Int(i128::from(*n)),
            Native::Negative(n) => Doc::Int(i128::from(*n)),
            Native::IntText(n) => Doc::Str(n.to_string()),
            Native::NegativeText(n) => Doc::Str(n.to_string()),
            Native::Text(text) => Doc::Str(text.clone()),
        }
    }

    /// How a message carries it.
    fn meta(&self) -> Meta {
        match self {
            Native::Int(n) => Meta::Uint(*n),
            Native::Negative(n) => Meta::Int(*n),
            Native::IntText(n) => Meta::Text(n.to_string()),
            Native::NegativeText(n) => Meta::Text(n.to_string()),
            Native::Text(text) => Meta::Text(text.clone()),
        }
    }

    /// How a bindings file writes it (`spec/bindings.md` §3): an integer
    /// within ±(2^53 − 1) as one, whatever its text; a wider one as its
    /// decimal string.
    fn toml(&self) -> String {
        match self {
            Native::Int(n) | Native::IntText(n) if *n > MAX_SAFE => format!("\"{n}\""),
            Native::Int(n) | Native::IntText(n) => n.to_string(),
            Native::Negative(n) | Native::NegativeText(n) => n.to_string(),
            Native::Text(text) => format!("\"{text}\""),
        }
    }

    fn key(&self) -> Key {
        match self {
            Native::Int(n) | Native::IntText(n) => Key::Int(i128::from(*n)),
            Native::Negative(n) | Native::NegativeText(n) => Key::Int(i128::from(*n)),
            Native::Text(text) => Key::Text(text.clone()),
        }
    }
}

/// What can go wrong with a message, by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    /// Its payload is cut to the id byte, before the bits locators.
    Short,
    /// It comes from a native id no binding names.
    Unbound,
    /// It names a version no set has.
    UnknownVersion,
    /// Its id is one no packet has.
    NoPacket,
    /// It gives a metadata name its family reads twice, which can't be
    /// read (§3.1).
    Twice,
    /// A JSON family's path locator, the alias's or the version's, finds
    /// what it can't read (§3.1).
    Unreadable,
}

/// How a family with a version locator writes versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Versions {
    /// Integers, sent as integers.
    Int,
    /// From metadata, integers sent as their text, which compares by value
    /// (`"2"` is 2); at a pointer, which compares exactly, set versions
    /// that are text spelling an integer (`"2"`), sent as that text.
    IntText,
    /// Text (`v2`), sent as text.
    Text,
}

#[derive(Debug, Clone)]
struct MessagePlan {
    asset: usize,
    /// The set it names, with a version locator.
    named: usize,
    choice: usize,
    /// A `sub` for the least specific packet.
    general_sub: u64,
    /// Milliseconds since the message before; a late message goes back.
    gap: i64,
    /// Its family: `fam`, or with a mirror `fam2`.
    mirror: bool,
    fault: Option<Fault>,
    /// A bindings change to arrive at exactly, or 1 ns before.
    snap: Option<(usize, bool)>,
    /// A JSON payload spoiled, so it isn't JSON MDL reads, whatever its
    /// fault.
    spoiled: bool,
    /// Picks the details of a JSON fault: which path locator can't be
    /// read and what it finds, which wrong version is sent, how a payload
    /// is spoiled.
    detail: usize,
}

#[derive(Debug, Clone)]
struct Plan {
    /// A JSON fleet, whose sets are `json_sets`; else binary, with `sets`.
    json: bool,
    json_sets: Vec<JsonSet>,
    little: bool,
    alias: Alias,
    version: Version,
    versions: Versions,
    /// The alias's and version's widths, for bits locators.
    node_bits: u32,
    version_bits: u32,
    /// An asset whose rows break §6's rule: naming a set when the family
    /// reads versions, or none when it doesn't.
    malformed: Option<usize>,
    /// A set the consumer hasn't loaded.
    unloaded: Option<usize>,
    /// Whether packets are told apart by an id and a `sub` byte.
    two_keys: bool,
    mirror: bool,
    sets: Vec<SetPlan>,
    /// Each asset's native id.
    ids: Vec<Native>,
    /// An asset rebound to another set: the asset, the set, and the
    /// milliseconds after the first message.
    switch: Option<(usize, usize, i64)>,
    /// An asset whose native id retires, and maybe goes to a new asset
    /// later: milliseconds after the first message for each.
    retire: Option<(usize, i64, Option<i64>)>,
    messages: Vec<MessagePlan>,
    /// The first message's ground time, in whole seconds.
    start: i128,
}

impl Plan {
    fn set_count(&self) -> usize {
        if self.json {
            self.json_sets.len()
        } else {
            self.sets.len()
        }
    }
}

#[derive(Debug, Clone)]
struct SetPlan {
    time: TimeSource,
    /// Each packet's fields and time fields.
    packets: Vec<(Vec<FieldSpec>, TimeFields)>,
    /// Whether there's a packet keyed by the id alone, less specific than
    /// the others.
    general: bool,
    correlation: Option<Correlation>,
}

/// A row of the bindings file: from an instant on, a native id is an asset
/// (with a set, without a version locator), or retired.
#[derive(Debug, Clone)]
struct Row {
    id: Native,
    from: i128,
    binding: Option<(usize, Option<usize>)>,
}

/// Families of one to three sets and assets and up to twelve messages.
pub fn fleet() -> BoxedStrategy<Fleet> {
    let shape = (
        (
            any::<bool>(),
            select(&[Alias::Fixed, Alias::Bits, Alias::Metadata][..]),
            select(&[Version::None, Version::Bits, Version::Metadata][..]),
            any::<bool>(),
            any::<bool>(),
        ),
        (
            1_usize..=3,
            1_usize..=3,
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
        ),
        (
            select(&[8_u32, 16, 32, 64][..]),
            select(&[8_u32, 16][..]),
            select(&[Versions::Int, Versions::IntText, Versions::Text][..]),
            any::<bool>(),
            any::<bool>(),
        ),
        proptest::bool::weighted(0.4),
    );
    shape
        .prop_flat_map(
            |(
                (little, alias, version, two_keys, mirror),
                (sets, assets, boot, text, faults),
                (node_bits, version_bits, versions, malformed, unloaded),
                json,
            )| {
                // A JSON family reads at a pointer where a binary one reads
                // bits.
                let (alias, version) = match (json, alias, version) {
                    (true, Alias::Bits, version) => (Alias::Path, version),
                    (_, alias, version) => (alias, version),
                };
                let version = match (json, version) {
                    (true, Version::Bits) => Version::Path,
                    (_, version) => version,
                };
                let assets = if alias == Alias::Fixed { 1 } else { assets };
                // Bits versions are integers.
                let versions = if matches!(version, Version::Metadata | Version::Path) {
                    versions
                } else {
                    Versions::Int
                };
                let ids = native_ids(alias, assets, text, node_bits);
                let switch = if version == Version::None && sets > 1 {
                    proptest::option::of((0..assets, 0..sets, 1_i64..100_000_000)).boxed()
                } else {
                    Just(None).boxed()
                };
                let retire = proptest::option::of((
                    0..assets,
                    1_i64..100_000_000,
                    proptest::option::of(1_i64..100_000_000),
                ))
                .prop_map(|retire| {
                    retire.map(|(asset, after, reuse)| (asset, after, reuse.map(|r| after + r)))
                });
                let malformed = if malformed && faults {
                    (0..assets).prop_map(Some).boxed()
                } else {
                    Just(None).boxed()
                };
                let unloaded = if unloaded && faults && sets > 1 {
                    (0..sets).prop_map(Some).boxed()
                } else {
                    Just(None).boxed()
                };
                let message = (
                    (0..assets, 0..sets, any::<usize>(), 4_u64..=255),
                    (-20_000_000_i64..50_000_000, any::<bool>()),
                    fault(alias, version, faults),
                    proptest::option::weighted(0.2, (0_usize..4, any::<bool>())),
                    (proptest::bool::weighted(0.25), any::<usize>()),
                )
                    .prop_map(
                        move |(
                            (asset, named, choice, general_sub),
                            (gap, other),
                            fault,
                            snap,
                            (spoiled, detail),
                        )| {
                            MessagePlan {
                                asset,
                                named,
                                choice,
                                general_sub,
                                gap,
                                mirror: mirror && other,
                                fault,
                                snap,
                                spoiled: spoiled && json && faults,
                                detail,
                            }
                        },
                    );
                let binary_sets = if json {
                    Just(Vec::new()).boxed()
                } else {
                    vec(set_plan(boot), sets).boxed()
                };
                let json_sets: Vec<BoxedStrategy<JsonSet>> = if json {
                    (0..sets)
                        .map(|set| json::fleet_set(format!("s{set}")))
                        .collect()
                } else {
                    Vec::new()
                };
                (
                    Just((json, little, alias, version, versions, two_keys, mirror)),
                    (Just((node_bits, version_bits)), malformed, unloaded),
                    (binary_sets, json_sets),
                    ids,
                    (switch, retire),
                    vec(message, 1..=12),
                    // Whole seconds; bindings instants add milliseconds.
                    (label((1995, 1, 1), (0, 0, 0), 0) / SECOND
                        ..label((2030, 1, 1), (0, 0, 0), 0) / SECOND)
                        .prop_map(|seconds| seconds * SECOND),
                )
            },
        )
        .prop_map(
            |(
                (json, little, alias, version, versions, two_keys, mirror),
                ((node_bits, version_bits), malformed, unloaded),
                (sets, json_sets),
                ids,
                (switch, retire),
                messages,
                start,
            )| Plan {
                json,
                json_sets,
                little,
                alias,
                version,
                versions,
                node_bits,
                version_bits,
                malformed,
                unloaded,
                two_keys,
                mirror,
                sets,
                ids,
                switch,
                retire,
                messages,
                start,
            },
        )
        .prop_flat_map(|plan| {
            let layouts = layouts(&plan);
            let rows = rows(&plan);
            let routed = route(&plan, &layouts, &rows);
            let messages: Vec<BoxedStrategy<Message>> = routed
                .iter()
                .map(|routed| {
                    if plan.json {
                        json::routed_message(
                            plan.json_sets[routed.set].clone(),
                            routed.packet,
                            routed.ground,
                            routed.members.clone(),
                        )
                    } else {
                        message_at(
                            layouts[routed.set][routed.packet].clone(),
                            plan.sets[routed.set].time.clone(),
                            routed.ground,
                            routed.header.clone(),
                        )
                    }
                })
                .collect();
            (
                Just(plan),
                Just(layouts),
                Just(rows),
                Just(routed),
                messages,
            )
        })
        .prop_map(|(plan, layouts, rows, routed, messages)| {
            assemble(&plan, &layouts, &rows, routed, messages)
        })
        .boxed()
}

/// Distinct native ids: integers a bits alias of `bits` bits holds (past
/// i64 for 64 bits); for a metadata alias, integers (some negative), or
/// with `text` integers written as text and other text (`007` isn't an
/// integer's one spelling, so it stays text).
fn native_ids(alias: Alias, assets: usize, text: bool, bits: u32) -> BoxedStrategy<Vec<Native>> {
    match alias {
        Alias::Fixed => Just(vec![Native::Text("A1".into())]).boxed(),
        Alias::Bits => {
            let most = if bits == 64 {
                u64::MAX
            } else {
                (1 << bits) - 1
            };
            btree_set(0..=most, assets)
                .prop_map(|ids| ids.into_iter().map(Native::Int).collect())
                .boxed()
        }
        Alias::Metadata | Alias::Path if text => btree_set(0_u64..1000, assets)
            .prop_flat_map(|numbers| {
                let count = numbers.len();
                (Just(numbers), vec(0_u8..6, count))
            })
            .prop_map(|(numbers, forms)| {
                // Text in an integer's one spelling within −2^63 to
                // 2^64 − 1 is that integer, negative or past 2^53 too;
                // other text, and digits past 2^64 − 1, stay text (§3.2).
                numbers
                    .into_iter()
                    .zip(forms)
                    .map(|(n, form)| match form {
                        0 => Native::IntText(n),
                        1 => Native::Text(format!("{n:03}x")),
                        2 => Native::Text(format!("00{n}")),
                        3 => Native::IntText((1 << 60) + n),
                        4 => Native::NegativeText(-1 - i64::try_from(n).unwrap_or(0)),
                        _ => Native::Text((u128::from(u64::MAX) + 1 + u128::from(n)).to_string()),
                    })
                    .collect()
            })
            .boxed(),
        Alias::Metadata => btree_set(-(1_i64 << 40)..1 << 40, assets)
            .prop_map(|ids| {
                ids.into_iter()
                    .map(|n| match u64::try_from(n) {
                        Ok(n) => Native::Int(n),
                        Err(_) => Native::Negative(n),
                    })
                    .collect()
            })
            .boxed(),
        // A JSON integer literal within 64 bits is exact, past 2^53 too.
        Alias::Path => {
            let id = prop_oneof![
                -(1_i128 << 40)..1 << 40,
                1_i128 << 53..=i128::from(u64::MAX),
            ];
            btree_set(id, assets)
                .prop_map(|ids| {
                    ids.into_iter()
                        .map(|n| match (u64::try_from(n), i64::try_from(n)) {
                            (Ok(n), _) => Native::Int(n),
                            (_, Ok(n)) => Native::Negative(n),
                            _ => Native::Int(0),
                        })
                        .collect()
                })
                .boxed()
        }
    }
}

/// A message's fault, now and then, of a kind the family can have.
fn fault(alias: Alias, version: Version, faults: bool) -> BoxedStrategy<Option<Fault>> {
    if !faults {
        return Just(None).boxed();
    }
    let mut kinds = vec![Fault::NoPacket];
    if alias == Alias::Bits || version == Version::Bits {
        kinds.push(Fault::Short);
    }
    if alias != Alias::Fixed {
        kinds.push(Fault::Unbound);
    }
    if version != Version::None {
        kinds.push(Fault::UnknownVersion);
    }
    if alias == Alias::Metadata || version == Version::Metadata {
        kinds.push(Fault::Twice);
    }
    if alias == Alias::Path || version == Version::Path {
        kinds.push(Fault::Unreadable);
    }
    proptest::option::weighted(0.25, select(kinds)).boxed()
}

/// One set: no time or boot time, one to three packets, maybe a least
/// specific one, and with boot time a correlation packet.
fn set_plan(boot: bool) -> BoxedStrategy<SetPlan> {
    let time = if boot {
        (unit(), unit(), epoch(), time_scale())
            .prop_map(|(unit, clock_unit, epoch, scale)| TimeSource::Boot {
                unit,
                clock_unit,
                epoch,
                scale,
            })
            .boxed()
    } else {
        Just(TimeSource::None).boxed()
    };
    time.prop_flat_map(|time| {
        let packets = vec((vec(field_spec(), 1..=4), time_fields(&time)), 1..=3);
        let correlation = match &time {
            TimeSource::Boot { clock_unit, .. } => {
                let widths: Vec<u32> = (8..=clock_unit.count_bits()).step_by(8).collect();
                (
                    select(widths),
                    btree_set(any::<u8>(), 0..=1),
                    btree_set(any::<u8>(), 0..=1),
                    any::<bool>(),
                )
                    .prop_map(|(clock_bits, clock, boot, units)| {
                        Some(Correlation {
                            clock_bits,
                            clock_invalid: clock.into_iter().map(u64::from).collect(),
                            boot_invalid: boot.into_iter().map(u64::from).collect(),
                            units,
                        })
                    })
                    .boxed()
            }
            _ => Just(None).boxed(),
        };
        (Just(time), packets, any::<bool>(), correlation)
    })
    .prop_map(|(time, packets, general, correlation)| SetPlan {
        time,
        packets,
        general,
        correlation,
    })
    .boxed()
}

/// The id a correlation packet is identified by.
const CORRELATION_ID: u8 = 200;

/// An id no packet has.
const NO_PACKET_ID: u8 = 250;

/// Each set's packets, laid out: the planned ones, then the least specific
/// one, then the correlation packet. With two keys, packet `k` is id
/// `k / 2 + 1` and sub `k % 2 + 1`, so packets share ids; the least specific
/// is id 1 alone, and takes every other sub.
fn layouts(plan: &Plan) -> Vec<Vec<PacketLayout>> {
    let header = header(plan);
    plan.sets
        .iter()
        .enumerate()
        .map(|(set, planned)| {
            let mut packets = Vec::new();
            let mut shapes: Vec<(PacketShape, TimeFields, Vec<FieldSpec>)> = Vec::new();
            for (index, (fields, timed)) in planned.packets.iter().enumerate() {
                let (id, identify) = if plan.two_keys {
                    let id = u8::try_from(index / 2 + 1).unwrap_or(1);
                    let sub = index % 2 + 1;
                    (id, json!({ "id": id, "sub": sub }))
                } else {
                    let id = u8::try_from(index + 1).unwrap_or(1);
                    (id, json!({ "id": id }))
                };
                let shape = PacketShape {
                    name: format!("P{index}"),
                    params: format!("s{set}p{index}"),
                    id,
                    identify,
                    header: header.clone(),
                };
                shapes.push((shape, timed.clone(), fields.clone()));
            }
            if plan.two_keys && planned.general {
                let shape = PacketShape {
                    name: "GENERAL".into(),
                    params: format!("s{set}g"),
                    id: 1,
                    identify: json!({ "id": 1 }),
                    header: header.clone(),
                };
                let fields = planned.packets[0].0.clone();
                shapes.push((shape, planned.packets[0].1.clone(), fields));
            }
            for (shape, timed, fields) in shapes {
                packets.push(lay_out(
                    shape,
                    plan.little,
                    &planned.time,
                    timed,
                    None,
                    BodySpec::fields(fields),
                ));
            }
            if let Some(correlation) = &planned.correlation {
                let shape = PacketShape {
                    name: "CORR".into(),
                    params: format!("s{set}c"),
                    id: CORRELATION_ID,
                    identify: json!({ "id": CORRELATION_ID }),
                    header: header.clone(),
                };
                packets.push(lay_out(
                    shape,
                    plan.little,
                    &planned.time,
                    TimeFields::default(),
                    Some(correlation.clone()),
                    BodySpec::fields(Vec::new()),
                ));
            }
            packets
        })
        .collect()
}

/// An asset's set when its family has no version locator.
/// The header fields after the id: the bits alias, the bits version, the
/// second identify key.
fn header(plan: &Plan) -> Vec<(&'static str, u32)> {
    let mut header = Vec::new();
    if plan.alias == Alias::Bits {
        header.push(("node", plan.node_bits));
    }
    if plan.version == Version::Bits {
        header.push(("ver", plan.version_bits));
    }
    if plan.two_keys {
        header.push(("sub", 8));
    }
    header
}

/// The set an asset's rows name: its own when the family has no version
/// locator, none when it has one, the other way round for a malformed
/// asset.
fn bound_set(plan: &Plan, asset: usize) -> Option<usize> {
    let named = plan.version == Version::None;
    (named != (plan.malformed == Some(asset))).then_some(asset % plan.set_count())
}

/// The bindings, as rows: each asset from 1980-01-06, then the rebinding,
/// the retirement and the reuse the plan has, at millisecond instants.
fn rows(plan: &Plan) -> Vec<Row> {
    let at = |millis: i64| plan.start + i128::from(millis) * 1_000_000;
    let mut rows: Vec<Row> = plan
        .ids
        .iter()
        .enumerate()
        .map(|(asset, id)| Row {
            id: id.clone(),
            from: label((1980, 1, 6), (0, 0, 0), 0),
            binding: Some((asset, bound_set(plan, asset))),
        })
        .collect();
    if let Some((asset, set, after)) = plan.switch {
        rows.push(Row {
            id: plan.ids[asset].clone(),
            from: at(after),
            binding: Some((asset, Some(set))),
        });
    }
    if let Some((asset, after, reuse)) = plan.retire {
        rows.push(Row {
            id: plan.ids[asset].clone(),
            from: at(after),
            binding: None,
        });
        if let Some(reuse) = reuse {
            let new = plan.ids.len();
            rows.push(Row {
                id: plan.ids[asset].clone(),
                from: at(reuse),
                binding: Some((new, bound_set(plan, new))),
            });
        }
    }
    rows.sort_by_key(|row| (row.id.key(), row.from));
    // Two rows of one id at one instant would be ambiguous; the later one
    // in the plan moves a millisecond on.
    for k in 1..rows.len() {
        if rows[k].id.key() == rows[k - 1].id.key() && rows[k].from <= rows[k - 1].from {
            rows[k].from = rows[k - 1].from + 1_000_000;
        }
    }
    rows
}

/// The asset and set a native id is bound to at an instant, if it is: by
/// the last row of that id from at or before it.
fn resolve(rows: &[Row], id: &Native, at: i128) -> Option<(usize, Option<usize>)> {
    rows.iter()
        .rev()
        .find(|row| row.id.key() == id.key() && row.from <= at)
        .and_then(|row| row.binding)
}

/// Where a message goes, what its header and metadata hold, and the error
/// it routes to, if any.
#[derive(Debug, Clone)]
struct Routed {
    family: usize,
    asset: usize,
    set: usize,
    packet: usize,
    ground: i128,
    header: Vec<u64>,
    /// A JSON message's members the fleet writes (`Some`) or takes out
    /// (`None`): the alias, the version, a `kind` no packet has.
    members: Vec<(String, Option<Doc>)>,
    metadata: Vec<(String, Meta)>,
    fault: Option<Fault>,
    spoiled: bool,
    detail: usize,
    error: Option<&'static str>,
    unspoiled_error: Option<&'static str>,
}

/// Each message's route, in the order `spec/decoding.md` §3.3 checks it:
/// the alias locator, the asset bound at its ground time, the version
/// locator and the set, then identification (§4). A JSON payload that
/// isn't JSON MDL reads is `invalid_json` where it's first parsed: by a
/// path locator reading it, else as decoding starts, before
/// identification.
fn route(plan: &Plan, layouts: &[Vec<PacketLayout>], rows: &[Row]) -> Vec<Routed> {
    let mut ground = plan.start;
    let changes: Vec<i128> = rows
        .iter()
        .map(|row| row.from)
        .filter(|from| *from >= plan.start)
        .collect();
    plan.messages
        .iter()
        .map(|message| {
            ground += i128::from(message.gap) * 1_000_000;
            let at = match message.snap {
                Some((k, before)) if !changes.is_empty() => {
                    changes[k % changes.len()] - i128::from(before)
                }
                _ => ground,
            };
            let id = match message.fault {
                Some(Fault::Unbound) => match (plan.alias, &plan.ids[0]) {
                    (Alias::Bits, _) => Native::Int(unbound_id(plan)),
                    (_, Native::Text(_) | Native::IntText(_) | Native::NegativeText(_)) => {
                        Native::Text("unbound".into())
                    }
                    _ => Native::Int(1 << 50),
                },
                _ => plan.ids[message.asset].clone(),
            };
            let bound = resolve(rows, &id, at);
            let versioned = plan.version != Version::None;
            let asset = bound.map_or(message.asset, |(asset, _)| asset);
            let set = match bound {
                _ if versioned => message.named,
                Some((_, Some(set))) => set,
                // Laid out as the asset's own set would be.
                _ => message.asset % plan.set_count(),
            };
            let packets = if plan.json {
                plan.json_sets[set].packet_count()
            } else {
                layouts[set].len()
            };
            let packet = message.choice % packets;
            // A JSON set's catch-all is the message of a kind no other packet
            // has, so no packet matching takes a set without one.
            let no_packet = message.fault == Some(Fault::NoPacket)
                && (!plan.json
                    || (!plan.json_sets[set].has_catch_all() && plan.json_sets[set].keyed(packet)));
            let unknown_version = message.fault == Some(Fault::UnknownVersion);
            let version = if unknown_version { 99 } else { set as u64 + 1 };
            // A JSON fault's details, from `detail`.
            let unusable = Unusable::ALL[message.detail % Unusable::ALL.len()];
            let paths = (plan.alias == Alias::Path, plan.version == Version::Path);
            let unreadable_version = match paths {
                (true, true) => message.detail / 8 % 2 == 1,
                (false, true) => true,
                _ => false,
            };
            let other_type = message.detail / 16 % 2 == 1;
            let short = message.fault == Some(Fault::Short);
            let twice = message.fault == Some(Fault::Twice);
            let unreadable = message.fault == Some(Fault::Unreadable);
            let alias_unreadable = (short && plan.alias == Alias::Bits)
                || (twice && plan.alias == Alias::Metadata)
                || (unreadable && paths.0 && !unreadable_version);
            let version_unreadable = (short && plan.version == Version::Bits)
                || (twice && plan.alias != Alias::Metadata && plan.version == Version::Metadata)
                || (unreadable && unreadable_version);
            let error_of = |spoiled: bool| -> Option<&'static str> {
                if spoiled && plan.alias == Alias::Path {
                    return Some("invalid_json");
                }
                if alias_unreadable {
                    return Some("unreadable_locator");
                }
                match bound {
                    None => return Some("unknown_asset"),
                    Some((_, Some(_))) if versioned => return Some("unexpected_set"),
                    _ => {}
                }
                if spoiled && plan.version == Version::Path {
                    return Some("invalid_json");
                }
                if versioned {
                    if version_unreadable {
                        return Some("unreadable_locator");
                    }
                    // A version no loaded set has.
                    if unknown_version || plan.unloaded == Some(message.named) {
                        return Some("unknown_version");
                    }
                } else {
                    match bound {
                        Some((_, None)) => return Some("missing_set"),
                        Some((_, Some(set))) if plan.unloaded == Some(set) => {
                            return Some("unknown_set");
                        }
                        _ => {}
                    }
                }
                if spoiled {
                    return Some("invalid_json");
                }
                no_packet.then_some("no_packet")
            };
            let mut header = Vec::new();
            let mut members = Vec::new();
            if plan.json {
                if plan.alias == Alias::Path {
                    let value = if alias_unreadable {
                        unusable.value()
                    } else {
                        Some(id.doc())
                    };
                    members.push(("sat".to_string(), value));
                }
                if plan.version == Version::Path {
                    let value = match plan.versions {
                        _ if version_unreadable => unusable.value(),
                        // The set's version, but the other JSON type: a path
                        // compares exactly (§3.2).
                        Versions::Int if unknown_version && other_type => {
                            Some(Doc::Str((set + 1).to_string()))
                        }
                        Versions::IntText if unknown_version && other_type => {
                            Some(Doc::Int(set as i128 + 1))
                        }
                        // The set's version with a leading zero: text no set
                        // has, compared exactly.
                        Versions::IntText if unknown_version => {
                            Some(Doc::Str(format!("0{}", set + 1)))
                        }
                        Versions::Int => Some(Doc::Int(i128::from(version))),
                        Versions::IntText => Some(Doc::Str(version.to_string())),
                        Versions::Text => Some(Doc::Str(format!("v{version}"))),
                    };
                    members.push(("v".to_string(), value));
                }
                if no_packet {
                    members.push(("kind".to_string(), Some(Doc::Str("NONE".into()))));
                }
            } else {
                let layout = &layouts[set][packet];
                if plan.alias == Alias::Bits {
                    header.push(match id.key() {
                        Key::Int(n) => u64::try_from(n).unwrap_or(0),
                        Key::Text(_) => 0,
                    });
                }
                if plan.version == Version::Bits {
                    header.push(version);
                }
                if plan.two_keys {
                    // The least specific packet takes subs no other one has.
                    let sub = match layout.identify.get("sub").and_then(Value::as_u64) {
                        Some(sub) => sub,
                        None if layout.name == "GENERAL" => message.general_sub,
                        None => 0,
                    };
                    header.push(sub);
                }
            }
            let mut metadata = Vec::new();
            if plan.alias == Alias::Metadata {
                metadata.push(("node".to_string(), id.meta()));
                if twice {
                    metadata.push(("node".to_string(), id.meta()));
                }
            }
            if plan.version == Version::Metadata {
                let value = match plan.versions {
                    Versions::Int => Meta::Uint(version),
                    // "02" isn't 2's one spelling, so it's text no set has.
                    Versions::IntText if unknown_version => Meta::Text(format!("0{}", set + 1)),
                    Versions::IntText => Meta::Text(version.to_string()),
                    Versions::Text => Meta::Text(format!("v{version}")),
                };
                metadata.push(("ver".to_string(), value.clone()));
                if twice && plan.alias != Alias::Metadata {
                    metadata.push(("ver".to_string(), value));
                }
            }
            Routed {
                family: usize::from(message.mirror),
                asset,
                set,
                packet,
                ground: at,
                header,
                members,
                metadata,
                fault: message.fault,
                spoiled: message.spoiled,
                detail: message.detail,
                error: error_of(message.spoiled),
                unspoiled_error: error_of(false),
            }
        })
        .collect()
}

/// A small integer no asset's native id is, which any bits alias holds.
fn unbound_id(plan: &Plan) -> u64 {
    (0..=255)
        .find(|n| !plan.ids.contains(&Native::Int(*n)))
        .unwrap_or(0)
}

fn family_name(family: usize) -> String {
    if family == 0 {
        "fam".to_string()
    } else {
        "fam2".to_string()
    }
}

fn asset_name(asset: usize) -> String {
    format!("AST{asset}")
}

fn set_name(family: usize, set: usize) -> String {
    format!("{}_v{}", family_name(family), set + 1)
}

fn assemble(
    plan: &Plan,
    layouts: &[Vec<PacketLayout>],
    rows: &[Row],
    routed: Vec<Routed>,
    messages: Vec<Message>,
) -> Fleet {
    let mut params = Map::new();
    for packet in layouts.iter().flatten() {
        params.extend(packet.body.params());
    }
    for set in &plan.json_sets {
        params.extend(json::set_params(set));
    }
    let families = if plan.mirror { 2 } else { 1 };
    let mut sets = Vec::new();
    for family in 0..families {
        let bodies: Vec<Value> = if plan.json {
            plan.json_sets
                .iter()
                .enumerate()
                .map(|(set, json_set)| {
                    json::set_json(json_set, &set_name(family, set), &family_name(family))
                })
                .collect()
        } else {
            let header: u64 = header(plan).iter().map(|(_, bits)| u64::from(*bits)).sum();
            layouts
                .iter()
                .zip(&plan.sets)
                .enumerate()
                .map(|(set, (packets, planned))| {
                    let packets: Vec<Value> = packets.iter().map(PacketLayout::json).collect();
                    let mut json = json!({ "name": set_name(family, set),
                                           "family": family_name(family), "packets": packets });
                    if let Some(time) = time_json(&planned.time, 8 + header) {
                        json["time"] = time;
                    }
                    json
                })
                .collect()
        };
        for (set, mut json) in bodies.into_iter().enumerate() {
            if plan.version != Version::None {
                json["version"] = match plan.versions {
                    Versions::Text => json!(format!("v{}", set + 1)),
                    // At a pointer, text spelling an integer is its own
                    // version, which only that text matches (§3.2).
                    Versions::IntText if plan.version == Version::Path => {
                        json!((set + 1).to_string())
                    }
                    Versions::Int | Versions::IntText => json!(set + 1),
                };
            }
            sets.push(json);
        }
    }
    let bundle = json!({
        "mdl_version": "2.0",
        "catalog": { "params": params },
        "families": (0..families).map(|family| family_json(plan, family)).collect::<Vec<_>>(),
        "sets": sets
    });
    let assets: Vec<String> = (0..=plan.ids.len()).map(asset_name).collect();
    let mut sent = Vec::new();
    let mut booted: BTreeMap<(usize, usize), i128> = BTreeMap::new();
    for (routed, mut message) in routed.into_iter().zip(messages) {
        match routed.fault {
            Some(Fault::Short) => message.payload.truncate(1),
            Some(Fault::NoPacket) if !plan.json => message.payload[0] = NO_PACKET_ID,
            _ => {}
        }
        if routed.spoiled {
            spoil(&mut message.payload, routed.detail / 32);
        }
        // Once routed and decoded, an event time that can't be read is the
        // message's error (§3.3, §8).
        let unreadable_time = message.event_time == EventTime::Invalid;
        let error = routed.error.or(unreadable_time.then_some("invalid_time"));
        let unspoiled_error = routed
            .unspoiled_error
            .or(unreadable_time.then_some("invalid_time"));
        let teaches = error.is_none() && correlate(plan, &routed, &message, &mut booted);
        let absolute = match (&error, &message.event_time) {
            (Some(_), _) => None,
            (None, EventTime::At(at)) => Some(*at),
            (None, EventTime::SinceBoot { value, .. }) => {
                match plan.sets.get(routed.set).map(|set| &set.time) {
                    Some(TimeSource::Boot { unit, .. }) => booted
                        .get(&(routed.family, routed.asset))
                        .map(|boot| boot + i128::from(*value) * unit.nanos()),
                    _ => None,
                }
            }
            _ => None,
        };
        sent.push(Sent {
            family: family_name(routed.family),
            metadata: routed.metadata,
            message,
            error,
            asset: asset_name(routed.asset),
            set: set_name(routed.family, routed.set),
            teaches,
            absolute,
            spoiled: routed.spoiled,
            unspoiled_error,
        });
    }
    Fleet {
        bundle,
        families: (0..families).map(family_name).collect(),
        assets,
        bindings: bindings(rows, families),
        unloaded: plan
            .unloaded
            .map(|set| (0..families).map(|family| set_name(family, set)).collect())
            .unwrap_or_default(),
        messages: sent,
    }
}

fn family_json(plan: &Plan, family: usize) -> Value {
    let mut json = if plan.json {
        json!({ "name": family_name(family), "encoding": "json" })
    } else {
        json!({
            "name": family_name(family),
            "encoding": "binary",
            "byte_order": if plan.little { "little" } else { "big" },
        })
    };
    let mut metadata = Vec::new();
    json["alias"] = match plan.alias {
        Alias::Fixed => json!({ "kind": "fixed", "value": "A1" }),
        Alias::Bits => json!({ "kind": "bits", "offset_bits": 8, "size_bits": plan.node_bits }),
        Alias::Metadata => {
            metadata.push("node");
            json!({ "kind": "metadata", "name": "node" })
        }
        Alias::Path => json!({ "kind": "path", "pointer": "/sat" }),
    };
    match plan.version {
        Version::None => {}
        Version::Bits => {
            let offset = if plan.alias == Alias::Bits {
                8 + plan.node_bits
            } else {
                8
            };
            json["version"] =
                json!({ "kind": "bits", "offset_bits": offset, "size_bits": plan.version_bits });
        }
        Version::Metadata => {
            metadata.push("ver");
            json["version"] = json!({ "kind": "metadata", "name": "ver" });
        }
        Version::Path => json["version"] = json!({ "kind": "path", "pointer": "/v" }),
    }
    if !metadata.is_empty() {
        json["metadata"] = json!(metadata);
    }
    json
}

/// Whether a correlator learns from the message (§8.2): a boot set's
/// correlation packet whose clocks both read good, giving the asset's boot
/// instant in its family as the absolute clock's instant less the boot
/// clock.
fn correlate(
    plan: &Plan,
    routed: &Routed,
    message: &Message,
    booted: &mut BTreeMap<(usize, usize), i128>,
) -> bool {
    // JSON sets have no boot time here.
    let Some(set) = plan.sets.get(routed.set).filter(|_| !plan.json) else {
        return false;
    };
    let TimeSource::Boot {
        unit,
        clock_unit,
        epoch,
        scale,
    } = &set.time
    else {
        return false;
    };
    let Some(correlation) = &set.correlation else {
        return false;
    };
    if message.packet != "CORR" {
        return false;
    }
    let value = |name: &str| {
        message.record.iter().find_map(|(field, raw)| match raw {
            super::Raw::Uint(n) if field == name => Some(*n),
            _ => None,
        })
    };
    let (Some(clock), Some(boot)) = (value("clock"), value("boot")) else {
        return false;
    };
    if correlation.clock_invalid.contains(&clock) || correlation.boot_invalid.contains(&boot) {
        return false;
    }
    let instant = to_utc(epoch.label + i128::from(clock) * clock_unit.nanos(), *scale);
    booted.insert(
        (routed.family, routed.asset),
        instant - i128::from(boot) * unit.nanos(),
    );
    true
}

/// A JSON payload made into one that isn't JSON MDL reads (§2.3), one of
/// four ways by `way`: cut before its closing brace, a token after the
/// document, a byte order mark before it, a byte that isn't UTF-8 inside
/// it.
fn spoil(payload: &mut Vec<u8>, way: usize) {
    let inside = payload
        .iter()
        .position(|&b| b == b'{')
        .map_or(0, |open| open + 1);
    match way % 4 {
        0 => {
            let close = payload.iter().rposition(|&b| b == b'}').unwrap_or(0);
            payload.truncate(close);
        }
        1 => payload.extend_from_slice(b" x"),
        2 => {
            payload.splice(0..0, [0xEF, 0xBB, 0xBF]);
        }
        _ => {
            payload.insert(inside, 0xFF);
        }
    }
}

/// The bindings file: every row, for each family.
fn bindings(rows: &[Row], families: usize) -> String {
    let mut text = String::from("mdl_version = \"2.0\"\n");
    for family in 0..families {
        for row in rows {
            text.push_str(&format!(
                "\n[[binding]]\nfamily = \"{}\"\nnative_id = {}\n",
                family_name(family),
                row.id.toml()
            ));
            match row.binding {
                Some((asset, set)) => {
                    text.push_str(&format!("asset = \"{}\"\n", asset_name(asset)));
                    if let Some(set) = set {
                        text.push_str(&format!("set = \"{}\"\n", set_name(family, set)));
                    }
                }
                None => text.push_str("retired = true\n"),
            }
            text.push_str(&format!(
                "effective_from = \"{}\"\n",
                instant_text(row.from)
            ));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use proptest::strategy::{Strategy, ValueTree};
    use proptest::test_runner::TestRunner;
    use serde_json::Value;

    use super::super::time::civil_from_days;
    use super::{EventTime, Meta, fleet, instant_text, label};

    /// Whether a path locator reads `value`: a string, or a number that is
    /// an integer by §2.3 (`17.0` is 17).
    fn readable(value: Option<&Value>) -> bool {
        value.is_some_and(|value| {
            value.is_string()
                || value.is_u64()
                || value.is_i64()
                || value
                    .as_f64()
                    .is_some_and(|x| x.fract() == 0.0 && x.abs() <= 9_007_199_254_740_991.0)
        })
    }

    /// How text a locator reads compares (§3.2): as an integer, when it's
    /// one's one spelling between −2^63 and 2^64 − 1, else as text.
    fn text_kind(text: &str) -> &'static str {
        let spelled = |n: String| n == text;
        match (
            text.parse::<i64>(),
            text.parse::<u64>(),
            text.parse::<u128>(),
        ) {
            (Ok(n), _, _) if n < 0 && spelled(n.to_string()) => "negative text",
            (_, Ok(n), _) if spelled(n.to_string()) && n > 1 << 53 => "wide text",
            (_, Ok(n), _) if spelled(n.to_string()) => "integer text",
            (_, _, Ok(n)) if spelled(n.to_string()) => "text past 64 bits",
            _ => "text",
        }
    }

    /// Generated fleets reach every routing and correlation case.
    #[test]
    fn fleets_reach_every_case() {
        let mut runner = TestRunner::deterministic();
        let strategy = fleet();
        let mut seen = BTreeSet::new();
        for _ in 0..2500 {
            let case = strategy.new_tree(&mut runner).unwrap().current();
            let family = &case.bundle["families"][0];
            let json = family["encoding"] == "json";
            if json {
                seen.insert("encoding:json".to_string());
            }
            let set_version = &case.bundle["sets"][0]["version"];
            let path_version = family["version"]["kind"] == "path";
            if path_version {
                seen.insert(
                    match set_version {
                        Value::String(text) if text.parse::<u64>().is_ok() => {
                            "path version:integer text"
                        }
                        Value::String(_) => "path version:text",
                        _ => "path version:integer",
                    }
                    .to_string(),
                );
            }
            seen.insert(format!(
                "alias:{}",
                family["alias"]["kind"].as_str().unwrap_or("")
            ));
            seen.insert(format!(
                "version:{}",
                family["version"]["kind"].as_str().unwrap_or("none")
            ));
            if case.families.len() > 1 {
                seen.insert("mirror".to_string());
            }
            if family["alias"]["size_bits"] == 64 {
                seen.insert("alias:64".to_string());
            }
            if case.bindings.contains("native_id = \"1") || case.bindings.contains("native_id = -")
            {
                seen.insert("wide or negative id".to_string());
            }
            if case.bindings.contains("retired = true") {
                seen.insert("retired".to_string());
            }
            if case.bindings.contains(".") && case.bindings.contains("effective_from = \"20") {
                seen.insert("fractional".to_string());
            }
            let mut previous = i128::MIN;
            for sent in &case.messages {
                // §3.3's order around the parse: which of a spoiled payload's
                // `invalid_json` and the error it would give otherwise comes
                // first.
                if sent.spoiled {
                    // Where the payload is parsed: by the alias, the version,
                    // or as decoding starts.
                    let point = if family["alias"]["kind"] == "path" {
                        "the alias"
                    } else if path_version {
                        "the version"
                    } else {
                        "decoding"
                    };
                    seen.insert(match sent.error {
                        Some("invalid_json") => format!(
                            "invalid_json at {point} over {}",
                            sent.unspoiled_error.unwrap_or("nothing")
                        ),
                        Some(code) => format!("{code} over invalid_json at {point}"),
                        None => "spoiled, decoded".to_string(),
                    });
                }
                let document: Option<Value> = serde_json::from_slice(&sent.message.payload).ok();
                let member = |name: &str| document.as_ref().and_then(|doc| doc.get(name));
                if json && sent.error == Some("unreadable_locator") && !sent.spoiled {
                    if family["alias"]["kind"] == "path" && !readable(member("sat")) {
                        seen.insert("unreadable:alias at a pointer".to_string());
                    } else if path_version && !readable(member("v")) {
                        seen.insert("unreadable:version at a pointer".to_string());
                    }
                }
                // The set's version in the other JSON type, or its text with a
                // leading zero: a path compares exactly (§3.2). A set that
                // isn't loaded is no version's.
                let loaded = !case.unloaded.contains(&sent.set);
                if path_version && loaded && sent.error == Some("unknown_version") {
                    match (member("v"), set_version) {
                        (Some(Value::String(_)), Value::Number(_)) => {
                            seen.insert("exact:text for an integer version".to_string());
                        }
                        (Some(Value::Number(_)), Value::String(_)) => {
                            seen.insert("exact:integer for a text version".to_string());
                        }
                        (Some(Value::String(text)), Value::String(_)) if text.starts_with('0') => {
                            seen.insert("exact:a leading zero".to_string());
                        }
                        _ => {}
                    }
                }
                if sent.error.is_none() && path_version && member("v").is_some_and(|v| v.is_f64()) {
                    seen.insert("path version:spelled as a double".to_string());
                }
                if sent.error.is_none() && family["alias"]["kind"] == "path" {
                    match member("sat") {
                        Some(Value::String(text)) => {
                            seen.insert(format!("{}:sat", text_kind(text)));
                        }
                        Some(Value::Number(n)) if n.is_i64() && !n.is_u64() => {
                            seen.insert("negative:sat".to_string());
                        }
                        Some(Value::Number(n)) if n.as_u64().is_some_and(|n| n > 1 << 53) => {
                            seen.insert("wide:sat".to_string());
                        }
                        _ => {}
                    }
                }
                if let Some(code) = sent.error {
                    seen.insert(format!("error:{code}"));
                    continue;
                }
                if sent.message.received < previous {
                    seen.insert("late".to_string());
                }
                previous = sent.message.received;
                if sent.asset == format!("AST{}", case.assets.len() - 1) {
                    seen.insert("reused".to_string());
                }
                if sent.message.packet == "GENERAL" {
                    seen.insert("general".to_string());
                }
                if sent.message.packet == "CORR" {
                    seen.insert(format!("teaches:{}", sent.teaches));
                }
                if let EventTime::SinceBoot { .. } = sent.message.event_time {
                    seen.insert(format!("correlated:{}", sent.absolute.is_some()));
                }
                for (name, value) in &sent.metadata {
                    match value {
                        Meta::Text(text) => {
                            seen.insert(format!("{}:{name}", text_kind(text)));
                        }
                        Meta::Int(_) => {
                            seen.insert(format!("negative:{name}"));
                        }
                        Meta::Uint(_) => {}
                    }
                }
            }
        }
        let required = [
            "alias:fixed",
            "alias:bits",
            "alias:metadata",
            "version:none",
            "version:bits",
            "version:metadata",
            "mirror",
            "retired",
            "fractional",
            "late",
            "reused",
            "general",
            "teaches:true",
            "teaches:false",
            "correlated:true",
            "correlated:false",
            "text:node",
            "text:ver",
            "integer text:node",
            "integer text:ver",
            "negative text:node",
            "wide text:node",
            "text past 64 bits:node",
            "negative:node",
            "alias:64",
            "wide or negative id",
            "error:unreadable_locator",
            "error:unknown_asset",
            "error:unknown_version",
            "error:unexpected_set",
            "error:missing_set",
            "error:unknown_set",
            "error:no_packet",
            "encoding:json",
            "alias:path",
            "version:path",
            "path version:integer",
            "path version:integer text",
            "path version:text",
            "integer text:sat",
            "text:sat",
            "negative text:sat",
            "wide text:sat",
            "text past 64 bits:sat",
            "negative:sat",
            "wide:sat",
            "path version:spelled as a double",
            "unreadable:alias at a pointer",
            "unreadable:version at a pointer",
            "exact:text for an integer version",
            "exact:integer for a text version",
            "exact:a leading zero",
            "error:invalid_json",
            "error:invalid_time",
            // §3.3's order around each place the payload is parsed.
            "invalid_json at the alias over nothing",
            "invalid_json at the alias over unknown_asset",
            "invalid_json at the alias over unreadable_locator",
            "unknown_asset over invalid_json at the version",
            "unexpected_set over invalid_json at the version",
            "invalid_json at the version over unreadable_locator",
            "invalid_json at the version over unknown_version",
            "unknown_asset over invalid_json at decoding",
            "unexpected_set over invalid_json at decoding",
            "unreadable_locator over invalid_json at decoding",
            "unknown_version over invalid_json at decoding",
            "missing_set over invalid_json at decoding",
            "unknown_set over invalid_json at decoding",
            "invalid_json at decoding over nothing",
            "invalid_json at decoding over no_packet",
            "invalid_json at decoding over invalid_time",
        ];
        let missing: Vec<&str> = required
            .into_iter()
            .filter(|case| !seen.contains(*case))
            .collect();
        assert!(missing.is_empty(), "no fleet reaches {missing:?}: {seen:?}");
    }

    #[test]
    fn dates_round_trip_through_days() {
        for (date, days) in [
            ((1970, 1, 1), 0),
            ((2000, 2, 29), 11_016),
            ((1969, 12, 31), -1),
        ] {
            assert_eq!(civil_from_days(days), date);
            assert_eq!(
                label(date, (0, 0, 0), 0),
                i128::from(days) * 86_400 * 1_000_000_000
            );
        }
        assert_eq!(
            instant_text(label((2016, 12, 31), (23, 59, 59), 0)),
            "2016-12-31T23:59:59Z"
        );
        assert_eq!(
            instant_text(label((2016, 12, 31), (23, 59, 59), 250_000_000)),
            "2016-12-31T23:59:59.25Z"
        );
    }
}
