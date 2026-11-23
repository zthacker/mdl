//! The runtime's results as the Elixir terms `spec/api.md` §1 gives a
//! binding: structs with atom fields for the results' fixed shapes, atoms
//! for MDL's own enumerations (a quality, a frame, an error's code, an
//! event time's kind, a time unit), and strings for every name, path,
//! unit and key that comes from definitions or messages, so no input can
//! grow the atom table.

use mdl::bindings::NativeId;
use mdl::decode::{EventTime, SampleValue};
use mdl::runtime::{
    ApiError, BindingAt, DecodedCommand, DecodedMessage, EncodedCommand, Problem, Sample, Value,
};
use mdl::time::Timestamp;
use rustler::types::NewBinary;
use rustler::{Atom, Encoder, Env, Error, NifResult, Term};

pub(crate) mod atoms {
    rustler::atoms! {
        ok,
        error,
        nil,
        true_ = "true",
        false_ = "false",
        nan,
        infinity,
        neg_infinity,
        bytes,
        ground,
        at,
        since_boot,
        struct_ = "__struct__",
        exception = "__exception__",
        // Inputs' keys.
        payload,
        metadata,
        ground_time,
        set,
        command,
        arguments,
        native_id,
        // Results' fields.
        family,
        asset,
        stand_in,
        def_version,
        packet,
        event_time,
        booted,
        samples,
        record,
        engineering,
        param,
        value,
        unit,
        path,
        index,
        time_offset_ns,
        quality,
        defaulted,
        deprecated,
        target,
        code,
        message,
        problems,
        frame,
        w,
        x,
        y,
        z,
        // Modules.
        decoded_module = "Elixir.MDL.Decoded",
        sample_module = "Elixir.MDL.Sample",
        encoded_module = "Elixir.MDL.Encoded",
        decoded_command_module = "Elixir.MDL.DecodedCommand",
        route_module = "Elixir.MDL.Route",
        error_module = "Elixir.MDL.Error",
        problem_module = "Elixir.MDL.Problem",
        vec3_module = "Elixir.MDL.Vec3",
        quat_module = "Elixir.MDL.Quat",
        argument_error = "Elixir.ArgumentError",
        runtime_error = "Elixir.RuntimeError",
    }
}

/// `{:ok, term}` for a result, or `{:error, %MDL.Error{}}`.
pub(crate) fn result<'a, T>(
    env: Env<'a>,
    result: Result<T, ApiError>,
    ok: impl FnOnce(Env<'a>, &T) -> NifResult<Term<'a>>,
) -> NifResult<Term<'a>> {
    match result {
        Ok(found) => Ok((atoms::ok(), ok(env, &found)?).encode(env)),
        Err(error) => error_tuple(env, &error),
    }
}

/// `{:error, %MDL.Error{}}`.
pub(crate) fn error_tuple<'a>(env: Env<'a>, error: &ApiError) -> NifResult<Term<'a>> {
    Ok((atoms::error(), self::error(env, error)?).encode(env))
}

/// `%MDL.Error{}` (`spec/api.md` §4), an exception.
fn error<'a>(env: Env<'a>, error: &ApiError) -> NifResult<Term<'a>> {
    let problems = error
        .problems
        .iter()
        .map(|problem| self::problem(env, problem))
        .collect::<NifResult<Vec<Term<'a>>>>()?;
    structure(
        env,
        atoms::error_module(),
        &[
            (atoms::exception(), true.encode(env)),
            (atoms::code(), code(env, error.code)?),
            (atoms::message(), error.message.encode(env)),
            (atoms::problems(), problems.encode(env)),
        ],
    )
}

/// `%MDL.Problem{}`.
pub(crate) fn problem<'a>(env: Env<'a>, problem: &Problem) -> NifResult<Term<'a>> {
    structure(
        env,
        atoms::problem_module(),
        &[
            (atoms::code(), code(env, problem.code)?),
            (atoms::message(), problem.message.encode(env)),
            (atoms::path(), problem.path.encode(env)),
        ],
    )
}

/// `%MDL.Decoded{}` (`spec/api.md` §3.1).
pub(crate) fn decoded<'a>(env: Env<'a>, decoded: &DecodedMessage) -> NifResult<Term<'a>> {
    let samples = decoded
        .samples
        .iter()
        .map(|sample| self::sample(env, sample))
        .collect::<NifResult<Vec<Term<'a>>>>()?;
    let engineering = decoded
        .engineering
        .iter()
        .map(|(path, engineering)| {
            let value = sample_value(env, &engineering.value)?;
            let quality = atom(env, engineering.quality.as_str())?;
            Ok((path.as_str(), value, quality).encode(env))
        })
        .collect::<NifResult<Vec<Term<'a>>>>()?;
    structure(
        env,
        atoms::decoded_module(),
        &[
            (atoms::family(), decoded.family.encode(env)),
            (atoms::native_id(), native_id(env, &decoded.native_id)),
            (atoms::asset(), decoded.asset.encode(env)),
            (atoms::stand_in(), decoded.stand_in.encode(env)),
            (atoms::set(), decoded.set.encode(env)),
            (
                atoms::def_version(),
                decoded.def_version.to_string().encode(env),
            ),
            (atoms::packet(), decoded.packet.encode(env)),
            (
                atoms::event_time(),
                event_time(env, &decoded.event_time, decoded.ground_time)?,
            ),
            (
                atoms::booted(),
                match decoded.booted {
                    Some(at) => Value::unix_nanos(at).encode(env),
                    None => atoms::nil().encode(env),
                },
            ),
            (atoms::samples(), samples.encode(env)),
            (atoms::record(), value(env, &decoded.record)?),
            (atoms::engineering(), engineering.encode(env)),
        ],
    )
}

/// `%MDL.Sample{}`.
fn sample<'a>(env: Env<'a>, sample: &Sample) -> NifResult<Term<'a>> {
    structure(
        env,
        atoms::sample_module(),
        &[
            (atoms::param(), sample.param.encode(env)),
            (atoms::value(), sample_value(env, &sample.value)?),
            (atoms::unit(), optional(env, sample.unit.as_deref())),
            (atoms::path(), sample.path.encode(env)),
            (atoms::index(), optional(env, sample.index)),
            (
                atoms::time_offset_ns(),
                optional(env, sample.time_offset_ns),
            ),
            (atoms::quality(), atom(env, sample.quality.as_str())?),
        ],
    )
}

/// `{:ground, ns}`, `{:at, ns}` or `{:since_boot, value, unit}`.
fn event_time<'a>(
    env: Env<'a>,
    event_time: &EventTime,
    ground_time: Timestamp,
) -> NifResult<Term<'a>> {
    match event_time {
        EventTime::Ground => Ok((atoms::ground(), Value::unix_nanos(ground_time)).encode(env)),
        EventTime::At(at) => Ok((atoms::at(), Value::unix_nanos(*at)).encode(env)),
        EventTime::SinceBoot { value, unit } => {
            Ok((atoms::since_boot(), *value, atom(env, unit.as_str())?).encode(env))
        }
        _ => Err(unknown_form("event time")),
    }
}

/// A sample's or engineering value's value (`spec/api.md` §3.1).
fn sample_value<'a>(env: Env<'a>, value: &SampleValue) -> NifResult<Term<'a>> {
    match value {
        SampleValue::F64(x) => Ok(float(env, *x)),
        SampleValue::I64(n) => Ok(n.encode(env)),
        SampleValue::U64(n) => Ok(n.encode(env)),
        SampleValue::Bool(b) => Ok(b.encode(env)),
        SampleValue::Str(text) | SampleValue::Enum(text) => Ok(text.encode(env)),
        SampleValue::Bytes(bytes) => Ok(binary(env, bytes)),
        SampleValue::Vec3 { frame, x, y, z } => structure(
            env,
            atoms::vec3_module(),
            &[
                (atoms::frame(), frame_atom(env, frame.as_str())?),
                (atoms::x(), float(env, *x)),
                (atoms::y(), float(env, *y)),
                (atoms::z(), float(env, *z)),
            ],
        ),
        SampleValue::Quat { frame, w, x, y, z } => structure(
            env,
            atoms::quat_module(),
            &[
                (atoms::frame(), frame_atom(env, frame.as_str())?),
                (atoms::w(), float(env, *w)),
                (atoms::x(), float(env, *x)),
                (atoms::y(), float(env, *y)),
                (atoms::z(), float(env, *z)),
            ],
        ),
        _ => Err(unknown_form("value")),
    }
}

/// `%MDL.Encoded{}` (`spec/api.md` §3.2).
pub(crate) fn encoded<'a>(env: Env<'a>, encoded: &EncodedCommand) -> NifResult<Term<'a>> {
    let metadata = encoded
        .metadata
        .iter()
        .map(|(name, meta)| Ok((name.as_str().encode(env), value(env, meta)?)))
        .collect::<NifResult<Vec<(Term<'a>, Term<'a>)>>>()?;
    structure(
        env,
        atoms::encoded_module(),
        &[
            (atoms::family(), encoded.family.encode(env)),
            (atoms::set(), encoded.set.encode(env)),
            (
                atoms::def_version(),
                encoded.def_version.to_string().encode(env),
            ),
            (atoms::packet(), encoded.packet.encode(env)),
            (atoms::command(), encoded.command.encode(env)),
            (atoms::payload(), binary(env, &encoded.payload)),
            (atoms::metadata(), Term::map_from_pairs(env, &metadata)?),
            (atoms::defaulted(), encoded.defaulted.encode(env)),
            (
                atoms::deprecated(),
                optional(env, encoded.deprecated.as_deref()),
            ),
            (atoms::record(), value(env, &encoded.record)?),
        ],
    )
}

/// `%MDL.DecodedCommand{}` (`spec/api.md` §3.5).
pub(crate) fn decoded_command<'a>(env: Env<'a>, decoded: &DecodedCommand) -> NifResult<Term<'a>> {
    let arguments = decoded
        .arguments
        .iter()
        .map(|(name, argument)| Ok((name.as_str().encode(env), value(env, argument)?)))
        .collect::<NifResult<Vec<(Term<'a>, Term<'a>)>>>()?;
    let target = match &decoded.target {
        Some(id) => native_id(env, id),
        None => atoms::nil().encode(env),
    };
    structure(
        env,
        atoms::decoded_command_module(),
        &[
            (atoms::family(), decoded.family.encode(env)),
            (atoms::set(), decoded.set.encode(env)),
            (
                atoms::def_version(),
                decoded.def_version.to_string().encode(env),
            ),
            (atoms::packet(), decoded.packet.encode(env)),
            (atoms::command(), decoded.command.encode(env)),
            (atoms::arguments(), Term::map_from_pairs(env, &arguments)?),
            (atoms::defaulted(), decoded.defaulted.encode(env)),
            (atoms::target(), target),
            (atoms::record(), value(env, &decoded.record)?),
        ],
    )
}

/// `%MDL.Route{}` (`spec/api.md` §3.4), or `nil` when there's none.
pub(crate) fn route_or_nil<'a>(env: Env<'a>, found: &Option<BindingAt>) -> NifResult<Term<'a>> {
    let Some(binding) = found else {
        return Ok(atoms::nil().encode(env));
    };
    structure(
        env,
        atoms::route_module(),
        &[
            (atoms::family(), binding.family.encode(env)),
            (atoms::native_id(), native_id(env, &binding.native_id)),
            (atoms::asset(), binding.asset.encode(env)),
            (atoms::set(), optional(env, binding.set.as_deref())),
            (atoms::stand_in(), binding.stand_in.encode(env)),
        ],
    )
}

/// A value of `spec/api.md` §1 as a binding returns it; an object is a map
/// keyed by strings, since its keys come from definitions or messages.
pub(crate) fn value<'a>(env: Env<'a>, value: &Value) -> NifResult<Term<'a>> {
    match value {
        Value::Integer(n) => Ok(n.encode(env)),
        Value::Float(x) => Ok(float(env, *x)),
        Value::Bool(b) => Ok(b.encode(env)),
        Value::Text(text) => Ok(text.encode(env)),
        Value::Bytes(bytes) => Ok(binary(env, bytes)),
        Value::Instant(at) => Ok(Value::unix_nanos(*at).encode(env)),
        Value::Absent => Ok(atoms::nil().encode(env)),
        Value::List(items) => {
            let items = items
                .iter()
                .map(|item| self::value(env, item))
                .collect::<NifResult<Vec<Term<'a>>>>()?;
            Ok(items.encode(env))
        }
        Value::Object(entries) => {
            let entries = entries
                .iter()
                .map(|(key, item)| Ok((key.as_str().encode(env), self::value(env, item)?)))
                .collect::<NifResult<Vec<(Term<'a>, Term<'a>)>>>()?;
            Term::map_from_pairs(env, &entries)
        }
    }
}

/// A float, or `:nan`, `:infinity` or `:neg_infinity`, which the BEAM's
/// floats can't hold.
fn float(env: Env<'_>, x: f64) -> Term<'_> {
    if x.is_nan() {
        atoms::nan().encode(env)
    } else if x == f64::INFINITY {
        atoms::infinity().encode(env)
    } else if x == f64::NEG_INFINITY {
        atoms::neg_infinity().encode(env)
    } else {
        x.encode(env)
    }
}

/// A native id: an integer, or a string.
fn native_id<'a>(env: Env<'a>, id: &NativeId) -> Term<'a> {
    match (id.as_int(), id.as_text()) {
        (Some(n), _) => n.encode(env),
        (None, Some(text)) => text.encode(env),
        // Every native id is one or the other.
        (None, None) => atoms::nil().encode(env),
    }
}

fn binary<'a>(env: Env<'a>, bytes: &[u8]) -> Term<'a> {
    let mut binary = NewBinary::new(env, bytes.len());
    binary.as_mut_slice().copy_from_slice(bytes);
    binary.into()
}

fn optional<'a, T: Encoder>(env: Env<'a>, value: Option<T>) -> Term<'a> {
    match value {
        Some(value) => value.encode(env),
        None => atoms::nil().encode(env),
    }
}

/// A struct: a map of `fields` and `__struct__`.
fn structure<'a>(env: Env<'a>, module: Atom, fields: &[(Atom, Term<'a>)]) -> NifResult<Term<'a>> {
    let mut entries = Vec::with_capacity(fields.len() + 1);
    entries.push((atoms::struct_(), module.encode(env)));
    entries.extend_from_slice(fields);
    Term::map_from_pairs(env, &entries)
}

/// An error's or problem's code, as an atom: codes are the runtime's own,
/// a fixed set.
fn code<'a>(env: Env<'a>, code: &str) -> NifResult<Term<'a>> {
    atom(env, code)
}

/// A frame's atom: its IR name in lower case (`ECI_J2000` is
/// `:eci_j2000`), as Elixir spells atoms.
fn frame_atom<'a>(env: Env<'a>, name: &str) -> NifResult<Term<'a>> {
    atom(env, &name.to_ascii_lowercase())
}

/// One of MDL's own names as an atom: never a name from definitions or
/// messages.
fn atom<'a>(env: Env<'a>, name: &str) -> NifResult<Term<'a>> {
    Ok(Atom::from_str(env, name)?.encode(env))
}

/// A form this binding doesn't know, which a runtime release added: the
/// binding's conversion is behind its runtime, a bug in the binding.
fn unknown_form(what: &str) -> Error {
    raise(
        atoms::runtime_error(),
        format!(
            "MDL's runtime returned a {what} the Elixir binding doesn't know; the binding \
             needs updating for this runtime release"
        ),
    )
}

/// Raises the exception `module` (an Elixir exception with a message
/// field, such as `ArgumentError`) with `message`.
pub(crate) fn raise(module: Atom, message: impl Into<String>) -> Error {
    Error::RaiseTerm(Box::new(Exception {
        module,
        message: message.into(),
    }))
}

/// An Elixir exception, as the term a NIF raises.
struct Exception {
    module: Atom,
    message: String,
}

impl Encoder for Exception {
    fn encode<'a>(&self, env: Env<'a>) -> Term<'a> {
        let fields = [
            (atoms::struct_(), self.module.encode(env)),
            (atoms::exception(), true.encode(env)),
            (atoms::message(), self.message.encode(env)),
        ];
        // Three distinct keys always make a map; the message alone if not.
        Term::map_from_pairs(env, &fields).unwrap_or_else(|_| self.message.encode(env))
    }
}
