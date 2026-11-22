//! The Elixir binding's native half: the runtime API (`spec/api.md`) as
//! NIFs for `MDL` (`lib/mdl.ex`). `input` reads Elixir terms into the
//! runtime's inputs and `output` writes its results as the terms
//! `spec/api.md` §1 gives Elixir; no MDL semantics live here.
//!
//! Every NIF that does MDL work runs on a dirty CPU scheduler. Its cost
//! depends on the definitions and the input (a dense JSON payload decodes
//! at about 180 µs a KiB, a packet's every element becomes a sample), so no
//! size limit could keep it under the millisecond a normal scheduler
//! allows; a dirty call costs a few microseconds more, which `decode_many`
//! spreads over a batch.
//!
//! Every MDL error is returned as `{:error, %MDL.Error{}}`. A malformed
//! input (a payload that isn't a binary, a metadata key that isn't a
//! string) raises `ArgumentError`, as Elixir functions do for arguments
//! of the wrong type.

mod input;
mod output;

use std::path::Path;

use mdl::bindings::Bindings;
use mdl::message::Message;
use mdl::runtime::{ApiError, Runtime};
use rustler::{Binary, Encoder, Env, NifResult, ResourceArc, Term};

use crate::input::MessageInput;
use crate::output::atoms;

/// An opened store (`spec/api.md` §2.1). The runtime is immutable and
/// `Send + Sync`, so one handle serves any number of processes at once.
struct Handle(Runtime);

#[rustler::resource_impl]
impl rustler::Resource for Handle {}

rustler::init!("Elixir.MDL.Native");

/// The store in `dir`, with the bindings file's text if any: its files
/// read, and the build validated, which is most of the work.
#[rustler::nif(schedule = "DirtyCpu")]
fn open<'a>(env: Env<'a>, dir: String, bindings: Option<String>) -> NifResult<Term<'a>> {
    let opened = Runtime::open(Path::new(&dir), bindings.as_deref());
    handle_result(env, opened)
}

/// A store from its files' bytes: the manifest's and each artifact's.
#[rustler::nif(schedule = "DirtyCpu")]
fn open_bytes<'a>(
    env: Env<'a>,
    manifest: Binary<'a>,
    artifacts: Vec<Binary<'a>>,
    bindings: Option<String>,
) -> NifResult<Term<'a>> {
    let artifacts: Vec<&[u8]> = artifacts
        .iter()
        .map(|artifact| artifact.as_slice())
        .collect();
    let opened = Runtime::open_bytes(manifest.as_slice(), &artifacts, bindings.as_deref());
    handle_result(env, opened)
}

/// What the handle's bindings say that its build doesn't have.
#[rustler::nif]
fn warnings<'a>(env: Env<'a>, handle: ResourceArc<Handle>) -> NifResult<Term<'a>> {
    let problems = handle
        .0
        .warnings()
        .iter()
        .map(|problem| output::problem(env, problem))
        .collect::<NifResult<Vec<Term<'a>>>>()?;
    Ok(problems.encode(env))
}

/// The handle's build with other bindings, or none, without assembling
/// the build again; the bindings are checked against the build.
#[rustler::nif(schedule = "DirtyCpu")]
fn with_bindings<'a>(
    env: Env<'a>,
    handle: ResourceArc<Handle>,
    bindings: Option<String>,
) -> NifResult<Term<'a>> {
    let bindings = bindings
        .as_deref()
        .map(Bindings::from_toml)
        .transpose()
        .map_err(ApiError::from);
    handle_result(
        env,
        bindings.map(|bindings| handle.0.with_bindings(bindings)),
    )
}

/// One message `family` sent, decoded (`spec/api.md` §2.2).
#[rustler::nif(schedule = "DirtyCpu")]
fn decode<'a>(
    env: Env<'a>,
    handle: ResourceArc<Handle>,
    family: String,
    message: Term<'a>,
) -> NifResult<Term<'a>> {
    let input = input::message(message)?;
    let runtime = &handle.0;
    let decoded = runtime.payload(&family, input.bytes()).and_then(|payload| {
        let metadata = input.metadata();
        runtime.decode(&family, &input.message(payload, &metadata))
    });
    output::result(env, decoded, output::decoded)
}

/// Each message, decoded or why not, in order (`spec/api.md` §2.3).
#[rustler::nif(schedule = "DirtyCpu")]
fn decode_many<'a>(
    env: Env<'a>,
    handle: ResourceArc<Handle>,
    family: String,
    messages: Vec<Term<'a>>,
) -> NifResult<Term<'a>> {
    let inputs = messages
        .into_iter()
        .map(input::message)
        .collect::<NifResult<Vec<MessageInput<'a>>>>()?;
    let runtime = &handle.0;
    let payloads = match inputs
        .iter()
        .map(|input| runtime.payload(&family, input.bytes()))
        .collect::<Result<Vec<_>, ApiError>>()
    {
        Ok(payloads) => payloads,
        // Only an unknown family fails here: each message is refused as
        // decoding it alone would be.
        Err(error) => {
            let refused = output::error_tuple(env, &error)?;
            return Ok(vec![refused; inputs.len()].encode(env));
        }
    };
    let metadata: Vec<_> = inputs.iter().map(MessageInput::metadata).collect();
    let messages: Vec<Message<'_>> = inputs
        .iter()
        .zip(payloads)
        .zip(&metadata)
        .map(|((input, payload), metadata)| input.message(payload, metadata))
        .collect();
    let results = runtime
        .decode_many(&family, &messages)
        .into_iter()
        .map(|decoded| output::result(env, decoded, output::decoded))
        .collect::<NifResult<Vec<Term<'a>>>>()?;
    Ok(results.encode(env))
}

/// A command request, encoded by one of `family`'s sets (`spec/api.md`
/// §2.4).
#[rustler::nif(schedule = "DirtyCpu")]
fn encode<'a>(
    env: Env<'a>,
    handle: ResourceArc<Handle>,
    family: String,
    request: Term<'a>,
) -> NifResult<Term<'a>> {
    let request = input::request(request)?;
    output::result(env, handle.0.encode(&family, &request), output::encoded)
}

/// A command payload, decoded by one of `family`'s sets (`spec/api.md`
/// §2.7).
#[rustler::nif(schedule = "DirtyCpu")]
fn decode_command<'a>(
    env: Env<'a>,
    handle: ResourceArc<Handle>,
    family: String,
    message: Term<'a>,
) -> NifResult<Term<'a>> {
    let (set, input) = input::command_message(message)?;
    let runtime = &handle.0;
    let decoded = runtime.payload(&family, input.bytes()).and_then(|payload| {
        let metadata = input.metadata();
        runtime.decode_command(&family, &set, &input.message(payload, &metadata))
    });
    output::result(env, decoded, output::decoded_command)
}

/// The build, described (`spec/api.md` §2.5).
#[rustler::nif(schedule = "DirtyCpu")]
fn describe<'a>(env: Env<'a>, handle: ResourceArc<Handle>) -> NifResult<Term<'a>> {
    let described = handle
        .0
        .describe()
        .and_then(|description| description.to_value());
    output::result(env, described, output::value)
}

/// What a native id of `family` is at an instant (`spec/api.md` §2.6).
#[rustler::nif(schedule = "DirtyCpu")]
fn binding<'a>(
    env: Env<'a>,
    handle: ResourceArc<Handle>,
    family: String,
    native_id: Term<'a>,
    at: Term<'a>,
) -> NifResult<Term<'a>> {
    let native_id = input::native_id(native_id)?;
    let at = input::instant(at, "the instant")?;
    let found = handle.0.binding(&family, &native_id, at);
    output::result(env, found, output::route_or_nil)
}

/// The native id that is an asset of `family` at an instant, with its
/// binding (`spec/api.md` §2.6).
#[rustler::nif(schedule = "DirtyCpu")]
fn asset<'a>(
    env: Env<'a>,
    handle: ResourceArc<Handle>,
    family: String,
    name: String,
    at: Term<'a>,
) -> NifResult<Term<'a>> {
    let at = input::instant(at, "the instant")?;
    let found = handle.0.asset(&family, &name, at);
    output::result(env, found, output::route_or_nil)
}

/// `{:ok, handle}` for an opened runtime, or `{:error, error}`.
fn handle_result<'a>(env: Env<'a>, opened: Result<Runtime, ApiError>) -> NifResult<Term<'a>> {
    match opened {
        Ok(runtime) => Ok((atoms::ok(), ResourceArc::new(Handle(runtime))).encode(env)),
        Err(error) => output::error_tuple(env, &error),
    }
}
