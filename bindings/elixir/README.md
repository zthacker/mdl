# MDL for Elixir

MDL's runtime as an Elixir library: open an artifact store `morphc`
built, decode telemetry, encode commands and decode them back, describe
the build, look up which asset a native id is, and turn an event time
into an instant. It calls the runtime every binding calls
([spec/api.md](../../spec/api.md)) through a native library built with
[Rustler](https://github.com/rusterlium/rustler), and holds no MDL
semantics of its own, so it decodes and encodes exactly as `morphc` does.

## Installing

The binding is on Hex as [`mdl`](https://hex.pm/packages/mdl). It needs
Elixir 1.18 or later:

```elixir
def deps do
  [
    {:mdl, "~> 0.2.0"}
  ]
end
```

The package holds no native source, so installing needs no Rust. When
the app compiles, it downloads the release's archive of the native
library for the platform, and uses it only if the archive's SHA-256 is
one the package lists. Each release builds it for Linux on x86_64 and
aarch64 (glibc 2.28 or later, or musl) and macOS (10.12 or later on
Intel, 11.0 or later on Apple silicon), and runs the binding's tests on
each. For a build without network access, put the archive, under the
release's name for it (`libmdl_nif-vX.Y.Z-nif-2.15-<target>.so.tar.gz`),
in a directory that `RUSTLER_PRECOMPILED_GLOBAL_CACHE_PATH` names.

On Alpine the library needs `libgcc`, which it uses to turn a panic into
an exception; Alpine's Erlang needs it too, so a system that runs the
BEAM has it.

On any other platform, or for a commit between releases, depend on this
repository instead, at a release's tag or a commit, with Rustler to
build the native library from source:

```elixir
def deps do
  [
    {:mdl, github: "zthacker/mdl", subdir: "bindings/elixir", tag: "v0.2.0"},
    {:rustler, "~> 0.38", runtime: false}
  ]
end
```

That needs Rust 1.91 or later wherever the app compiles. Nothing tests
the binding on platforms other than those above.

## Using it

A ground system compiles its definitions into a store with `morphc` (in
CI, say) and opens the store at run time:

```elixir
{:ok, mdl} = MDL.open("store", bindings: File.read!("bindings.toml"))

{:ok, decoded} =
  MDL.decode(mdl, "sat", %{
    payload: payload,
    metadata: %{"apid" => 100},
    ground_time: System.os_time(:nanosecond)
  })

for %MDL.Sample{param: param, value: value, quality: :good} <- decoded.samples do
  {param, value}
end

{:ok, command} =
  MDL.encode(mdl, "sat", %{
    set: "sat_v1",
    command: "eps.set_mode",
    arguments: %{"mode" => "SAFE"}
  })

# command.payload is what the link sends, framed with command.metadata.
```

| Function | Does |
| --- | --- |
| `MDL.open/2`, `MDL.open_bytes/3` | Open a store from its directory, or from its files' bytes, with a bindings file's text or without |
| `MDL.warnings/1` | What the bindings say that the build doesn't have |
| `MDL.with_bindings/2` | The same build with other bindings, without reading it again |
| `MDL.decode/3`, `MDL.decode_many/3` | Decode one message, or many, each to an `MDL.Decoded` or an error |
| `MDL.encode/3` | Encode a catalog command as a set's command packet, an `MDL.Encoded` |
| `MDL.decode_command/3` | Decode a command payload back into its catalog command and arguments, an `MDL.DecodedCommand` |
| `MDL.instant/2` | The instant an event time is, from the boot instant the caller keeps for its asset, or `nil` |
| `MDL.describe/1` | The catalog, families and sets as the IR has them, and each binary set's layout |
| `MDL.binding/4`, `MDL.asset/4` | What a native id is at an instant, and which native id an asset is |

Every function returns `{:ok, result}` or `{:error, %MDL.Error{}}`, whose
`:code` atom names the problem and is stable across releases (match on it,
not on the message). An argument of the wrong shape, such as a payload
that isn't a binary, raises `ArgumentError`.

## Values

| Value | In Elixir |
| --- | --- |
| A decoded message, sample, encoded command, decoded command, route, error, problem | `%MDL.Decoded{}`, `%MDL.Sample{}`, `%MDL.Encoded{}`, `%MDL.DecodedCommand{}`, `%MDL.Route{}`, `%MDL.Error{}`, `%MDL.Problem{}` |
| A vector's or quaternion's value | `%MDL.Vec3{frame:, x:, y:, z:}`, `%MDL.Quat{frame:, w:, x:, y:, z:}` |
| A quality, a frame, a code, a time unit | atoms: `:good`, `:eci_j2000` (the IR's `ECI_J2000` in lower case), `:past_end`, `:ms` |
| Names, paths, units, states, metadata keys, a record's keys | strings |
| An event time | `{:ground, ns}`, `{:at, ns}`, or `{:since_boot, count, unit}` |
| An engineering value | `{path, value, quality}`, in the record's order |
| An integer | an integer, of any size |
| NaN, infinity, negative infinity | `:nan`, `:infinity`, `:neg_infinity` |
| Text, bytes | binaries; a param's or field's type says which. A bytes argument is `{:bytes, binary}` |
| An instant | integer nanoseconds since 1970-01-01T00:00:00Z (`DateTime` holds microseconds) |
| Absent | `nil` |

Nothing that comes from definitions or messages becomes an atom, so a node
that keeps reloading stores never grows its atom table. A value whose
quality isn't `:good` is the raw value: check the quality before storing
or plotting it. `describe/1` gives the IR's own JSON as maps with string
keys.

## Handles and scheduling

A handle is immutable, and any number of processes may use one at once:
keep it where they can all reach it, such as `:persistent_term`. Opening
the store again gives a new handle; an older one keeps working, on the
build it opened, until it's garbage collected.

Every function that does MDL's work runs on a dirty CPU scheduler. Its
cost depends on the definitions and the payload (a dense JSON document
decodes at about 180 µs a KiB on the machine it was measured on, and a
packet's every element can become a sample), so no size limit could keep
it within the millisecond a normal scheduler allows. The switch costs a few
microseconds a call (about 6 µs there, beside 15 µs for a small message);
`decode_many/3` pays it once for a batch.

## Versions

A binding's version is the runtime release it bundles (a binding 0.3.1
calls runtime 0.3.1), so it reads and writes what that release does
([compatibility](../../spec/compatibility.md) section 6).

## Developing

A checkout builds the native library from source, as above, unless a
checksum file (`checksum-Elixir.MDL.Native.exs`, made only for publishing)
is left in it; `just elixir` builds from source regardless. The release
workflow builds each precompiled library, checks what it needs of the
system, and runs these tests on it as a Hex install would load it, with
`scripts/nif.sh` (RELEASING.md).

`mix test` builds `morphc` with the repository's workspace, builds its
test stores from the repository's definitions, and checks the binding's
results against `morphc`'s for the same messages and commands
([test/fixtures](test/fixtures/README.md)), and against the decoding,
encoding, command decoding and instant cases in `conformance/`, whose
results are worked out from the spec
([spec/conformance.md](../../spec/conformance.md)). `mix format`
formats the Elixir; the native library is in `native/mdl_nif`, its own
Cargo workspace (`cargo clippy` and `cargo fmt` there).
