//! What the random-layout tests share: loading a generated bundle as a
//! consumer would (`artifacts`), and checking a decoded message against
//! what the testkit worked out for it.

mod artifacts;

pub use artifacts::load;
use mdl::decode::{Decoded, EventTime, SampleValue};
use mdl::ir::Frame;
use mdl_testkit::layout::{self, Expected, Raw, Sample};
use proptest::prelude::*;

/// The decoded packet, every raw value, every sample and the event time
/// are what the testkit packed.
pub fn check(decoded: &Decoded<'_>, message: &layout::Message) -> Result<(), TestCaseError> {
    prop_assert_eq!(&decoded.packet.name, &message.packet);
    let record: Vec<(String, Raw)> = artifacts::record(&decoded.record);
    prop_assert_eq!(&record, &message.record, "{:?}", message.payload);
    let samples: Vec<Sample> = decoded
        .samples
        .iter()
        .map(|sample| Sample {
            param: sample.param.to_string(),
            value: expected(&sample.value),
            path: sample.path.clone(),
            index: sample.index,
            time_offset_ns: sample.time_offset_ns,
            quality: sample.quality.as_str(),
        })
        .collect();
    prop_assert_eq!(&samples, &message.samples);
    prop_assert_eq!(&event_time(&decoded.event_time), &message.event_time);
    Ok(())
}

/// A timestamp as Unix nanoseconds.
pub fn nanos(at: mdl::time::Timestamp) -> i128 {
    i128::from(at.seconds()) * 1_000_000_000 + i128::from(at.nanos())
}

fn event_time(time: &EventTime) -> layout::EventTime {
    match time {
        EventTime::At(at) => layout::EventTime::At(nanos(*at)),
        EventTime::SinceBoot { value, unit } => {
            let named = serde_json::to_value(unit).unwrap_or_default();
            let unit = match named.as_str() {
                Some("s") => "s",
                Some("ms") => "ms",
                Some("us") => "us",
                Some("ns") => "ns",
                _ => "other",
            };
            layout::EventTime::SinceBoot {
                value: *value,
                unit,
            }
        }
        _ => layout::EventTime::Ground,
    }
}

fn expected(value: &SampleValue) -> Expected {
    match value {
        SampleValue::F64(x) => Expected::F64(*x),
        SampleValue::I64(n) => Expected::I64(*n),
        SampleValue::U64(n) => Expected::U64(*n),
        SampleValue::Bool(b) => Expected::Bool(*b),
        SampleValue::Str(text) => Expected::Str(text.clone()),
        SampleValue::Bytes(bytes) => Expected::Bytes(bytes.clone()),
        SampleValue::Enum(name) => Expected::Enum(name.clone()),
        SampleValue::Vec3 { frame, x, y, z } => Expected::Vector {
            frame: frame_name(*frame),
            members: vec![*x, *y, *z],
        },
        SampleValue::Quat { frame, w, x, y, z } => Expected::Vector {
            frame: frame_name(*frame),
            members: vec![*w, *x, *y, *z],
        },
        other => Expected::Str(format!("not generated: {other:?}")),
    }
}

/// A frame as the IR spells it.
fn frame_name(frame: Frame) -> String {
    serde_json::to_value(frame)
        .ok()
        .and_then(|name| name.as_str().map(str::to_string))
        .unwrap_or_default()
}
