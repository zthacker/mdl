defmodule MDL do
  @moduledoc """
  MDL's runtime for Elixir: open an artifact store `morphc` built, decode
  telemetry, encode commands and decode them back, describe the build,
  look up which asset a native id is, and turn an event time into an
  instant. It calls the runtime every binding calls (spec/api.md),
  through a native library, and holds no MDL semantics of its own.

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

  ## Values

  Results use Elixir's own values:

  * The results' fixed shapes are structs (`MDL.Decoded`, `MDL.Sample`,
    `MDL.Encoded`, `MDL.Route`, `MDL.Error`, `MDL.Problem`, `MDL.Vec3`,
    `MDL.Quat`), and MDL's own names in them are atoms: a quality, a frame,
    an error's code, an event time's kind, a time unit.
  * Everything that comes from definitions or messages is a string: param
    and entry names, paths, units, state names, metadata keys, the
    record's keys. A node that keeps reloading stores never grows its atom
    table.
  * Integers are of any size. NaN and the infinities, which the BEAM's
    floats can't hold, are `:nan`, `:infinity` and `:neg_infinity`.
  * Text and bytes are both binaries; a param's or field's type, which
    `describe/1` gives, says which. A bytes argument to `encode/3` is
    `{:bytes, binary}`.
  * An instant is an integer: nanoseconds since 1970-01-01T00:00:00Z,
    every day 86,400 seconds. `DateTime` holds microseconds.

  ## Errors

  Every operation returns `{:ok, result}` or `{:error, %MDL.Error{}}`,
  whose `:code` names the problem and is stable across releases. An
  argument of the wrong shape (a payload that isn't a binary, a metadata
  key that isn't a string) raises `ArgumentError`.

  ## Handles and scheduling

  A handle is immutable and may be used by any number of processes at
  once: store it where every process can reach it (`:persistent_term`, an
  ETS table, a process's state). Opening the store again gives a new
  handle; an older one keeps working, on the build it opened, until it's
  garbage collected.

  Every function that does MDL's work runs on a dirty CPU scheduler, so no
  definition or payload, however large, holds up the BEAM's normal
  schedulers. The switch costs a few microseconds a call; `decode_many/3`
  pays it once for a batch.
  """

  @typedoc "An opened store."
  @opaque handle :: reference()

  @typedoc "A family's name."
  @type family :: String.t()

  @typedoc "Nanoseconds since 1970-01-01T00:00:00Z, every day 86,400 seconds."
  @type instant :: integer()

  @typedoc "What a family's alias locator reads: an integer or a string."
  @type native_id :: integer() | String.t()

  @typedoc "Metadata a link delivers with a payload, by name."
  @type metadata :: %{optional(String.t()) => integer() | String.t()}

  @typedoc """
  A message: its payload (a JSON family's is the document's UTF-8 text),
  the metadata the link delivers with it, and when the ground received it.
  """
  @type message :: %{
          required(:payload) => binary(),
          required(:ground_time) => instant(),
          optional(:metadata) => metadata()
        }

  @typedoc "An argument's value: bytes are `{:bytes, binary}`, text a string."
  @type argument ::
          integer()
          | float()
          | :nan
          | :infinity
          | :neg_infinity
          | boolean()
          | String.t()
          | {:bytes, binary()}
          | [argument()]

  @typedoc """
  A command to encode: the set that encodes it, the catalog command, its
  arguments by name, and the native id of the asset it's sent to, for a
  packet that writes it.
  """
  @type request :: %{
          required(:set) => String.t(),
          required(:command) => String.t(),
          optional(:arguments) => %{optional(String.t()) => argument()},
          optional(:native_id) => native_id() | nil
        }

  @typedoc """
  A command payload to decode: the set that decodes it, the payload, a
  binary, and the metadata the link framed it with, which a command
  packet's identity includes. No ground time: decoding a command takes
  none.
  """
  @type command_message :: %{
          required(:set) => String.t(),
          required(:payload) => binary(),
          optional(:metadata) => %{optional(String.t()) => integer() | String.t()}
        }

  @typedoc "A float, or NaN or an infinity."
  @type float_value :: float() | :nan | :infinity | :neg_infinity

  @typedoc "A decoded value: a record's, a sample's or an engineering value's."
  @type value ::
          integer()
          | float_value()
          | boolean()
          | binary()
          | MDL.Vec3.t()
          | MDL.Quat.t()
          | nil
          | [value()]
          | %{optional(String.t()) => value()}

  @typedoc "Whether a value can be trusted; any but `:good` is the raw value."
  @type quality ::
          :good | :invalid | :unknown_state | :out_of_calibration | :out_of_range | :non_finite

  @typedoc "A composite's frame: its IR name, `ECI_J2000`, in lower case."
  @type frame :: :ecef | :eci_j2000 | :body | :ned | :enu | :geodetic

  @typedoc """
  When a message's values were taken: its ground time, when the packet
  gives none; an instant the packet gives; or a count since the asset
  booted, which isn't an instant until correlated.
  """
  @type event_time ::
          {:ground, instant()}
          | {:at, instant()}
          | {:since_boot, non_neg_integer(), :s | :ms | :us | :ns}

  @typedoc "An operation's result."
  @type result(ok) :: {:ok, ok} | {:error, MDL.Error.t()}

  @doc """
  Opens the store in `dir` (spec/api.md section 2.1).

  Options:

  * `:bindings`: the text of a bindings file, which names the asset each
    native id is and the set that decodes it. Without it, each native id
    stands for an asset of its own.

  Bindings that can't be read refuse the open. Anything else they say that
  the build doesn't have is a warning (`warnings/1`), and the messages it
  affects are refused when decoded.
  """
  @spec open(Path.t(), keyword()) :: result(handle())
  def open(dir, opts \\ []) do
    MDL.Native.open(IO.chardata_to_string(dir), bindings(opts))
  end

  @doc """
  Opens a store given as its files' bytes: the manifest's, and each
  artifact's, which are found by their own identity and need no names.
  Takes the options `open/2` takes.
  """
  @spec open_bytes(binary(), [binary()], keyword()) :: result(handle())
  def open_bytes(manifest, artifacts, opts \\ [])
      when is_binary(manifest) and is_list(artifacts) do
    MDL.Native.open_bytes(manifest, artifacts, bindings(opts))
  end

  @doc """
  What the handle's bindings say that its build doesn't have: a set it
  lacks, a set named for a family whose version locator chooses. The
  messages these affect are refused when decoded, with their own codes.
  """
  @spec warnings(handle()) :: [MDL.Problem.t()]
  def warnings(handle), do: MDL.Native.warnings(handle)

  @doc """
  The handle's build with other bindings (their text), or none (`nil`),
  without reading or assembling the build again.
  """
  @spec with_bindings(handle(), String.t() | nil) :: result(handle())
  def with_bindings(handle, bindings) when is_binary(bindings) or is_nil(bindings) do
    MDL.Native.with_bindings(handle, bindings)
  end

  @doc """
  Decodes a message `family` sent (spec/api.md section 2.2): routes it to
  an asset and a set, identifies its packet, and gives its samples.
  """
  @spec decode(handle(), family(), message()) :: result(MDL.Decoded.t())
  def decode(handle, family, message) when is_binary(family) and is_map(message) do
    MDL.Native.decode(handle, family, message)
  end

  @doc """
  Decodes each message, in order (spec/api.md section 2.3): one result per
  message, one message's error not stopping the others. One call for the
  batch, so the switch to a dirty scheduler is paid once.
  """
  @spec decode_many(handle(), family(), [message()]) :: [result(MDL.Decoded.t())]
  def decode_many(handle, family, messages) when is_binary(family) and is_list(messages) do
    MDL.Native.decode_many(handle, family, messages)
  end

  @doc """
  Encodes a catalog command as one of `family`'s sets' command packets
  (spec/api.md section 2.4), its arguments checked against their types,
  states, ranges and lists.
  """
  @spec encode(handle(), family(), request()) :: result(MDL.Encoded.t())
  def encode(handle, family, request) when is_binary(family) and is_map(request) do
    MDL.Native.encode(handle, family, request)
  end

  @doc """
  Decodes a command payload with one of `family`'s sets (spec/api.md
  section 2.7): which of its command packets the payload and its metadata
  are, and each argument, as a simulated asset receiving it or command
  history reads it. The set is the caller's to name, as for `encode/3`.
  """
  @spec decode_command(handle(), family(), command_message()) :: result(MDL.DecodedCommand.t())
  def decode_command(handle, family, message) when is_binary(family) and is_map(message) do
    MDL.Native.decode_command(handle, family, message)
  end

  @doc """
  The instant an event time is (spec/api.md section 2.8), or `nil`:
  `{:at, ns}` gives `ns`; `{:since_boot, count, unit}` gives `booted` plus
  `count` in `unit`, when there's a boot instant and the sum is within
  years 0001 to 9999; `{:ground, ns}` gives `nil`, as the message carries
  no time of its own.

  `booted` is the latest boot instant the caller has seen for the
  message's family and asset, its own `:booted` included (`MDL.Decoded`),
  or `nil`. It needs no handle. Instants are integer nanoseconds since
  1970-01-01T00:00:00Z, of any size MDL gives; an argument that isn't an
  event time or an instant raises `ArgumentError`.

      key = {decoded.family, decoded.asset}
      boots = if decoded.booted, do: Map.put(boots, key, decoded.booted), else: boots
      MDL.instant(decoded.event_time, Map.get(boots, key))
  """
  @spec instant(event_time(), instant() | nil) :: instant() | nil
  def instant(event_time, booted), do: MDL.Native.instant(event_time, booted)

  @doc """
  Describes the build (spec/api.md section 3.3): the catalog, each family
  and each set as the IR has them, with their digests, and where each
  entry of a binary set can be. The description is maps with string keys,
  as the IR's JSON is: `"format"`, `"catalog"`, `"families"` and `"sets"`.
  """
  @spec describe(handle()) :: result(map())
  def describe(handle), do: MDL.Native.describe(handle)

  @doc """
  What `native_id` of `family` is at `at` (spec/api.md section 2.6), as a
  message received then would resolve it: its binding, or without bindings
  the asset it stands for; `nil` when it's no asset then.
  """
  @spec binding(handle(), family(), native_id(), instant()) :: result(MDL.Route.t() | nil)
  def binding(handle, family, native_id, at) when is_binary(family) and is_integer(at) do
    MDL.Native.binding(handle, family, native_id, at)
  end

  @doc """
  The native id that is `asset` in `family` at `at`, with its binding
  (spec/api.md section 2.6): where a command to the asset goes. `nil` when
  no native id is the asset then; `:ambiguous_asset` when several are.
  """
  @spec asset(handle(), family(), String.t(), instant()) :: result(MDL.Route.t() | nil)
  def asset(handle, family, asset, at)
      when is_binary(family) and is_binary(asset) and is_integer(at) do
    MDL.Native.asset(handle, family, asset, at)
  end

  defp bindings(opts) do
    case Keyword.validate!(opts, bindings: nil)[:bindings] do
      text when is_binary(text) or is_nil(text) ->
        text

      _ ->
        raise ArgumentError, ":bindings is the text of a bindings file, or nil"
    end
  end
end
