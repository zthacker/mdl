//! Event time: when a message's values were taken, by the asset's clock.

use std::collections::HashMap;

use serde_json::Value as Json;

use super::{Decoded, Quality, Record, Value};
use crate::v2::bits::read_bits;
use crate::v2::ir::{ByteOrder, Correlate, Packet, Set, Time, TimeUnit};
use crate::v2::route::Readable;
use crate::v2::time::Timestamp;

/// When a message's values were taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EventTime {
    /// The set has no time source, or the packet carries none: only the
    /// message's ground time is known.
    Ground,
    At(Timestamp),
    /// Time since the asset booted, not yet absolute; a [`Correlator`]
    /// turns it into an instant.
    SinceBoot {
        value: u64,
        unit: TimeUnit,
    },
}

/// A packet's event time, by its set's time source. `Err` explains why it
/// can't be read.
pub(super) fn event_time(
    set: &Set,
    packet: &Packet,
    record: &Record<'_>,
    readable: &Readable<'_>,
    json: Option<&Json>,
) -> Result<EventTime, String> {
    match &set.time {
        None => Ok(EventTime::Ground),
        Some(Time::Cuc {
            offset_bits,
            coarse_bytes,
            fine_bytes,
            epoch,
        }) => {
            let Readable::Binary { bytes, .. } = readable else {
                return Err("a CUC time code needs a binary payload".to_string());
            };
            let epoch = epoch
                .parse::<Timestamp>()
                .map_err(|err| format!("the CUC epoch: {err}"))?;
            cuc(
                bytes,
                u64::from(*offset_bits),
                *coarse_bytes,
                *fine_bytes,
                epoch,
            )
            .map(EventTime::At)
        }
        Some(Time::Iso8601 { pointer }) => {
            let text = json
                .and_then(|document| document.pointer(pointer))
                .and_then(Json::as_str)
                .ok_or_else(|| format!("no string at {pointer}"))?;
            Timestamp::from_rfc3339(text)
                .map(EventTime::At)
                .map_err(|err| err.to_string())
        }
        Some(Time::Boot { unit, .. }) => match &packet.time_field {
            None => Ok(EventTime::Ground),
            Some(field) => match record.get(field) {
                Some(Value::Uint(value)) => Ok(EventTime::SinceBoot {
                    value: *value,
                    unit: *unit,
                }),
                _ => Err(format!("time field {field} has no unsigned value")),
            },
        },
    }
}

/// A CCSDS unsegmented time code: whole seconds in `coarse_bytes`, then a
/// binary fraction of a second in `fine_bytes`, big-endian as CCSDS
/// defines it, counted from `epoch`. The fraction is truncated to whole
/// nanoseconds.
fn cuc(
    payload: &[u8],
    offset_bits: u64,
    coarse_bytes: u8,
    fine_bytes: u8,
    epoch: Timestamp,
) -> Result<Timestamp, String> {
    let coarse_bits = u32::from(coarse_bytes) * 8;
    let fine_bits = u32::from(fine_bytes) * 8;
    let too_short = || "the payload is too short for its CUC time code".to_string();
    let coarse =
        read_bits(payload, offset_bits, coarse_bits, ByteOrder::Big).map_err(|_| too_short())?;
    let fine = if fine_bits == 0 {
        0
    } else {
        read_bits(
            payload,
            offset_bits + u64::from(coarse_bits),
            fine_bits,
            ByteOrder::Big,
        )
        .map_err(|_| too_short())?
    };
    // At most 24 fine bits, so this fits easily.
    let fine_nanos = (i128::from(fine) * 1_000_000_000) >> fine_bits;
    let nanos = epoch.unix_nanos_wide() + i128::from(coarse) * 1_000_000_000 + fine_nanos;
    Timestamp::from_unix_nanos_wide(nanos)
        .ok_or_else(|| "the CUC time is past year 9999".to_string())
}

fn unit_nanos(unit: TimeUnit) -> i128 {
    match unit {
        TimeUnit::S => 1_000_000_000,
        TimeUnit::Ms => 1_000_000,
        TimeUnit::Us => 1_000,
        TimeUnit::Ns => 1,
    }
}

/// Turns boot-relative event times into instants, from the packets that
/// carry both clocks (a boot time source's `correlate` packet).
///
/// MDL's decoder is stateless; this is the one piece of cross-message
/// state it offers, and it's optional. It keeps, per family and asset,
/// the instant the asset last booted, from the latest correlation it
/// observed: absolute time minus time since boot. Feed it every decoded
/// message with [`observe`](Correlator::observe), in arrival order, then
/// ask [`absolute`](Correlator::absolute). Messages decoded before an
/// asset's first correlation have no instant, and those between a reboot
/// and the next correlation get the previous boot's. A consumer that
/// restarts can save boot instants ([`boot_time`](Correlator::boot_time))
/// and restore them ([`set_boot_time`](Correlator::set_boot_time)) instead
/// of waiting for the next correlation.
#[derive(Debug, Clone, Default)]
pub struct Correlator {
    /// When each asset booted, by family, then asset.
    booted: HashMap<String, HashMap<String, Timestamp>>,
}

impl Correlator {
    pub fn new() -> Self {
        Correlator::default()
    }

    /// Learn from a decoded message if it's its set's correlation packet
    /// and both clocks have a good reading (a sentinel on either doesn't
    /// count). Returns whether it learned.
    pub fn observe(&mut self, decoded: &Decoded<'_>) -> bool {
        let set = decoded.route.set;
        let Some(Time::Boot { unit, correlate }) = &set.time else {
            return false;
        };
        if decoded.packet.name != correlate.packet {
            return false;
        }
        let Some((unix, boot)) = clocks(decoded, correlate) else {
            return false;
        };
        let booted = i128::from(unix) * unit_nanos(correlate.unix_unit)
            - i128::from(boot) * unit_nanos(*unit);
        let Some(booted) = Timestamp::from_unix_nanos_wide(booted) else {
            return false;
        };
        self.set_boot_time(&set.family, &decoded.route.binding.asset, booted);
        true
    }

    /// The instant a decoded message's values were taken: its event time
    /// if that's absolute, its boot time after the asset's boot instant,
    /// or `None` for ground time only or an asset not yet correlated.
    pub fn absolute(&self, decoded: &Decoded<'_>) -> Option<Timestamp> {
        match decoded.event_time {
            EventTime::At(instant) => Some(instant),
            EventTime::Ground => None,
            EventTime::SinceBoot { value, unit } => {
                let booted =
                    self.boot_time(&decoded.route.set.family, &decoded.route.binding.asset)?;
                Timestamp::from_unix_nanos_wide(
                    booted.unix_nanos_wide() + i128::from(value) * unit_nanos(unit),
                )
            }
        }
    }

    /// When an asset of a family last booted, as far as this correlator
    /// knows.
    pub fn boot_time(&self, family: &str, asset: &str) -> Option<Timestamp> {
        self.booted.get(family)?.get(asset).copied()
    }

    /// Set when an asset of a family booted, such as one saved before a
    /// restart. The next correlation it observes replaces it.
    pub fn set_boot_time(&mut self, family: &str, asset: &str, booted: Timestamp) {
        self.booted
            .entry(family.to_string())
            .or_default()
            .insert(asset.to_string(), booted);
    }
}

/// The two clocks of a correlation packet, if both have a good reading.
fn clocks(decoded: &Decoded<'_>, correlate: &Correlate) -> Option<(u64, u64)> {
    let reading = |name: &str| {
        let good = decoded
            .engineering(name)
            .is_some_and(|engineering| engineering.quality == Quality::Good);
        match decoded.record.get(name) {
            Some(Value::Uint(value)) if good => Some(*value),
            _ => None,
        }
    };
    Some((
        reading(&correlate.unix_field)?,
        reading(&correlate.boot_field)?,
    ))
}
