//! Families of several sets and assets, and a stream of messages for them
//! in arrival order: routing by alias, version and bindings that change,
//! retire and reuse native ids over time (`spec/decoding.md` §3,
//! `spec/bindings.md` §4), identification by more than one key with the
//! more specific packet winning (§4), boot time tied to instants by
//! correlation (§8.2), and the errors a message can route to.

use std::collections::BTreeMap;

use proptest::collection::{btree_set, vec};
use proptest::prelude::*;
use proptest::sample::select;
use serde_json::{Map, Value, json};

use super::{
    Correlation, EventTime, FieldSpec, Message, PacketLayout, PacketShape, SECOND, TimeFields,
    TimeSource, epoch, field_spec, instant_text, label, lay_out, message_at, param_json,
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
}

/// Where a family's messages say which asset sent them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Alias {
    Fixed,
    Bits,
    Metadata,
}

/// Where they say which set decodes them, if they do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Version {
    None,
    Bits,
    Metadata,
}

/// A native id: an integer, a negative one, the text of one (which reads
/// as that integer), or other text.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Native {
    Int(u64),
    Negative(i64),
    IntText(u64),
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
    /// How a message carries it.
    fn meta(&self) -> Meta {
        match self {
            Native::Int(n) => Meta::Uint(*n),
            Native::Negative(n) => Meta::Int(*n),
            Native::IntText(n) => Meta::Text(n.to_string()),
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
            Native::Negative(n) => n.to_string(),
            Native::Text(text) => format!("\"{text}\""),
        }
    }

    fn key(&self) -> Key {
        match self {
            Native::Int(n) | Native::IntText(n) => Key::Int(i128::from(*n)),
            Native::Negative(n) => Key::Int(i128::from(*n)),
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
}

/// How a family with a version locator writes versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Versions {
    /// Integers, sent as integers.
    Int,
    /// Integers, sent from metadata as their text, which compares by value
    /// (`"2"` is 2).
    IntText,
    /// Text (`v2`), sent from metadata as text.
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
}

#[derive(Debug, Clone)]
struct Plan {
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
    );
    shape
        .prop_flat_map(
            |(
                (little, alias, version, two_keys, mirror),
                (sets, assets, boot, text, faults),
                (node_bits, version_bits, versions, malformed, unloaded),
            )| {
                let assets = if alias == Alias::Fixed { 1 } else { assets };
                // Text versions come from metadata only.
                let versions = if version == Version::Metadata {
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
                )
                    .prop_map(
                        move |((asset, named, choice, general_sub), (gap, other), fault, snap)| {
                            MessagePlan {
                                asset,
                                named,
                                choice,
                                general_sub,
                                gap,
                                mirror: mirror && other,
                                fault,
                                snap,
                            }
                        },
                    );
                (
                    Just((little, alias, version, versions, two_keys, mirror)),
                    (Just((node_bits, version_bits)), malformed, unloaded),
                    vec(set_plan(boot), sets),
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
                (little, alias, version, versions, two_keys, mirror),
                ((node_bits, version_bits), malformed, unloaded),
                sets,
                ids,
                (switch, retire),
                messages,
                start,
            )| Plan {
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
                    message_at(
                        layouts[routed.set][routed.packet].clone(),
                        plan.sets[routed.set].time.clone(),
                        routed.ground,
                        routed.header.clone(),
                    )
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
        Alias::Metadata if text => btree_set(0_u64..1000, assets)
            .prop_flat_map(|numbers| {
                let count = numbers.len();
                (Just(numbers), vec(0_u8..3, count))
            })
            .prop_map(|(numbers, forms)| {
                numbers
                    .into_iter()
                    .zip(forms)
                    .map(|(n, form)| match form {
                        0 => Native::IntText(n),
                        1 => Native::Text(format!("{n:03}x")),
                        _ => Native::Text(format!("00{n}")),
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
                    fields,
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
                    Vec::new(),
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
    (named != (plan.malformed == Some(asset))).then_some(asset % plan.sets.len())
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
    metadata: Vec<(String, Meta)>,
    fault: Option<Fault>,
    error: Option<&'static str>,
}

/// Each message's route, in the order `spec/decoding.md` §3.3 checks it:
/// the alias locator, the asset bound at its ground time, the version
/// locator and the set, then identification (§4).
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
                    (_, Native::Text(_) | Native::IntText(_)) => Native::Text("unbound".into()),
                    _ => Native::Int(1 << 50),
                },
                _ => plan.ids[message.asset].clone(),
            };
            let bound = resolve(rows, &id, at);
            let versioned = plan.version != Version::None;
            let short = message.fault == Some(Fault::Short);
            let twice = message.fault == Some(Fault::Twice);
            let alias_unreadable =
                (short && plan.alias == Alias::Bits) || (twice && plan.alias == Alias::Metadata);
            let version_unreadable = (short && plan.version == Version::Bits)
                || (twice && plan.alias != Alias::Metadata && plan.version == Version::Metadata);
            let routing = if alias_unreadable {
                Some("unreadable_locator")
            } else {
                match bound {
                    None => Some("unknown_asset"),
                    Some((_, Some(_))) if versioned => Some("unexpected_set"),
                    Some(_) if versioned && version_unreadable => Some("unreadable_locator"),
                    // A version no loaded set has.
                    Some(_)
                        if versioned
                            && (message.fault == Some(Fault::UnknownVersion)
                                || plan.unloaded == Some(message.named)) =>
                    {
                        Some("unknown_version")
                    }
                    Some((_, None)) if !versioned => Some("missing_set"),
                    Some((_, Some(set))) if plan.unloaded == Some(set) => Some("unknown_set"),
                    _ => None,
                }
            };
            let error = routing.or((message.fault == Some(Fault::NoPacket)).then_some("no_packet"));
            let asset = bound.map_or(message.asset, |(asset, _)| asset);
            let set = match bound {
                _ if versioned => message.named,
                Some((_, Some(set))) => set,
                // Laid out as the asset's own set would be.
                _ => message.asset % plan.sets.len(),
            };
            let packets = &layouts[set];
            let packet = message.choice % packets.len();
            let layout = &packets[packet];
            let unknown_version = message.fault == Some(Fault::UnknownVersion);
            let version = if unknown_version { 99 } else { set as u64 + 1 };
            let mut header = Vec::new();
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
                metadata,
                fault: message.fault,
                error,
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
    for field in layouts.iter().flatten().flat_map(|packet| &packet.fields) {
        params.insert(field.param.clone(), param_json(&field.meaning));
    }
    let families = if plan.mirror { 2 } else { 1 };
    let mut sets = Vec::new();
    for family in 0..families {
        for (set, packets) in layouts.iter().enumerate() {
            let packets: Vec<Value> = packets
                .iter()
                .map(|packet| {
                    let mut json = json!({ "name": packet.name, "identify": packet.identify,
                                           "entries": packet.entries });
                    if packet.count.is_some() {
                        json["time_field"] = json!("t");
                    }
                    json
                })
                .collect();
            let mut json = json!({ "name": set_name(family, set), "family": family_name(family),
                                   "packets": packets });
            if plan.version != Version::None {
                json["version"] = if plan.versions == Versions::Text {
                    json!(format!("v{}", set + 1))
                } else {
                    json!(set + 1)
                };
            }
            let header: u64 = header(plan).iter().map(|(_, bits)| u64::from(*bits)).sum();
            if let Some(time) = time_json(&plan.sets[set].time, 8 + header) {
                json["time"] = time;
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
            Some(Fault::NoPacket) => message.payload[0] = NO_PACKET_ID,
            _ => {}
        }
        let teaches = routed.error.is_none() && correlate(plan, &routed, &message, &mut booted);
        let absolute = match (&routed.error, &message.event_time) {
            (Some(_), _) => None,
            (None, EventTime::At(at)) => Some(*at),
            (None, EventTime::SinceBoot { value, .. }) => match &plan.sets[routed.set].time {
                TimeSource::Boot { unit, .. } => booted
                    .get(&(routed.family, routed.asset))
                    .map(|boot| boot + i128::from(*value) * unit.nanos()),
                _ => None,
            },
            _ => None,
        };
        sent.push(Sent {
            family: family_name(routed.family),
            metadata: routed.metadata,
            message,
            error: routed.error,
            asset: asset_name(routed.asset),
            set: set_name(routed.family, routed.set),
            teaches,
            absolute,
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
    let mut json = json!({
        "name": family_name(family),
        "encoding": "binary",
        "byte_order": if plan.little { "little" } else { "big" },
    });
    let mut metadata = Vec::new();
    json["alias"] = match plan.alias {
        Alias::Fixed => json!({ "kind": "fixed", "value": "A1" }),
        Alias::Bits => json!({ "kind": "bits", "offset_bits": 8, "size_bits": plan.node_bits }),
        Alias::Metadata => {
            metadata.push("node");
            json!({ "kind": "metadata", "name": "node" })
        }
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
    let TimeSource::Boot {
        unit,
        clock_unit,
        epoch,
        scale,
    } = &plan.sets[routed.set].time
    else {
        return false;
    };
    let Some(correlation) = &plan.sets[routed.set].correlation else {
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

    use super::super::civil_from_days;
    use super::{EventTime, Meta, fleet, instant_text, label};

    /// Generated fleets reach every routing and correlation case.
    #[test]
    fn fleets_reach_every_case() {
        let mut runner = TestRunner::deterministic();
        let strategy = fleet();
        let mut seen = BTreeSet::new();
        for _ in 0..600 {
            let case = strategy.new_tree(&mut runner).unwrap().current();
            let family = &case.bundle["families"][0];
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
                        Meta::Text(text) if text.parse::<u64>().is_ok() => {
                            seen.insert(format!("integer text:{name}"));
                        }
                        Meta::Text(_) => {
                            seen.insert(format!("text:{name}"));
                        }
                        Meta::Int(_) => {
                            seen.insert(format!("negative:{name}"));
                        }
                        Meta::Uint(_) => {}
                    }
                }
            }
        }
        for case in [
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
        ] {
            assert!(seen.contains(case), "no fleet reaches {case}: {seen:?}");
        }
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
