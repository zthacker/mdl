//! Encoding telemetry (`spec/encoding.md` §9) over random valid layouts
//! (`mdl_testkit::layout`), binary and JSON: every message the testkit
//! packed, decoded and its record given back as raw values, encodes to a
//! payload that decodes to the same packet, native id and record, and so
//! to the same samples. The testkit packs each message by its own reading
//! of the decoding spec, over random bytes, so encoding agrees with that
//! reading wherever a value lands, and leaves nothing a value depends on
//! to the bytes around it.
//!
//! Checksums aren't given back: they covered the random bytes, where
//! encoding writes zeros. A JSON payload writes -0 as 0, as RFC 8785 does,
//! so a JSON record is compared with its zeros positive.

#[path = "common/artifacts.rs"]
mod artifacts;

use mdl::bindings::{Bindings, NativeId};
use std::collections::BTreeSet;

use mdl::decode::{Decoded, SampleValue, Value};
use mdl::encode::{EntryProblem, ParamProblem, RawValue, TelemetryError, TelemetryRequest};
use mdl::message::{Message, MetaValue, Payload};
use mdl::route::FamilySets;
use mdl::time::Timestamp;
use mdl_testkit::layout::{self, telemetry};
use proptest::prelude::*;

proptest! {
    #[test]
    fn a_decoded_record_encodes_back_to_itself(case in telemetry()) {
        round_trip(&case)?;
    }

    #[test]
    fn a_decoded_json_record_encodes_back_to_itself(case in layout::json_telemetry()) {
        round_trip(&case)?;
    }

    #[test]
    fn a_decoded_boot_record_encodes_back_to_itself(case in layout::boot_telemetry()) {
        round_trip(&case)?;
    }

    #[test]
    fn a_decoded_boot_json_record_encodes_back_to_itself(
        case in layout::json_boot_telemetry()
    ) {
        round_trip(&case)?;
    }

    /// The decoded samples given back as params, through inverse
    /// conversions, units, states and composites, and raw values for the
    /// entries no param maps.
    #[test]
    fn a_decoded_message_encodes_back_from_its_samples(case in telemetry()) {
        from_samples(&case)?;
    }

    #[test]
    fn a_decoded_json_message_encodes_back_from_its_samples(case in layout::json_telemetry()) {
        from_samples(&case)?;
    }
}

/// Every message whose samples are all good encodes from them, and
/// decodes back to them: exactly, but for numbers, which go through a
/// conversion and its inverse in binary64 and may come back a rounding
/// away. A value at the edge of what its field holds may round past it,
/// or onto a raw value the field lists as no reading, which is refused,
/// as is a message whose array is partly samples and
/// partly not, which no request can give. A param whose field's
/// conversion has no inverse, or whose values are all that could count an
/// array in a repeated record, is refused by name, and given raw instead;
/// so is one left over, which maps optional entries in repeated records.
fn from_samples(case: &layout::Telemetry) -> Result<(), TestCaseError> {
    let sets = artifacts::load(&case.bundle, &case.family, &[])?;
    let bindings = bindings(case);
    let time = &case.bundle["sets"][0]["time"]["kind"];
    let timeless = time == "cuc" || time == "iso8601";
    let mut read = BTreeSet::new();
    read_fields(&case.bundle["sets"][0], &mut read);
    let json = case.bundle["families"][0]["encoding"] == "json";
    for message in &case.messages {
        let metadata = metadata_of(message);
        let Ok(decoded) = sets.decode(&message_of(message).with_metadata(&metadata), &bindings)
        else {
            continue;
        };
        if decoded
            .samples
            .iter()
            .any(|sample| sample.quality.as_str() != "good")
        {
            continue;
        }
        let checksums = checksums(case, &message.packet);
        let full = record_raw(&decoded, &checksums)?;
        // Fields a condition, count or length reads keep their raw values:
        // a param can't always give one back exactly (a bool reads any
        // integer but 0 as true).
        let mut raw_params: BTreeSet<String> = decoded
            .samples
            .iter()
            .filter(|sample| {
                sample.path.split(',').any(|path| {
                    let last = path.rsplit('.').next().unwrap_or(path);
                    let name = last.split('[').next().unwrap_or(last);
                    read.contains(name)
                })
            })
            .map(|sample| sample.param.to_string())
            .collect();
        let encoded = loop {
            // A given composite gives its members, so their own params
            // take nothing there.
            let members: BTreeSet<&str> = decoded
                .samples
                .iter()
                .filter(|sample| !raw_params.contains(sample.param))
                .filter(|sample| {
                    matches!(
                        sample.value,
                        SampleValue::Vec3 { .. } | SampleValue::Quat { .. }
                    )
                })
                .flat_map(|sample| sample.path.split(','))
                .collect();
            let samples: Vec<_> = decoded
                .samples
                .iter()
                .filter(|sample| !raw_params.contains(sample.param))
                .filter(|sample| {
                    matches!(
                        sample.value,
                        SampleValue::Vec3 { .. } | SampleValue::Quat { .. }
                    ) || !members.contains(sample.path.as_str())
                })
                .collect();
            let mapped: BTreeSet<String> = samples
                .iter()
                .flat_map(|sample| sample.path.split(',').map(str::to_string))
                .collect();
            let Some(raw) = pruned(full.clone(), "", &mapped) else {
                break None;
            };
            let mut params: Vec<(String, Vec<SampleValue>)> = Vec::new();
            for sample in samples {
                match params.iter_mut().find(|(param, _)| param == sample.param) {
                    Some((_, values)) => values.push(sample.value.clone()),
                    None => params.push((sample.param.to_string(), vec![sample.value.clone()])),
                }
            }
            let mut request = TelemetryRequest::new(
                &case.set,
                &message.packet,
                NativeId::from_text(case.native_id.clone()),
            );
            request.raw = raw;
            request.params = params;
            match sets.encode_telemetry(&request) {
                Ok(encoded) => {
                    let members: BTreeSet<String> =
                        members.into_iter().map(str::to_string).collect();
                    break Some((encoded, members));
                }
                Err(TelemetryError::Entry {
                    problem: EntryProblem::NoInverse { param } | EntryProblem::Uncounted { param },
                    ..
                }) if !raw_params.contains(&param) => {
                    raw_params.insert(param);
                }
                // An optional entry inside an array's records takes no value
                // from its param, so the param's values are left over.
                Err(TelemetryError::Param {
                    param,
                    problem: ParamProblem::Count { .. },
                }) if !raw_params.contains(&param) => {
                    raw_params.insert(param);
                }
                Err(error) if timeless && error.to_string().contains("needs the event time") => {
                    break None;
                }
                Err(error)
                    if matches!(error.code(), "raw_range" | "out_of_range" | "invalid_raw") =>
                {
                    break None;
                }
                Err(error) => {
                    return Err(TestCaseError::fail(format!(
                        "{error} [{}]: {message:?}",
                        error.code()
                    )));
                }
            }
        };
        let Some((encoded, members)) = encoded else {
            continue;
        };
        let meta: Vec<(&str, MetaValue<'_>)> = encoded
            .metadata
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_meta()))
            .collect();
        let again = decode(&sets, &encoded.payload, &meta, message, &bindings)?;
        prop_assert_eq!(&again.packet.name, &decoded.packet.name);
        prop_assert_eq!(again.samples.len(), decoded.samples.len());
        for (back, given) in again.samples.iter().zip(&decoded.samples) {
            prop_assert_eq!(back.param, given.param);
            prop_assert_eq!(
                &back.path,
                &given.path,
                "{} encoded as {}",
                shown(&message.payload, json),
                shown(&encoded.payload, json)
            );
            // A value in a temperature unit, or a member given through its
            // composite, went through an affine conversion (Cel, K, [degF]):
            // beside the offset, a value smaller than its rounding is lost,
            // so it comes back that rounding away.
            let through = members.contains(&given.path)
                && !matches!(
                    given.value,
                    SampleValue::Vec3 { .. } | SampleValue::Quat { .. }
                );
            let unit = &case.bundle["catalog"]["params"][given.param]["unit"];
            let affine = matches!(unit.as_str(), Some("K" | "Cel" | "[degF]"));
            prop_assert!(
                close(&back.value, &given.value, through || affine),
                "{} at {}: {:?} came back {:?}, payload {:02x?}",
                given.param,
                given.path,
                given.value,
                back.value,
                encoded.payload
            );
        }
    }
    Ok(())
}

/// The names of the fields a set's conditions, counts and lengths read.
fn read_fields(value: &serde_json::Value, read: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            let kind = map.get("kind").and_then(serde_json::Value::as_str);
            if let (Some("field" | "compare" | "in"), Some(field)) =
                (kind, map.get("field").and_then(serde_json::Value::as_str))
            {
                read.insert(field.to_string());
            }
            map.values().for_each(|inner| read_fields(inner, read));
        }
        serde_json::Value::Array(items) => items.iter().for_each(|inner| read_fields(inner, read)),
        _ => {}
    }
}

/// Sample values equal, or numbers a rounding apart; through an affine
/// unit, a rounding of its offset apart (Kelvin's 273.15 and Fahrenheit's
/// 459.67 are under 500).
fn close(a: &SampleValue, b: &SampleValue, affine: bool) -> bool {
    let near = |x: f64, y: f64| {
        let floor = if affine { 500.0 } else { 0.0 };
        x == y || (x - y).abs() <= 1e-9 * (x.abs().max(y.abs()) + floor)
    };
    match (a, b) {
        (SampleValue::F64(x), SampleValue::F64(y)) => near(*x, *y),
        (
            SampleValue::Vec3 { frame, x, y, z },
            SampleValue::Vec3 {
                frame: f,
                x: x2,
                y: y2,
                z: z2,
            },
        ) => frame == f && near(*x, *x2) && near(*y, *y2) && near(*z, *z2),
        (
            SampleValue::Quat { frame, w, x, y, z },
            SampleValue::Quat {
                frame: f,
                w: w2,
                x: x2,
                y: y2,
                z: z2,
            },
        ) => frame == f && near(*w, *w2) && near(*x, *x2) && near(*y, *y2) && near(*z, *z2),
        _ => a == b,
    }
}

/// Raw values without the ones samples give: `None` when an array is
/// partly samples and partly not, which raw values can't leave out.
fn pruned(
    raw: Vec<(String, RawValue)>,
    prefix: &str,
    mapped: &BTreeSet<String>,
) -> Option<Vec<(String, RawValue)>> {
    let mut kept = Vec::new();
    for (name, value) in raw {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        if let Some(value) = pruned_value(value, &path, mapped)? {
            kept.push((name, value));
        }
    }
    Some(kept)
}

fn pruned_value(
    value: RawValue,
    path: &str,
    mapped: &BTreeSet<String>,
) -> Option<Option<RawValue>> {
    match value {
        RawValue::Record(entries) => Some(Some(RawValue::Record(pruned(entries, path, mapped)?))),
        RawValue::List(items) => {
            let records = items
                .iter()
                .all(|item| matches!(item, RawValue::Record(_) | RawValue::Null))
                && items.iter().any(|item| matches!(item, RawValue::Record(_)));
            if records {
                let mut kept = Vec::with_capacity(items.len());
                for (index, item) in items.into_iter().enumerate() {
                    kept.push(match item {
                        RawValue::Record(entries) => {
                            RawValue::Record(pruned(entries, &format!("{path}[{index}]"), mapped)?)
                        }
                        other => other,
                    });
                }
                return Some(Some(RawValue::List(kept)));
            }
            let given = (0..items.len())
                .filter(|index| mapped.contains(&format!("{path}[{index}]")))
                .count();
            match given {
                0 => Some(Some(RawValue::List(items))),
                all if all == items.len() => Some(None),
                _ => None,
            }
        }
        leaf if mapped.contains(path) => {
            let _ = leaf;
            Some(None)
        }
        leaf => Some(Some(leaf)),
    }
}

/// Every message that decodes encodes back from its record; a set whose
/// time source reads where no entry is needs the event time instead.
fn round_trip(case: &layout::Telemetry) -> Result<(), TestCaseError> {
    let sets = artifacts::load(&case.bundle, &case.family, &[])?;
    let bindings = bindings(case);
    let time = &case.bundle["sets"][0]["time"]["kind"];
    let timeless = time == "cuc" || time == "iso8601";
    let json = case.bundle["families"][0]["encoding"] == "json";
    for message in &case.messages {
        let metadata = metadata_of(message);
        let Ok(decoded) = sets.decode(&message_of(message).with_metadata(&metadata), &bindings)
        else {
            continue;
        };
        let raw = record_raw(&decoded, &checksums(case, &message.packet))?;
        let mut request = TelemetryRequest::new(
            &case.set,
            &message.packet,
            NativeId::from_text(case.native_id.clone()),
        );
        request.raw = raw;
        let encoded = sets.encode_telemetry(&request);
        // A time source that reads where no entry is needs the event time.
        if let Err(error) = &encoded
            && timeless
            && error.code() == "missing_value"
            && error.to_string().contains("needs the event time")
        {
            continue;
        }
        let encoded = encoded.map_err(|error| {
            TestCaseError::fail(format!("{error} [{}]: {message:?}", error.code()))
        })?;
        let meta: Vec<(&str, MetaValue<'_>)> = encoded
            .metadata
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_meta()))
            .collect();
        let again = decode(&sets, &encoded.payload, &meta, message, &bindings)?;
        prop_assert_eq!(&again.packet.name, &decoded.packet.name);
        prop_assert_eq!(&again.route.native_id, &decoded.route.native_id);
        let checksums = checksums(case, &message.packet);
        // Every entry's raw value but a checksum's, which covered random
        // bytes, is what the testkit packed.
        let shown = |record: Vec<(String, layout::Raw)>| {
            let text: Vec<String> = record
                .into_iter()
                .filter(|(name, _)| !checksums.contains(name))
                .map(|(name, value)| format!("{name}: {value:?}"))
                .collect();
            positive(text.join(", "), json)
        };
        prop_assert_eq!(
            shown(artifacts::record(&again.record)),
            shown(message.record.clone()),
            "payload {:02x?}",
            encoded.payload
        );
        prop_assert_eq!(
            format!("{:?}", encoded.record),
            format!("{:?}", again.record)
        );
        prop_assert_eq!(
            positive(format!("{:?}", again.samples), json),
            positive(format!("{:?}", decoded.samples), json)
        );
    }
    Ok(())
}

/// A payload as a failing case shows it: JSON as its text.
fn shown(payload: &[u8], json: bool) -> String {
    if json {
        String::from_utf8_lossy(payload).into_owned()
    } else {
        format!("{payload:02x?}")
    }
}

/// A JSON payload's zeros, which it writes positive.
fn positive(text: String, json: bool) -> String {
    if json {
        text.replace("-0.0", "0.0")
    } else {
        text
    }
}

fn decode<'s>(
    sets: &'s FamilySets,
    payload: &[u8],
    metadata: &[(&str, MetaValue<'_>)],
    message: &layout::Message,
    bindings: &Bindings,
) -> Result<Decoded<'s>, TestCaseError> {
    let received = Timestamp::from_unix_nanos(i64::try_from(message.received).unwrap());
    let payload = match message.encoding {
        layout::Encoding::Json => Payload::Json(payload),
        layout::Encoding::Binary => Payload::Binary(payload),
    };
    sets.decode(
        &Message::new(payload, received).with_metadata(metadata),
        bindings,
    )
    .map_err(|error| TestCaseError::fail(format!("the encoded payload: {error}")))
}

/// A decoded record as the raw values a request gives, but its checksums.
fn record_raw(
    decoded: &Decoded<'_>,
    checksums: &[String],
) -> Result<Vec<(String, RawValue)>, TestCaseError> {
    decoded
        .record
        .iter()
        .filter(|(name, _)| !checksums.iter().any(|checksum| checksum == name))
        .map(|(name, value)| Ok((name.to_string(), raw(value)?)))
        .collect()
}

/// The names of a packet's checksums.
fn checksums(case: &layout::Telemetry, packet: &str) -> Vec<String> {
    case.bundle["sets"][0]["packets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|candidate| candidate["name"] == packet)
        .flat_map(|candidate| candidate["entries"].as_array().cloned().unwrap_or_default())
        .filter(|entry| entry["kind"] == "checksum")
        .filter_map(|entry| entry["name"].as_str().map(str::to_string))
        .collect()
}

fn raw(value: &Value<'_>) -> Result<RawValue, TestCaseError> {
    Ok(match value {
        Value::Uint(n) => RawValue::Integer(i128::from(*n)),
        Value::Int(n) => RawValue::Integer(i128::from(*n)),
        Value::Float(x) => RawValue::Float(*x),
        Value::Bool(b) => RawValue::Bool(*b),
        Value::Str(text) => RawValue::Text(text.clone()),
        Value::Bytes(bytes) => RawValue::Bytes(bytes.clone()),
        Value::Array(items) => RawValue::List(items.iter().map(raw).collect::<Result<_, _>>()?),
        Value::Record(record) => RawValue::Record(
            record
                .iter()
                .map(|(name, value)| Ok((name.to_string(), raw(value)?)))
                .collect::<Result<_, TestCaseError>>()?,
        ),
        Value::Null => RawValue::Null,
        other => return Err(TestCaseError::fail(format!("a raw value: {other:?}"))),
    })
}

/// The binding of the case's native id to its set, as the layout tests
/// write it.
fn bindings(case: &layout::Telemetry) -> Bindings {
    let native_id = match case.native_id.parse::<i64>() {
        Ok(n) => n.to_string(),
        Err(_) => format!("\"{}\"", case.native_id),
    };
    Bindings::from_toml(&format!(
        "mdl_version = \"2.0\"\n\n[[binding]]\nfamily = \"{}\"\nnative_id = {native_id}\n\
         asset = \"ASSET\"\nset = \"{}\"\neffective_from = \"1980-01-06T00:00:00Z\"\n",
        case.family, case.set
    ))
    .unwrap()
}

fn message_of(message: &layout::Message) -> Message<'_> {
    let received = Timestamp::from_unix_nanos(i64::try_from(message.received).unwrap());
    let payload = match message.encoding {
        layout::Encoding::Json => Payload::Json(&message.payload),
        layout::Encoding::Binary => Payload::Binary(&message.payload),
    };
    Message::new(payload, received)
}

fn metadata_of(message: &layout::Message) -> Vec<(&str, MetaValue<'_>)> {
    message
        .metadata
        .iter()
        .map(|(name, value)| {
            let value = match value {
                layout::Meta::Uint(n) => MetaValue::Uint(*n),
                layout::Meta::Int(n) => MetaValue::Int(*n),
                layout::Meta::Text(text) => MetaValue::Text(text),
            };
            (name.as_str(), value)
        })
        .collect()
}
