//! Targets for definitions the testkit generates from a seed in the input.
//! Coverage picks the seeds that reach new code, though one changed bit
//! gives an unrelated case. A later change to a generator changes what a
//! seed makes, so a crash found here is kept as a unit test of its
//! definition and payload, not as an input: the replay test refuses inputs
//! in `fuzz/regressions` for these targets.

use std::collections::BTreeSet;

use mdl::bindings::Bindings;
use mdl::message::{Message, MetaValue, Payload};
use mdl::read::read_bundle_value;
use mdl::route::FamilySets;
use mdl::time::Timestamp;
use mdl::validate::validate;
use mdl_testkit::layout::{self, command};
use mdl_testkit::shape;
use proptest::strategy::{Strategy, ValueTree};
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use serde_json::Value;

use crate::input::Input;
use crate::messages::{check_command_payload, check_decoded, check_error};
use crate::pipeline::{assemble, load};

/// A bundle from `mdl_testkit::shape::bundle()`, IR that reads but needn't
/// validate. Layout: a seed, 32 bytes. The bundle reads, as the testkit
/// says, and goes on as a consumer takes it.
pub fn shapes(data: &[u8]) {
    let mut input = Input::new(data);
    let Some(bundle) = generated(shape::bundle(), input.take(32)) else {
        return;
    };
    let loaded = match read_bundle_value(&bundle) {
        Ok(loaded) => loaded,
        Err(error) => panic!("the testkit's bundle didn't read (a testkit or reader bug): {error}"),
    };
    if let Err(diagnostics) = load(loaded.value) {
        for diagnostic in diagnostics.iter() {
            assert!(!diagnostic.message.is_empty(), "{diagnostic:?}");
        }
    }
}

/// A valid definition the testkit generates, and its own first payload,
/// damaged as the input says. Layout:
/// - what to generate: a byte, modulo 4, for binary telemetry, JSON
///   telemetry, binary commands or JSON commands;
/// - a seed, 32 bytes;
/// - the damage, as `damaged` reads it.
///
/// With no damage, the payload is the valid message, so every seed decodes
/// a whole one. Telemetry is decoded with the first message's metadata and
/// ground time, and what decodes passes `check_decoded`; a command payload
/// is decoded by the generated set with its request's metadata, and passes
/// `check_command_payload`.
pub fn layouts(data: &[u8]) {
    let mut input = Input::new(data);
    let kind = input.choose(4);
    let seed = input.take(32);
    match kind {
        0 | 1 => {
            let strategy = if kind == 0 {
                layout::telemetry()
            } else {
                layout::json_telemetry()
            };
            if let Some(case) = generated(strategy, seed) {
                telemetry(&case, input);
            }
        }
        _ => {
            let strategy = if kind == 2 {
                command::commands()
            } else {
                command::json_commands()
            };
            if let Some(case) = generated(strategy, seed) {
                commands(&case, input);
            }
        }
    }
}

/// A payload damaged as the input says: a cut, a byte, the bytes taken
/// off its end, modulo its length and one; an offset, two bytes,
/// little-endian, modulo the length left and one; then the rest of the
/// input, XORed in from the offset and running on past the end. All zeros
/// leave it as it was.
fn damaged(payload: &[u8], mut input: Input<'_>) -> Vec<u8> {
    let cut = usize::from(input.byte()) % (payload.len() + 1);
    let mut bytes = payload[..payload.len() - cut].to_vec();
    let offset = usize::from(input.u16()) % (bytes.len() + 1);
    for (index, byte) in input.rest().iter().enumerate() {
        match bytes.get_mut(offset + index) {
            Some(at) => *at ^= byte,
            None => bytes.push(*byte),
        }
    }
    bytes
}

fn telemetry(case: &layout::Telemetry, damage: Input<'_>) {
    let Some(first) = case.messages.first() else {
        return;
    };
    let payload = damaged(&first.payload, damage);
    let payload = payload.as_slice();
    let sets = family_sets(&case.bundle, &case.family);
    let bindings = binding(case);
    let metadata: Vec<(&str, MetaValue<'_>)> = first
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
        .collect();
    let Ok(received) = i64::try_from(first.received) else {
        return;
    };
    let payload = match first.encoding {
        layout::Encoding::Json => Payload::Json(payload),
        layout::Encoding::Binary => Payload::Binary(payload),
    };
    let message =
        Message::new(payload, Timestamp::from_unix_nanos(received)).with_metadata(&metadata);
    let first_time = sets.decode(&message, &bindings);
    let second_time = sets.decode(&message, &bindings);
    assert_eq!(
        format!("{first_time:?}"),
        format!("{second_time:?}"),
        "the same message decoded twice"
    );
    match first_time {
        Ok(decoded) => check_decoded(&decoded, &params(&case.bundle)),
        Err(error) => check_error(error.code(), &error),
    }
}

fn commands(case: &command::Commands, damage: Input<'_>) {
    let Some(first) = case.requests.first() else {
        return;
    };
    let sets = family_sets(&case.bundle, &case.family);
    let json = case.bundle["families"][0]["encoding"] == "json";
    let metadata: Vec<(&str, MetaValue<'_>)> = first
        .metadata
        .iter()
        .map(|(name, n)| (name.as_str(), MetaValue::Int(*n)))
        .collect();
    let payload = damaged(&first.payload, damage);
    let payload = if json {
        Payload::Json(&payload)
    } else {
        Payload::Binary(&payload)
    };
    let message = Message::new(payload, Timestamp::from_unix_nanos(0)).with_metadata(&metadata);
    check_command_payload(&sets, &case.set, json, &message);
}

/// The case `strategy` makes from a seed: the first 32 bytes given, zeros
/// past them.
fn generated<S: Strategy>(strategy: S, seed: &[u8]) -> Option<S::Value> {
    let mut bytes = [0; 32];
    bytes[..seed.len().min(32)].copy_from_slice(&seed[..seed.len().min(32)]);
    let rng = TestRng::from_seed(RngAlgorithm::ChaCha, &bytes);
    // `PROPTEST_MAX_LOCAL_REJECTS` would change what a filtered strategy
    // makes; pinned, the case depends on the seed alone.
    let config = Config {
        max_local_rejects: 65_536,
        ..Config::default()
    };
    let mut runner = TestRunner::new_with_rng(config, rng);
    strategy
        .new_tree(&mut runner)
        .ok()
        .map(|tree| tree.current())
}

/// A generated bundle's family, assembled; the testkit's layouts are
/// valid.
fn family_sets(bundle: &Value, family: &str) -> FamilySets {
    let read = match read_bundle_value(bundle) {
        Ok(loaded) => loaded.value,
        Err(error) => panic!("the testkit's layout didn't read (a testkit or reader bug): {error}"),
    };
    let valid = match validate(read) {
        Ok(valid) => valid,
        Err(diagnostics) => {
            panic!("the testkit's layout isn't valid (a testkit or validator bug): {diagnostics:?}")
        }
    };
    match assemble(&valid)
        .into_iter()
        .find(|(name, _)| name == family)
    {
        Some((_, sets)) => sets,
        None => panic!("the testkit's family {family} wasn't assembled"),
    }
}

/// The binding the testkit's case names: its native id to an asset of its
/// set.
fn binding(case: &layout::Telemetry) -> Bindings {
    let native_id = match case.native_id.parse::<i64>() {
        Ok(n) => n.to_string(),
        Err(_) => format!("\"{}\"", case.native_id),
    };
    let text = format!(
        "mdl_version = \"2.0\"\n\n[[binding]]\nfamily = \"{}\"\nnative_id = {native_id}\n\
         asset = \"ASSET\"\nset = \"{}\"\neffective_from = \"1980-01-06T00:00:00Z\"\n",
        case.family, case.set
    );
    match Bindings::from_toml(&text) {
        Ok(bindings) => bindings,
        Err(error) => panic!("the testkit's binding didn't read: {error}: {text}"),
    }
}

/// A generated bundle's catalog params.
fn params(bundle: &Value) -> BTreeSet<String> {
    bundle["catalog"]["params"]
        .as_object()
        .map(|params| params.keys().cloned().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::damaged;
    use crate::input::Input;

    #[test]
    fn no_damage_leaves_the_payload_as_it_was() {
        assert_eq!(damaged(b"abcd", Input::new(&[])), b"abcd");
        assert_eq!(damaged(b"abcd", Input::new(&[0; 6])), b"abcd");
    }

    #[test]
    fn damage_cuts_then_xors_from_the_offset_past_the_end() {
        // Cut one byte ("abc"), start at 2, flip 'c' to 'b', then add 'z'.
        assert_eq!(
            damaged(b"abcd", Input::new(&[1, 2, 0, 0x01, b'z'])),
            b"abbz"
        );
    }
}
