//! Event time the other way round: the readings a packet's clocks hold for
//! an instant (`spec/encoding.md` §9.4), which `decode/clock.rs` reads back.
//!
//! A clock holds its latest reading at or before the instant's label on
//! its scale, as a clock that ticks does. Decoding that reading gives the
//! instant back when the clock holds its label exactly, and otherwise the
//! instant of the clock's latest reading before it.

use super::{EntryProblem, Fix, fix_value, show_value, top_level_at};
use crate::decode::{GPS_EPOCH, Value, WEEK, count_nanos, epoch_label, is_sentinel, unit_nanos};
use crate::encode::FieldProblem;
use crate::ir::{Entry, Field, Packet, ScalarType, Set, Time, TimeScale, TimeUnit};
use crate::time::{Timestamp, leap};

const NANOS: i128 = 1_000_000_000;

/// What fixes a clock the event time sets.
pub(super) const EVENT_TIME: &str = "the event time";

/// What fixes a clock that counts time since boot.
const SINCE_BOOT: &str = "the event time less the boot instant";

/// A reading of one of a packet's clocks.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Reading {
    /// A top-level field's raw value, and what fixes it.
    Field {
        name: String,
        value: Fix,
        why: &'static str,
    },
    /// A CUC time code at `offset_bits`: `coarse` whole seconds in
    /// `coarse_bytes`, then `fine`, a binary fraction of a second, in
    /// `fine_bytes`, both big-endian.
    Code {
        offset_bits: u32,
        coarse_bytes: u8,
        coarse: u64,
        fine_bytes: u8,
        fine: u64,
    },
    /// Text at a JSON Pointer.
    Text { pointer: String, text: String },
}

/// The readings a request's instants give a packet's clocks, and the
/// clocks they don't set: each one's top-level field, and what would set
/// it.
#[derive(Debug, Default)]
pub(super) struct Clocks {
    pub(super) readings: Vec<Reading>,
    pub(super) unset: Vec<(String, &'static str)>,
}

/// A clock that can't take its reading: the entry's path, and why.
type Refused = (String, EntryProblem);

/// The readings of `packet`'s clocks at `at`, for an asset that booted at
/// `booted`, by its set's time source (`decoding.md` §8). A packet with no
/// clock takes none, whatever the request gives. A clock whose time the
/// request doesn't give is unset; a CUC time code or an ISO 8601 pointer
/// that no field holds is refused instead, as it can take no other value.
pub(super) fn clocks(
    set: &Set,
    packet: &Packet,
    at: Option<Timestamp>,
    booted: Option<Timestamp>,
    binary: bool,
) -> Result<Clocks, Refused> {
    let mut clocks = Clocks::default();
    let Some(time) = &set.time else {
        return Ok(clocks);
    };
    match time {
        Time::Cuc {
            offset_bits,
            coarse_bytes,
            fine_bytes,
            epoch,
            scale,
        } => {
            let Some(at) = at else {
                return Err(("time".to_string(), EntryProblem::NoEventTime));
            };
            let since = label(*scale, at) - epoch_of(epoch, "time")?;
            let coarse_bits = u32::from(*coarse_bytes) * 8;
            let whole = since.div_euclid(NANOS);
            let coarse = u64::try_from(whole)
                .ok()
                .filter(|seconds| coarse_bits >= 64 || seconds >> coarse_bits == 0)
                .ok_or_else(|| {
                    (
                        "time".to_string(),
                        EntryProblem::EventTimeRange {
                            raw: format!("{whole} s since {epoch}"),
                            holds: format!("0 to {} s", (1_u128 << coarse_bits) - 1),
                        },
                    )
                })?;
            clocks.readings.push(Reading::Code {
                offset_bits: *offset_bits,
                coarse_bytes: *coarse_bytes,
                coarse,
                fine_bytes: *fine_bytes,
                fine: fraction(since.rem_euclid(NANOS), u32::from(*fine_bytes) * 8).ok_or_else(
                    || {
                        (
                            "time".to_string(),
                            EntryProblem::Field(FieldProblem::Layout(
                                "a CUC time code of at most 3 fine bytes",
                            )),
                        )
                    },
                )?,
            });
        }
        Time::Elapsed { unit, epoch, scale } => {
            let Some(name) = &packet.time_field else {
                return Ok(clocks);
            };
            match at {
                Some(at) => {
                    let since = label(*scale, at) - epoch_of(epoch, name)?;
                    clocks.field(packet, name, since, *unit, EVENT_TIME, binary)?;
                }
                None => clocks.unset.push((name.clone(), EVENT_TIME)),
            }
        }
        Time::Gps {
            unit,
            week_field,
            week_bits,
        } => {
            let Some(name) = &packet.time_field else {
                return Ok(clocks);
            };
            let Some(at) = at else {
                clocks.unset.push((week_field.clone(), EVENT_TIME));
                clocks.unset.push((name.clone(), EVENT_TIME));
                return Ok(clocks);
            };
            let since = label(TimeScale::Gps, at) - GPS_EPOCH;
            let week = since.div_euclid(WEEK);
            // A week before the first can't be told from one a cycle later,
            // so it's left whole for the field to refuse.
            let written = match week_bits {
                Some(bits) if week >= 0 => week.rem_euclid(1_i128 << (*bits).clamp(1, 32)),
                _ => week,
            };
            let week_reading = checked(packet, week_field, Fix::Int(written), EVENT_TIME, binary)?;
            clocks.readings.push(week_reading);
            clocks.field(
                packet,
                name,
                since.rem_euclid(WEEK),
                *unit,
                EVENT_TIME,
                binary,
            )?;
        }
        Time::Iso8601 { pointer } => match (at, top_level_at(packet, pointer)) {
            (Some(at), Some(name)) => {
                let reading =
                    checked(packet, &name, Fix::Text(at.to_string()), EVENT_TIME, binary)?;
                clocks.readings.push(reading);
            }
            (Some(at), None) => clocks.readings.push(Reading::Text {
                pointer: pointer.clone(),
                text: at.to_string(),
            }),
            (None, Some(name)) => clocks.unset.push((name, EVENT_TIME)),
            (None, None) => return Err((pointer.clone(), EntryProblem::NoEventTime)),
        },
        Time::Boot { unit, correlate } => {
            let mut boot_clocks: Vec<&str> = packet.time_field.iter().map(String::as_str).collect();
            if packet.name == correlate.packet {
                boot_clocks.push(&correlate.boot_field);
                let name = &correlate.clock_field;
                match at {
                    Some(at) => {
                        let since = label(correlate.scale, at) - epoch_of(&correlate.epoch, name)?;
                        clocks.field(
                            packet,
                            name,
                            since,
                            correlate.clock_unit,
                            EVENT_TIME,
                            binary,
                        )?;
                    }
                    None => clocks.unset.push((name.clone(), EVENT_TIME)),
                }
            }
            for name in boot_clocks {
                match (at, booted) {
                    (Some(at), Some(booted)) => {
                        let since = at.unix_nanos_wide() - booted.unix_nanos_wide();
                        clocks.field(packet, name, since, *unit, SINCE_BOOT, binary)?;
                    }
                    (Some(_), None) => clocks.unset.push((name.to_string(), "the boot instant")),
                    (None, Some(_)) => clocks.unset.push((name.to_string(), "the event time")),
                    (None, None) => clocks
                        .unset
                        .push((name.to_string(), "the event time and the boot instant")),
                }
            }
        }
    }
    Ok(clocks)
}

impl Clocks {
    /// The reading of the clock field `name`, `nanos` since its start in
    /// `unit`s.
    fn field(
        &mut self,
        packet: &Packet,
        name: &str,
        nanos: i128,
        unit: TimeUnit,
        why: &'static str,
        binary: bool,
    ) -> Result<(), Refused> {
        let field = top_field(packet, name)?;
        let value = match field.ty {
            ScalarType::Float => {
                let single = binary && field.size_bits == Some(32);
                Fix::Float(float_count(nanos, unit, single).ok_or_else(|| {
                    (
                        name.to_string(),
                        EntryProblem::Field(FieldProblem::Layout(
                            "a float clock that holds the event time",
                        )),
                    )
                })?)
            }
            _ => Fix::Int(nanos.div_euclid(unit_nanos(unit))),
        };
        self.readings
            .push(checked(packet, name, value, why, binary)?);
        Ok(())
    }
}

/// A top-level field's reading, once its field holds it and it isn't one
/// of the field's `invalid` values, which decoding reads as no reading.
fn checked(
    packet: &Packet,
    name: &str,
    value: Fix,
    why: &'static str,
    binary: bool,
) -> Result<Reading, Refused> {
    let refuse = |problem: EntryProblem| (name.to_string(), problem);
    let field = top_field(packet, name)?;
    let raw = fix_value(field, &value, binary).map_err(|problem| match problem {
        FieldProblem::RawRange { raw, holds } => {
            refuse(EntryProblem::EventTimeRange { raw, holds })
        }
        problem => refuse(EntryProblem::Field(problem)),
    })?;
    if is_sentinel(&field.invalid, &raw) {
        return Err(refuse(EntryProblem::InvalidRaw {
            raw: show_value(&raw),
        }));
    }
    Ok(Reading::Field {
        name: name.to_string(),
        value,
        why,
    })
}

/// The packet's top-level field `name`, where validation puts its clocks.
fn top_field<'p>(packet: &'p Packet, name: &str) -> Result<&'p Field, Refused> {
    packet
        .entries
        .iter()
        .find_map(|entry| match entry {
            Entry::Field(field) if field.name == name => Some(field),
            _ => None,
        })
        .ok_or_else(|| {
            (
                name.to_string(),
                EntryProblem::Field(FieldProblem::Layout("a clock at the packet's top level")),
            )
        })
}

/// An instant's label on `scale` (`decoding.md` §8.1).
fn label(scale: TimeScale, at: Timestamp) -> i128 {
    leap::from_utc(scale, at.unix_nanos_wide())
}

fn epoch_of(epoch: &str, path: &str) -> Result<i128, Refused> {
    epoch_label(epoch).map_err(|_| {
        (
            path.to_string(),
            EntryProblem::Field(FieldProblem::Layout("a clock epoch MDL reads")),
        )
    })
}

/// The largest binary fraction of `bits` bits that decoding reads as at
/// most `nanos` into the second, rounding down: `f` with
/// `floor(f * 10^9 / 2^bits) <= nanos`, which is `f * 10^9 < (nanos + 1) *
/// 2^bits`. Validation allows at most 24 bits, so each fraction reads as
/// its own nanosecond, and it fits; `None` for more.
fn fraction(nanos: i128, bits: u32) -> Option<u64> {
    if bits > 24 {
        return None;
    }
    u64::try_from((((nanos + 1) << bits) - 1).div_euclid(NANOS)).ok()
}

/// The bits of `f32::MAX` and `f64::MAX`: the largest keys of finite
/// values, by format.
pub(super) const SINGLE_TOP: i64 = 0x7F7F_FFFF;
pub(super) const DOUBLE_TOP: i64 = 0x7FEF_FFFF_FFFF_FFFF;

/// The finite values of a float format in order, by key: a value's key is
/// its bits, negated for a negative value, so +0 and -0 are both key 0,
/// which is +0.
fn float_at(key: i64, single: bool) -> f64 {
    let magnitude = key.unsigned_abs();
    let x = if single {
        // A binary32 key is at most SINGLE_TOP, so it fits in 32 bits.
        f64::from(f32::from_bits(magnitude as u32))
    } else {
        f64::from_bits(magnitude)
    };
    if key < 0 { -x } else { x }
}

/// What decoding reads a float clock as.
enum Decodes {
    /// Too far below 0 to read.
    Below,
    Nanos(i128),
    /// Too far above 0 to read.
    Above,
}

/// A float clock's reading for `nanos` in `unit`s: of the values of its
/// format (binary32 when `single`), the least that decodes to the latest
/// time at or before `nanos`. Decoding multiplies a reading by the unit in
/// binary64 and rounds down, which never falls as the reading grows, so two
/// binary searches over the finite values in order find it: first the
/// largest value that decodes to at most `nanos`, then the least that
/// decodes to as much. `None` only if no value decodes that low.
pub(super) fn float_count(nanos: i128, unit: TimeUnit, single: bool) -> Option<f64> {
    let top = if single { SINGLE_TOP } else { DOUBLE_TOP };
    let decodes = |key: i64| {
        let x = float_at(key, single);
        match count_nanos(Some(&Value::Float(x)), unit, "") {
            Ok(n) => Decodes::Nanos(n),
            Err(_) if x < 0.0 => Decodes::Below,
            Err(_) => Decodes::Above,
        }
    };
    let at_most = |key: i64, limit: i128| match decodes(key) {
        Decodes::Below => true,
        Decodes::Nanos(n) => n <= limit,
        Decodes::Above => false,
    };
    // The largest key that decodes to at most `nanos`. The lowest key is
    // too far below 0 to read, and the highest too far above.
    let (mut low, mut high) = (-top, top);
    if !at_most(low, nanos) || at_most(high, nanos) {
        return None;
    }
    while high > low + 1 {
        let mid = low.midpoint(high);
        if at_most(mid, nanos) {
            low = mid;
        } else {
            high = mid;
        }
    }
    let Decodes::Nanos(latest) = decodes(low) else {
        return None;
    };
    // The least key that decodes to `latest`: every key below it decodes
    // to less, and `low` itself to `latest`.
    let (mut less, mut least) = (-top, low);
    while least > less + 1 {
        let mid = less.midpoint(least);
        if at_most(mid, latest - 1) {
            less = mid;
        } else {
            least = mid;
        }
    }
    Some(float_at(least, single))
}
