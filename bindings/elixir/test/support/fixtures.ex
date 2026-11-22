defmodule MDL.Test.Fixtures do
  @moduledoc false
  # Stores morphc builds from the repository's own definitions, the
  # messages and bindings the tests use, and morphc's results for the same
  # inputs, which the binding's must match (spec/api.md: `morphc decode`
  # and `morphc encode` write the runtime's results as JSON).

  @root Path.expand("../../../..", __DIR__)
  @kitchen_bundle Path.join(@root, "crates/mdl/tests/fixtures/full_bundle.json")
  @kitchen_bindings Path.join(@root, "crates/mdl/tests/fixtures/full_bindings.toml")
  @beacon Path.join(@root, "examples/beacon")
  @messages Path.expand("../fixtures/kitchen/messages.json", __DIR__)

  # The kitchen sink's binding rows that morphc refuses and the runtime
  # warns about (bindings.md section 6, spec/api.md section 2.1): a set for
  # a family whose version locator chooses (vendor_x 102), none for one
  # without (PX-8), and a set the family hasn't got (PX-9).
  @refused_rows ["native_id = 102\n", ~s("PX-8"), ~s("PX-9")]

  @doc "Builds morphc and the stores once, before the tests run."
  def setup! do
    work = Path.join(Mix.Project.build_path(), "mdl-fixtures")
    File.rm_rf!(work)
    File.mkdir_p!(work)
    {_, 0} = System.cmd("cargo", ["build", "--quiet", "-p", "morpheus-morphc"], cd: @root)
    target = System.get_env("CARGO_TARGET_DIR", Path.join(@root, "target"))
    morphc = Path.join([target, "debug", "morphc"])

    clean = Path.join(work, "clean_bindings.toml")
    File.write!(clean, clean_bindings())

    paths = %{
      work: work,
      morphc: morphc,
      clean_bindings: clean,
      kitchen: build!(morphc, [@kitchen_bundle], Path.join(work, "kitchen")),
      beacon: build!(morphc, [Path.join(@beacon, "beacon.toml")], Path.join(work, "beacon"))
    }

    :persistent_term.put(__MODULE__, paths)
  end

  defp build!(morphc, inputs, out) do
    {output, status} = System.cmd(morphc, ["build" | inputs] ++ ["--out", out])
    if status != 0, do: raise("morphc build #{inspect(inputs)} failed:\n#{output}")
    out
  end

  def store(name), do: Map.fetch!(:persistent_term.get(__MODULE__), name)

  @doc """
  A store morphc builds from a conformance file's definitions, an IR
  bundle (spec/conformance.md section 2), written to a file named `name`
  for morphc to read.
  """
  def build_definitions!(definitions, name) do
    dir = Path.join(work(), "conformance")
    File.mkdir_p!(dir)
    input = Path.join(dir, name <> ".json")
    File.write!(input, JSON.encode!(definitions))
    build!(Map.fetch!(:persistent_term.get(__MODULE__), :morphc), [input], Path.join(dir, name))
  end

  def root, do: @root

  def kitchen_bundle, do: @kitchen_bundle |> File.read!() |> JSON.decode!()

  @doc "The kitchen sink's bindings, the rows routing refuses included."
  def kitchen_bindings, do: File.read!(@kitchen_bindings)

  @doc "The kitchen sink's bindings without the rows morphc refuses."
  def clean_bindings do
    [head | rows] = String.split(kitchen_bindings(), "[[binding]]")
    kept = Enum.reject(rows, fn row -> Enum.any?(@refused_rows, &String.contains?(row, &1)) end)
    head <> Enum.map_join(kept, &("[[binding]]" <> &1))
  end

  def beacon_bindings, do: File.read!(Path.join(@beacon, "bindings.toml"))

  @doc "The kitchen sink's messages (test/fixtures/README.md)."
  def messages, do: @messages |> File.read!() |> JSON.decode!()

  @doc "A fixture message as `MDL.decode/3` takes it."
  def message(fixture) do
    payload =
      case fixture do
        %{"json" => text} -> text
        %{"hex" => hex} -> Base.decode16!(hex, case: :lower)
      end

    %{payload: payload, metadata: fixture["metadata"], ground_time: nanos(fixture["ground_time"])}
  end

  @doc """
  Nanoseconds since the Unix epoch of an RFC 3339 instant in UTC, to the
  nanosecond (`DateTime` keeps microseconds).
  """
  def nanos(text) do
    [whole, fraction] =
      case Regex.run(~r/^([^.Z]+)(?:\.(\d{1,9}))?Z$/, text) do
        [_, whole] -> [whole, ""]
        [_, whole, fraction] -> [whole, fraction]
      end

    {:ok, at, 0} = DateTime.from_iso8601(whole <> "Z")
    DateTime.to_unix(at, :nanosecond) + String.to_integer(String.pad_trailing(fraction, 9, "0"))
  end

  @doc """
  morphc's result for each fixture message of `family`, in order: a
  decoded message's JSON as `{:ok, json}`, or an error's as
  `{:error, json}`.
  """
  def morphc_decode(store, family, fixtures, bindings) do
    records =
      Enum.map_join(fixtures, "\n", fn fixture ->
        payload =
          case fixture do
            %{"json" => text} -> %{"text" => text}
            %{"hex" => hex} -> %{"hex" => hex}
          end

        JSON.encode!(
          Map.merge(payload, %{"meta" => fixture["metadata"], "time" => fixture["ground_time"]})
        )
      end)

    file = Path.join(work(), "records-#{System.unique_integer([:positive])}.jsonl")
    File.write!(file, records <> "\n")
    with_bindings = if bindings, do: ["--bindings", bindings], else: []

    report =
      morphc!(
        ["decode", store, "--family", family, "--records", file, "--format", "json"] ++
          with_bindings
      )

    for message <- report["decode"]["messages"] do
      case message do
        %{"ok" => true, "telemetry" => telemetry} -> {:ok, telemetry}
        %{"ok" => false, "error" => error} -> {:error, error}
      end
    end
  end

  @doc """
  morphc's result for a command request: `{:ok, json}` or
  `{:error, json}`. `args` are morphc's `--arg NAME=VALUE` texts.
  """
  def morphc_encode(store, set, command, args, target \\ nil) do
    with_target = if target, do: ["--target", to_string(target)], else: []
    with_args = Enum.flat_map(args, fn arg -> ["--arg", arg] end)

    report =
      morphc!(
        ["encode", store, "--set", set, "--command", command, "--format", "json"] ++
          with_args ++ with_target
      )

    case report["encode"] do
      %{"ok" => true, "packet" => packet} -> {:ok, packet}
      %{"ok" => false, "error" => error} -> {:error, error}
    end
  end

  # morphc's JSON report. It exits 1 when a message doesn't decode or a
  # command doesn't encode, which the report says too.
  defp morphc!(args) do
    {output, status} = System.cmd(Map.fetch!(:persistent_term.get(__MODULE__), :morphc), args)

    if status not in [0, 1] do
      raise "morphc #{Enum.join(args, " ")} exited #{status}:\n#{output}"
    end

    JSON.decode!(output)
  end

  defp work, do: Map.fetch!(:persistent_term.get(__MODULE__), :work)
end
