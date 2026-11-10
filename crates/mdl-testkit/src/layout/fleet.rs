//! A family of several sets and assets, and a stream of messages for it in
//! ground-time order: routing by alias and version (`spec/decoding.md`
//! §3), identification by more than one key with the more specific packet
//! winning (§4), and boot time tied to instants by correlation (§8.2).

use std::collections::{BTreeMap, BTreeSet};

use proptest::collection::{btree_set, vec};
use proptest::prelude::*;
use proptest::sample::select;
use serde_json::{Map, Value, json};

use super::{
    Correlation, Count, EventTime, FieldSpec, Message, PacketLayout, PacketShape, SECOND,
    TimeSource, count, epoch, field_spec, label, lay_out, message_at, param_json, time_json,
    time_scale, to_utc, unit,
};

/// A family, its sets, its bindings, and messages for it.
#[derive(Debug, Clone)]
pub struct Fleet {
    pub bundle: Value,
    pub family: String,
    /// The assets, by name.
    pub assets: Vec<String>,
    /// The bindings file, in TOML.
    pub bindings: String,
    /// In ground-time order.
    pub messages: Vec<Sent>,
}

/// A message sent by an asset, and how it routes and correlates.
#[derive(Debug, Clone)]
pub struct Sent {
    pub asset: String,
    pub set: String,
    /// Its metadata, by name.
    pub metadata: Vec<(String, u64)>,
    pub message: Message,
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

#[derive(Debug, Clone)]
struct Plan {
    little: bool,
    alias: Alias,
    version: Version,
    /// Whether packets are told apart by an id and a `sub` byte.
    two_keys: bool,
    sets: Vec<SetPlan>,
    /// Each asset's native id.
    ids: Vec<u64>,
    /// An asset rebound to another set partway through: the asset, the set,
    /// and the seconds after the first message.
    switch: Option<(usize, usize, i64)>,
    /// Each message: its asset, the set it names (by version), which packet,
    /// a `sub` for the least specific packet, and the seconds since the
    /// message before.
    messages: Vec<(usize, usize, usize, u64, i64)>,
    /// The first message's ground time.
    start: i128,
}

#[derive(Debug, Clone)]
struct SetPlan {
    time: TimeSource,
    /// Each packet's fields and count field.
    packets: Vec<(Vec<FieldSpec>, Option<Count>)>,
    /// Whether there's a packet keyed by the id alone, less specific than
    /// the others.
    general: bool,
    correlation: Option<Correlation>,
}

/// A family of one to three sets and assets and up to twelve messages.
pub fn fleet() -> BoxedStrategy<Fleet> {
    let shape = (
        any::<bool>(),
        select(&[Alias::Fixed, Alias::Bits, Alias::Metadata][..]),
        select(&[Version::None, Version::Bits, Version::Metadata][..]),
        any::<bool>(),
        1_usize..=3,
        1_usize..=3,
        any::<bool>(),
    );
    shape
        .prop_flat_map(|(little, alias, version, two_keys, sets, assets, boot)| {
            let assets = if alias == Alias::Fixed { 1 } else { assets };
            let ids = match alias {
                Alias::Fixed => Just(vec![0]).boxed(),
                Alias::Bits => btree_set(0_u64..=255, assets)
                    .prop_map(|ids| ids.into_iter().collect())
                    .boxed(),
                Alias::Metadata => btree_set(0_u64..1 << 32, assets)
                    .prop_map(|ids| ids.into_iter().collect())
                    .boxed(),
            };
            let switch = if version == Version::None && sets > 1 {
                proptest::option::of((0..assets, 0..sets, 1_i64..100_000)).boxed()
            } else {
                Just(None).boxed()
            };
            let messages = vec(
                (
                    0..assets,
                    0..sets,
                    any::<usize>(),
                    4_u64..=255,
                    0_i64..50_000,
                ),
                1..=12,
            );
            (
                Just((little, alias, version, two_keys)),
                vec(set_plan(boot), sets),
                ids,
                switch,
                messages,
                // Whole seconds, as a rebinding instant is written.
                (label((1995, 1, 1), (0, 0, 0), 0) / SECOND
                    ..label((2030, 1, 1), (0, 0, 0), 0) / SECOND)
                    .prop_map(|seconds| seconds * SECOND),
            )
        })
        .prop_map(
            |((little, alias, version, two_keys), sets, ids, switch, messages, start)| Plan {
                little,
                alias,
                version,
                two_keys,
                sets,
                ids,
                switch,
                messages,
                start,
            },
        )
        .prop_flat_map(|plan| {
            let layouts = layouts(&plan);
            let routed = route(&plan, &layouts);
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
            (Just(plan), Just(layouts), Just(routed), messages)
        })
        .prop_map(|(plan, layouts, routed, messages)| assemble(&plan, &layouts, routed, messages))
        .boxed()
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
        let packets = vec((vec(field_spec(), 1..=4), count(&time)), 1..=3);
        let correlation = match &time {
            TimeSource::Boot { clock_unit, .. } => {
                let widths: Vec<u32> = (8..=clock_unit.count_bits()).step_by(8).collect();
                (
                    select(widths),
                    btree_set(any::<u8>(), 0..=1),
                    btree_set(any::<u8>(), 0..=1),
                )
                    .prop_map(|(clock_bits, clock, boot)| {
                        Some(Correlation {
                            clock_bits,
                            clock_invalid: clock.into_iter().map(u64::from).collect(),
                            boot_invalid: boot.into_iter().map(u64::from).collect(),
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

/// Each set's packets, laid out: the planned ones, then the least specific
/// one, then the correlation packet. With two keys, packet `k` is id
/// `k / 2 + 1` and sub `k % 2 + 1`, so packets share ids; the least specific
/// is id 1 alone, and takes every other sub.
fn layouts(plan: &Plan) -> Vec<Vec<PacketLayout>> {
    let mut header = Vec::new();
    if plan.alias == Alias::Bits {
        header.push("node");
    }
    if plan.version == Version::Bits {
        header.push("ver");
    }
    if plan.two_keys {
        header.push("sub");
    }
    plan.sets
        .iter()
        .enumerate()
        .map(|(set, planned)| {
            let mut packets = Vec::new();
            let mut shapes: Vec<(PacketShape, Option<Count>, Vec<FieldSpec>)> = Vec::new();
            for (index, (fields, count)) in planned.packets.iter().enumerate() {
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
                shapes.push((shape, *count, fields.clone()));
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
                shapes.push((shape, planned.packets[0].1, fields));
            }
            for (shape, count, fields) in shapes {
                packets.push(lay_out(
                    shape,
                    plan.little,
                    &planned.time,
                    count,
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
                    None,
                    Some(correlation.clone()),
                    Vec::new(),
                ));
            }
            packets
        })
        .collect()
}

/// Where a message goes, and what its header and metadata hold.
#[derive(Debug, Clone)]
struct Routed {
    asset: usize,
    set: usize,
    packet: usize,
    ground: i128,
    header: Vec<u64>,
    metadata: Vec<(String, u64)>,
}

/// The set that decodes each message, by its version or its asset's
/// binding at its ground time, and its packet in that set.
fn route(plan: &Plan, layouts: &[Vec<PacketLayout>]) -> Vec<Routed> {
    let mut ground = plan.start;
    plan.messages
        .iter()
        .map(|&(asset, named, choice, general_sub, gap)| {
            ground += i128::from(gap) * SECOND;
            let set = match (plan.version, plan.switch) {
                (Version::None, Some((switched, to, after)))
                    if switched == asset && ground >= plan.start + i128::from(after) * SECOND =>
                {
                    to
                }
                (Version::None, _) => asset % plan.sets.len(),
                _ => named,
            };
            let packets = &layouts[set];
            let packet = choice % packets.len();
            let layout = &packets[packet];
            let version = set as u64 + 1;
            let mut header = Vec::new();
            if plan.alias == Alias::Bits {
                header.push(plan.ids[asset]);
            }
            if plan.version == Version::Bits {
                header.push(version);
            }
            if plan.two_keys {
                // The least specific packet takes subs no other one has.
                let sub = match layout.identify.get("sub").and_then(Value::as_u64) {
                    Some(sub) => sub,
                    None if layout.name == "GENERAL" => general_sub,
                    None => 0,
                };
                header.push(sub);
            }
            let mut metadata = Vec::new();
            if plan.alias == Alias::Metadata {
                metadata.push(("node".to_string(), plan.ids[asset]));
            }
            if plan.version == Version::Metadata {
                metadata.push(("ver".to_string(), version));
            }
            Routed {
                asset,
                set,
                packet,
                ground,
                header,
                metadata,
            }
        })
        .collect()
}

fn asset_name(asset: usize) -> String {
    format!("AST{asset}")
}

fn set_name(set: usize) -> String {
    format!("fam_v{}", set + 1)
}

fn assemble(
    plan: &Plan,
    layouts: &[Vec<PacketLayout>],
    routed: Vec<Routed>,
    messages: Vec<Message>,
) -> Fleet {
    let mut params = Map::new();
    for field in layouts.iter().flatten().flat_map(|packet| &packet.fields) {
        params.insert(field.param.clone(), param_json(&field.meaning));
    }
    let sets: Vec<Value> = layouts
        .iter()
        .enumerate()
        .map(|(set, packets)| {
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
            let mut json = json!({ "name": set_name(set), "family": "fam", "packets": packets });
            if plan.version != Version::None {
                json["version"] = json!(set + 1);
            }
            if let Some(time) = time_json(&plan.sets[set].time, 8 + 8 * header_length(plan)) {
                json["time"] = time;
            }
            json
        })
        .collect();
    let bundle = json!({
        "mdl_version": "2.0",
        "catalog": { "params": params },
        "families": [family_json(plan)],
        "sets": sets
    });
    let assets: Vec<String> = (0..plan.ids.len()).map(asset_name).collect();
    let mut sent = Vec::new();
    let mut booted: BTreeMap<usize, i128> = BTreeMap::new();
    for (routed, message) in routed.into_iter().zip(messages) {
        let teaches = correlate(plan, &routed, &message, &mut booted);
        let absolute = match &message.event_time {
            EventTime::At(at) => Some(*at),
            EventTime::SinceBoot { value, .. } => match &plan.sets[routed.set].time {
                TimeSource::Boot { unit, .. } => booted
                    .get(&routed.asset)
                    .map(|boot| boot + i128::from(*value) * unit.nanos()),
                _ => None,
            },
            _ => None,
        };
        sent.push(Sent {
            asset: asset_name(routed.asset),
            set: set_name(routed.set),
            metadata: routed.metadata,
            message,
            teaches,
            absolute,
        });
    }
    Fleet {
        bundle,
        family: "fam".into(),
        bindings: bindings(plan),
        assets,
        messages: sent,
    }
}

fn header_length(plan: &Plan) -> u64 {
    [
        plan.alias == Alias::Bits,
        plan.version == Version::Bits,
        plan.two_keys,
    ]
    .into_iter()
    .filter(|present| *present)
    .count() as u64
}

fn family_json(plan: &Plan) -> Value {
    let mut family = json!({
        "name": "fam",
        "encoding": "binary",
        "byte_order": if plan.little { "little" } else { "big" },
    });
    let mut metadata = Vec::new();
    family["alias"] = match plan.alias {
        Alias::Fixed => json!({ "kind": "fixed", "value": "A1" }),
        Alias::Bits => json!({ "kind": "bits", "offset_bits": 8, "size_bits": 8 }),
        Alias::Metadata => {
            metadata.push("node");
            json!({ "kind": "metadata", "name": "node" })
        }
    };
    match plan.version {
        Version::None => {}
        Version::Bits => {
            let offset = if plan.alias == Alias::Bits { 16 } else { 8 };
            family["version"] = json!({ "kind": "bits", "offset_bits": offset, "size_bits": 8 });
        }
        Version::Metadata => {
            metadata.push("ver");
            family["version"] = json!({ "kind": "metadata", "name": "ver" });
        }
    }
    if !metadata.is_empty() {
        family["metadata"] = json!(metadata);
    }
    family
}

/// Whether a correlator learns from the message (§8.2): a boot set's
/// correlation packet whose clocks both read good, giving the asset's boot
/// instant as the absolute clock's instant less the boot clock.
fn correlate(
    plan: &Plan,
    routed: &Routed,
    message: &Message,
    booted: &mut BTreeMap<usize, i128>,
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
    booted.insert(routed.asset, instant - i128::from(boot) * unit.nanos());
    true
}

/// The bindings file: each asset from 1980-01-06, bound to its set when
/// the family has no version locator, and rebound to another partway
/// through when the plan says so.
fn bindings(plan: &Plan) -> String {
    let mut text = String::from("mdl_version = \"2.0\"\n");
    let native = |asset: usize| match plan.alias {
        Alias::Fixed => "\"A1\"".to_string(),
        _ => plan.ids[asset].to_string(),
    };
    let mut seen = BTreeSet::new();
    for asset in 0..plan.ids.len() {
        if !seen.insert(asset) {
            continue;
        }
        text.push_str(&format!(
            "\n[[binding]]\nfamily = \"fam\"\nnative_id = {}\nasset = \"{}\"\n",
            native(asset),
            asset_name(asset)
        ));
        if plan.version == Version::None {
            text.push_str(&format!(
                "set = \"{}\"\n",
                set_name(asset % plan.sets.len())
            ));
        }
        text.push_str("effective_from = \"1980-01-06T00:00:00Z\"\n");
    }
    if let (Version::None, Some((asset, set, after))) = (plan.version, plan.switch) {
        let at = plan.start + i128::from(after) * SECOND;
        text.push_str(&format!(
            "\n[[binding]]\nfamily = \"fam\"\nnative_id = {}\nasset = \"{}\"\nset = \"{}\"\n\
             effective_from = \"{}\"\n",
            native(asset),
            asset_name(asset),
            set_name(set),
            instant_text(at)
        ));
    }
    text
}

/// A whole-second Unix instant in MDL's spelling.
fn instant_text(unix: i128) -> String {
    let seconds = unix.div_euclid(SECOND);
    let days = seconds.div_euclid(86_400);
    let of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(i64::try_from(days).unwrap_or(0));
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        of_day / 3600,
        of_day % 3600 / 60,
        of_day % 60
    )
}

/// The date `days` after 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use proptest::strategy::{Strategy, ValueTree};
    use proptest::test_runner::TestRunner;

    use super::{EventTime, civil_from_days, fleet, instant_text, label};

    /// Generated fleets reach every routing and correlation case: each
    /// alias and version locator, a binding that changes set, the least
    /// specific packet, correlations that teach and ones a sentinel
    /// spoils, and boot times before and after the first correlation.
    #[test]
    fn fleets_reach_every_case() {
        let mut runner = TestRunner::deterministic();
        let strategy = fleet();
        let mut seen = BTreeSet::new();
        for _ in 0..300 {
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
            if case.bindings.matches("[[binding]]").count() > case.assets.len() {
                seen.insert("rebound".to_string());
            }
            for sent in &case.messages {
                if sent.message.packet == "GENERAL" {
                    seen.insert("general".to_string());
                }
                if sent.message.packet == "CORR" {
                    seen.insert(format!("teaches:{}", sent.teaches));
                }
                if let EventTime::SinceBoot { .. } = sent.message.event_time {
                    seen.insert(format!("correlated:{}", sent.absolute.is_some()));
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
            "rebound",
            "general",
            "teaches:true",
            "teaches:false",
            "correlated:true",
            "correlated:false",
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
    }
}
