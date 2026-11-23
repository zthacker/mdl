defmodule MDLTest do
  use ExUnit.Case, async: true

  alias MDL.Test.Fixtures

  setup_all do
    {:ok, kitchen} = MDL.open(Fixtures.store(:kitchen), bindings: Fixtures.clean_bindings())
    %{kitchen: kitchen, messages: Map.new(Fixtures.messages(), &{&1["name"], &1})}
  end

  defp decode(mdl, messages, name) do
    fixture = Map.fetch!(messages, name)
    MDL.decode(mdl, fixture["family"], Fixtures.message(fixture))
  end

  # 2026-06-01T12:00:00Z
  @noon 1_780_315_200_000_000_000

  describe "values" do
    test "the README's worked example decodes as the README shows" do
      {:ok, beacon} =
        MDL.open(Fixtures.store(:beacon), bindings: Fixtures.beacon_bindings())

      message = %{payload: <<0x07, 0x0B, 0x19, 0x01>>, ground_time: @noon}

      assert MDL.decode(beacon, "beacon", message) ===
               {:ok,
                %MDL.Decoded{
                  family: "beacon",
                  native_id: 7,
                  asset: "NODE-7",
                  stand_in: false,
                  set: "beacon_v1",
                  def_version: "6ac262f19dba7ca92aed618221d5e88a8d0796e9e5b26ce7415bb4f8e3cc6a66",
                  packet: "BEACON",
                  event_time: {:ground, @noon},
                  samples: [
                    %MDL.Sample{
                      param: "eps.bus_voltage",
                      value: 28.41,
                      unit: "V",
                      path: "bus",
                      index: nil,
                      time_offset_ns: nil,
                      quality: :good
                    },
                    %MDL.Sample{
                      param: "eps.mode",
                      value: "NOMINAL",
                      unit: nil,
                      path: "mode",
                      index: nil,
                      time_offset_ns: nil,
                      quality: :good
                    }
                  ],
                  record: %{"bus" => 2841, "mode" => 1},
                  engineering: [{"bus", 28410.0, :good}, {"mode", "NOMINAL", :good}]
                }}
    end

    test "NaN and the infinities are atoms, with their quality", context do
      {:ok, decoded} = decode(context.kitchen, context.messages, "sink-non-finite")
      attitude = Enum.find(decoded.samples, &(&1.param == "sink.attitude"))

      assert attitude.value === %MDL.Quat{
               frame: :body,
               w: :nan,
               x: :infinity,
               y: :neg_infinity,
               z: 0.5
             }

      assert attitude.quality == :non_finite
      assert {"q[1]", :infinity, :non_finite} in decoded.engineering
      # A temperature of -128 means no reading: the raw integer, invalid.
      temps = Enum.find(decoded.samples, &(&1.param == "sink.temps" and &1.index == 0))
      assert {temps.value, temps.quality} === {-128, :invalid}
    end

    test "instants before 1970 are negative, and past 2262 exceed 64 bits", context do
      {:ok, old} = decode(context.kitchen, context.messages, "sink-non-finite")
      assert old.event_time === {:at, Fixtures.nanos("1957-12-31T23:59:50Z")}
      assert {:at, before} = old.event_time
      assert before < 0

      {:ok, future} = decode(context.kitchen, context.messages, "vendor_j-far-future")
      assert future.event_time === {:at, 32_503_680_000 * 1_000_000_000}
      assert elem(future.event_time, 1) > 0x7FFFFFFFFFFFFFFF
    end

    test "integers past 2^63 keep every digit", context do
      {:ok, decoded} = decode(context.kitchen, context.messages, "legacy-time-invalid")
      assert decoded.record["unix_us"] === 18_446_744_073_709_551_615
      assert {"unix_us", 18_446_744_073_709_551_615, :invalid} in decoded.engineering
    end

    test "a count since boot keeps its unit", context do
      {:ok, decoded} = decode(context.kitchen, context.messages, "legacy-time-v1")
      assert {:since_boot, count, unit} = decoded.event_time
      assert is_integer(count) and unit in [:s, :ms, :us, :ns]
    end

    # legacy_v1's TIME is its correlation packet: 1700000000000000 us since
    # 1970 is 2023-11-14T22:13:20Z, read 5000 ms after boot.
    test "a correlation packet gives when its asset booted", context do
      {:ok, decoded} = decode(context.kitchen, context.messages, "legacy-time-v1")
      assert decoded.event_time == {:since_boot, 5000, :ms}
      assert decoded.booted == 1_699_999_995_000_000_000

      {:ok, other} = decode(context.kitchen, context.messages, "sink-mixed")
      assert other.booted == nil
    end

    # A correlation packet's own boot instant gives its own instant: its
    # clock's, 2023-11-14T22:13:20Z.
    test "an event time and a boot instant give an instant", context do
      {:ok, decoded} = decode(context.kitchen, context.messages, "legacy-time-v1")
      assert MDL.instant(decoded.event_time, decoded.booted) == 1_700_000_000_000_000_000
      assert MDL.instant(decoded.event_time, nil) == nil

      assert MDL.instant({:at, 5}, nil) == 5
      assert MDL.instant({:ground, 5}, 7) == nil

      # Past 2262, nanoseconds beyond 64 bits, both in and out.
      year_2600 = 19_880_899_200_000_000_000
      assert MDL.instant({:at, year_2600}, nil) == year_2600
      assert MDL.instant({:since_boot, 1, :ns}, year_2600) == year_2600 + 1
    end

    test "frames are atoms, and vectors structs", context do
      {:ok, decoded} = decode(context.kitchen, context.messages, "sink-mixed")
      frames = for %{value: %MDL.Vec3{frame: frame}} <- decoded.samples, do: frame
      assert Enum.sort(frames) == [:enu, :geodetic]
    end
  end

  describe "errors" do
    test "an unknown family", context do
      message = %{payload: <<>>, ground_time: @noon}

      assert {:error, %MDL.Error{code: :unknown_family, problems: []}} =
               MDL.decode(context.kitchen, "nope", message)

      assert [{:error, %MDL.Error{code: :unknown_family}}, {:error, _}] =
               MDL.decode_many(context.kitchen, "nope", [message, message])
    end

    test "a store that isn't there" do
      assert {:error, %MDL.Error{code: :store_unreadable}} = MDL.open("/nonexistent/store")
    end

    # A state the catalog has that the set's packet doesn't accept, and
    # one the catalog doesn't have.
    test "a state a packet doesn't accept", context do
      request = fn mode ->
        %{
          set: "vendor_x_v2",
          command: "eps.set_mode",
          arguments: %{"mode" => mode, "delay_s" => 0}
        }
      end

      assert {:error, %MDL.Error{code: :unsupported_state, message: message}} =
               MDL.encode(context.kitchen, "vendor_x", request.("PAYLOAD"))

      assert message =~ ~s(it accepts "SAFE" and "NOMINAL")

      assert {:error, %MDL.Error{code: :unknown_state}} =
               MDL.encode(context.kitchen, "vendor_x", request.("SCIENCE"))
    end

    # Vendor X's packet requires delay_s, so the catalog's default isn't
    # sent with it.
    test "an argument a packet requires", context do
      request = %{set: "vendor_x_v2", command: "eps.set_mode", arguments: %{"mode" => "SAFE"}}

      assert {:error, %MDL.Error{code: :missing_argument, message: message}} =
               MDL.encode(context.kitchen, "vendor_x", request)

      assert message =~ "the set requires it, so the catalog's default, 0, isn't sent"

      given = put_in(request, [:arguments, "delay_s"], 0)
      assert {:ok, %MDL.Encoded{defaulted: []}} = MDL.encode(context.kitchen, "vendor_x", given)
    end

    # The payload is the packet by its opcode and the link's metadata
    # together, so without the apid it's no packet of the set.
    test "a command payload without its metadata", context do
      request = %{
        set: "vendor_x_v2",
        command: "eps.set_mode",
        arguments: %{"mode" => "SAFE", "delay_s" => 0}
      }

      {:ok, encoded} = MDL.encode(context.kitchen, "vendor_x", request)
      message = %{set: "vendor_x_v2", payload: encoded.payload}

      assert {:error, %MDL.Error{code: :no_packet}} =
               MDL.decode_command(context.kitchen, "vendor_x", message)
    end

    # A native id is never empty (spec/api.md section 1), so empty text
    # names no target, in a string target field or an integer one.
    test "an empty target", context do
      legacy = %{set: "legacy_v1", command: "eps.set_mode", arguments: %{"mode" => "SAFE"}}

      sink = %{
        set: "sink_v1",
        command: "sink.configure",
        arguments: %{"enable" => false, "gain" => 2.0, "label" => "OK", "taps" => [1.0, 2.0]}
      }

      for {family, request, target} <- [{"legacy", legacy, "box-1"}, {"sink", sink, 7}] do
        assert {:ok, %MDL.Encoded{}} =
                 MDL.encode(context.kitchen, family, Map.put(request, :native_id, target))

        assert {:error, %MDL.Error{code: :missing_target}} =
                 MDL.encode(context.kitchen, family, Map.put(request, :native_id, ""))
      end
    end

    # An artifact's identity is its canonical JSON's digest, so changing a
    # value (not its spacing) changes it.
    test "a damaged artifact" do
      store = copy_store("damaged")
      manifest = store |> Path.join("manifest.json") |> File.read!() |> JSON.decode!()
      artifact = Path.join(store, manifest["sets"]["sink_v1"] <> ".json")
      damaged = String.replace(File.read!(artifact), ~s("name":"sink_v1"), ~s("name":"sink_v9"))
      assert damaged != File.read!(artifact)
      File.write!(artifact, damaged)
      assert {:error, %MDL.Error{code: :wrong_digest}} = MDL.open(store)
    end

    test "bindings that aren't TOML, and rows invalid in themselves" do
      store = Fixtures.store(:kitchen)
      assert {:error, %MDL.Error{code: :invalid_toml}} = MDL.open(store, bindings: "not = [toml")

      bindings = """
      mdl_version = "2.0"

      [[binding]]
      family = "sink"
      native_id = 7
      effective_from = "2026-01-01T00:00:00Z"
      """

      assert {:error, %MDL.Error{code: :invalid_bindings, problems: [problem | _]}} =
               MDL.open(store, bindings: bindings)

      assert %MDL.Problem{code: code, path: "/binding/0" <> _} = problem
      assert is_atom(code)
    end

    test "an error is an exception a caller may raise", context do
      {:error, error} = MDL.decode(context.kitchen, "nope", %{payload: <<>>, ground_time: 0})
      assert Exception.message(error) == error.message
      assert_raise MDL.Error, error.message, fn -> raise error end
    end
  end

  describe "bindings" do
    test "rows a build doesn't fit are warnings, and their messages are refused", context do
      {:ok, mdl} = MDL.open(Fixtures.store(:kitchen), bindings: Fixtures.kitchen_bindings())

      assert [
               %MDL.Problem{code: :binding_set, path: "/binding/1/set"},
               %MDL.Problem{code: :binding_set, path: "/binding/5"},
               %MDL.Problem{code: :unknown_set, path: "/binding/6/set"}
             ] = MDL.warnings(mdl)

      assert {:error, %MDL.Error{code: :unexpected_set}} =
               decode(mdl, context.messages, "vendor_x-set-and-version")

      assert {:error, %MDL.Error{code: :missing_set}} =
               decode(mdl, context.messages, "vendor_j-no-set")

      unknown_set = %{
        payload: ~s({"sat":"PX-9","t":"2026-03-01T00:00:00Z","ch":"THM","heater":1,"flow":0.5}),
        ground_time: @noon
      }

      assert {:error, %MDL.Error{code: :unknown_set}} = MDL.decode(mdl, "vendor_j", unknown_set)
      assert MDL.warnings(context.kitchen) == []
    end

    test "without bindings each native id stands for an asset of its own", context do
      {:ok, mdl} = MDL.open(Fixtures.store(:kitchen))
      {:ok, decoded} = decode(mdl, context.messages, "sink-mixed")
      assert {decoded.asset, decoded.stand_in} == {"7", true}

      # A family of several sets and no version locator needs bindings.
      assert {:error, %MDL.Error{code: :missing_set}} =
               decode(mdl, context.messages, "legacy-nav-v2")
    end

    test "with_bindings gives the build other bindings, or none", context do
      {:ok, bare} = MDL.with_bindings(context.kitchen, nil)
      {:ok, decoded} = decode(bare, context.messages, "sink-mixed")
      assert decoded.stand_in

      {:ok, bound} = MDL.with_bindings(bare, Fixtures.clean_bindings())
      {:ok, decoded} = decode(bound, context.messages, "sink-mixed")
      assert {decoded.asset, decoded.stand_in} == {"NODE-7", false}

      assert {:error, %MDL.Error{code: :invalid_toml}} = MDL.with_bindings(bare, "[")
    end
  end

  test "a store from its files' bytes is the store from its directory", context do
    store = Fixtures.store(:kitchen)
    manifest = File.read!(Path.join(store, "manifest.json"))
    artifacts = Enum.map(artifact_files(store), &File.read!/1)
    {:ok, from_bytes} = MDL.open_bytes(manifest, artifacts, bindings: Fixtures.clean_bindings())

    for name <- Map.keys(context.messages) do
      assert decode(from_bytes, context.messages, name) ===
               decode(context.kitchen, context.messages, name)
    end

    assert MDL.describe(from_bytes) === MDL.describe(context.kitchen)
  end

  describe "routes" do
    test "a native id's binding over time, and a retired one", context do
      at = &Fixtures.nanos/1

      sets =
        for time <- ["2025-06-01T00:00:00Z", "2026-03-01T00:00:00Z", "2026-07-01T00:00:00Z"] do
          {:ok, %MDL.Route{asset: "BOX-1", set: set}} =
            MDL.binding(context.kitchen, "legacy", "box-1", at.(time))

          set
        end

      assert sets == ["legacy_v1", "legacy_v2", "legacy_v3"]

      assert MDL.binding(context.kitchen, "legacy", "box-1", at.("2027-03-01T00:00:00Z")) ==
               {:ok, nil}
    end

    test "an asset's native id, where a command to it goes", context do
      assert MDL.asset(context.kitchen, "sink", "NODE-7", @noon) ===
               {:ok,
                %MDL.Route{
                  family: "sink",
                  native_id: 7,
                  asset: "NODE-7",
                  set: "sink_v1",
                  stand_in: false
                }}

      assert MDL.asset(context.kitchen, "sink", "NODE-99", @noon) == {:ok, nil}

      assert {:error, %MDL.Error{code: :unknown_family}} =
               MDL.asset(context.kitchen, "nope", "NODE-7", @noon)
    end

    test "a family whose version locator chooses names no set", context do
      assert {:ok, %MDL.Route{native_id: 101, asset: "SAT-X", set: nil}} =
               MDL.binding(context.kitchen, "vendor_x", 101, @noon)
    end

    test "without bindings, the stand-in" do
      {:ok, mdl} = MDL.open(Fixtures.store(:kitchen))

      assert MDL.binding(mdl, "sink_json", "node-3", @noon) ===
               {:ok,
                %MDL.Route{
                  family: "sink_json",
                  native_id: "node-3",
                  asset: "node-3",
                  set: "sink_json_v1",
                  stand_in: true
                }}
    end

    # A native id is never empty (spec/api.md section 1), so neither
    # lookup finds an asset for empty text, with bindings or without.
    test "an empty native id is no asset", context do
      {:ok, bare} = MDL.open(Fixtures.store(:kitchen))

      for mdl <- [context.kitchen, bare] do
        assert MDL.binding(mdl, "sink_json", "", @noon) == {:ok, nil}
        assert MDL.asset(mdl, "sink_json", "", @noon) == {:ok, nil}
      end
    end
  end

  describe "commands" do
    test "a payload decodes back to the command and arguments that encoded it", context do
      request = %{
        set: "vendor_x_v2",
        command: "eps.set_mode",
        arguments: %{"mode" => "NOMINAL", "delay_s" => 2.5}
      }

      {:ok, encoded} = MDL.encode(context.kitchen, "vendor_x", request)

      message = %{set: "vendor_x_v2", payload: encoded.payload, metadata: encoded.metadata}
      {:ok, decoded} = MDL.decode_command(context.kitchen, "vendor_x", message)

      assert decoded == %MDL.DecodedCommand{
               family: "vendor_x",
               set: "vendor_x_v2",
               def_version: encoded.def_version,
               packet: encoded.packet,
               command: "eps.set_mode",
               arguments: %{"mode" => "NOMINAL", "delay_s" => 2.5},
               defaulted: [],
               target: nil,
               record: encoded.record
             }
    end
  end

  describe "describe" do
    test "the build, as maps with string keys", context do
      {:ok, decoded} = decode(context.kitchen, context.messages, "sink-mixed")
      {:ok, description} = MDL.describe(context.kitchen)

      assert %{"format" => 1, "catalog" => %{"digest" => digest, "catalog" => catalog}} =
               description

      assert is_binary(digest)

      assert Enum.map(description["families"], & &1["name"]) ==
               ["legacy", "sink", "sink_json", "vendor_j", "vendor_x"]

      sets = Map.new(description["sets"], &{&1["name"], &1})
      assert sets["sink_v1"]["def_version"] == decoded.def_version
      assert %{"packets" => [_ | _]} = sets["sink_v1"]["layout"]
      assert sets["sink_json_v1"]["layout"] == nil

      # The IR's own values: a range end past 2^53 stays the IR's decimal string.
      [stamp] = catalog["commands"]["sink.mark"]["arguments"]
      assert stamp["range"]["max"] == "1152921504606846975"
    end
  end

  describe "arguments of the wrong shape raise" do
    test "a message", context do
      kitchen = context.kitchen

      for {message, expected} <- [
            {%{payload: <<1>>}, ~r/:ground_time/},
            {%{payload: [1], ground_time: 0}, ~r/:payload is a binary/},
            {%{payload: <<1>>, ground_time: 1.5}, ~r/integer/},
            {%{payload: <<1>>, ground_time: 0x8000000000000000}, ~r/64 bits/},
            {%{payload: <<1>>, ground_time: 0, metadata: %{apid: 1}}, ~r/names \(strings/},
            {%{payload: <<1>>, ground_time: 0, metadata: %{"apid" => 1.5}},
             ~r/integers or strings/},
            {%{payload: <<1>>, ground_time: 0, metadata: %{"apid" => 0x10000000000000000}},
             ~r/2\^64/},
            {%{payload: <<1>>, ground_time: 0, ground: 1}, ~r/a message is a map/}
          ] do
        assert_raise ArgumentError, expected, fn -> MDL.decode(kitchen, "sink", message) end
      end

      assert_raise ArgumentError, fn -> MDL.decode_many(kitchen, "sink", [%{payload: 1}]) end
    end

    # A command's decoding takes no ground time, so a message with one is
    # of the wrong shape, as with any other key a command message hasn't got.
    test "a command message", context do
      kitchen = context.kitchen

      for {message, expected} <- [
            {%{payload: <<1>>}, ~r/a command message is a map/},
            {%{set: :sink_v1, payload: <<1>>}, ~r/:set is a string/},
            {%{set: "sink_v1", payload: [1]}, ~r/:payload is a binary/},
            {%{set: "sink_v1", payload: <<1>>, ground_time: 0}, ~r/a command message is a map/},
            {%{set: "sink_v1", payload: <<1>>, metadata: %{apid: 1}}, ~r/names \(strings/}
          ] do
        assert_raise ArgumentError, expected, fn ->
          MDL.decode_command(kitchen, "sink", message)
        end
      end
    end

    # Instants span years 0001 to 9999: -62135596800 s to just before
    # 253402300800 s since 1970.
    test "an event time and a boot instant" do
      for {event_time, booted, expected} <- [
            {:now, nil, ~r/an event time is/},
            {{:later, 1}, nil, ~r/an event time is/},
            {{:at, 1, 2}, nil, ~r/an event time is/},
            {{:at, 1.5}, nil, ~r/integer nanoseconds/},
            {{:at, 253_402_300_800_000_000_000}, nil, ~r/within years 0001 to 9999/},
            {{:ground, "noon"}, nil, ~r/integer nanoseconds/},
            {{:since_boot, -1, :s}, nil, ~r/from 0 to 2\^64 - 1/},
            {{:since_boot, 0x10000000000000000, :s}, nil, ~r/from 0 to 2\^64 - 1/},
            {{:since_boot, 1, :min}, nil, ~r/an event time is/},
            {{:at, 1}, "x", ~r/a boot instant is/},
            {{:since_boot, 1, :s}, -62_135_596_800_000_000_001, ~r/within years 0001/}
          ] do
        assert_raise ArgumentError, expected, fn -> MDL.instant(event_time, booted) end
      end
    end

    test "a command request", context do
      kitchen = context.kitchen
      base = %{set: "sink_v1", command: "sink.mark"}

      for {request, expected} <- [
            {%{command: "sink.mark"}, ~r/:set and :command/},
            {Map.put(base, :target, 7), ~r/a command request is a map/},
            {Map.put(base, :arguments, %{stamp: 1}), ~r/argument names \(strings\)/},
            {Map.put(base, :arguments, %{"stamp" => :one}), ~r/the only atoms/},
            {Map.put(base, :arguments, %{"stamp" => <<0xFF>>}), ~r/\{:bytes, binary\}/},
            {Map.put(base, :arguments, %{"stamp" => {:raw, <<1>>}}), ~r/the only tuple/},
            {Map.put(base, :native_id, 1.5), ~r/a native id is/}
          ] do
        assert_raise ArgumentError, expected, fn -> MDL.encode(kitchen, "sink", request) end
      end
    end

    test "a list argument's values are scalars, however deep a list is nested", context do
      deep = Enum.reduce(1..100_000, 1, fn _, inner -> [inner] end)

      for nested <- [[[1]], deep] do
        request = %{set: "sink_v1", command: "sink.mark", arguments: %{"stamp" => nested}}

        assert_raise ArgumentError, ~r/values are scalars, not lists/, fn ->
          MDL.encode(context.kitchen, "sink", request)
        end
      end
    end

    test "options and lookups", context do
      assert_raise ArgumentError, fn -> MDL.open(Fixtures.store(:kitchen), binding: "") end

      assert_raise ArgumentError, ~r/:bindings/, fn ->
        MDL.open(Fixtures.store(:kitchen), bindings: 1)
      end

      assert_raise ArgumentError, ~r/a native id is/, fn ->
        MDL.binding(context.kitchen, "sink", 1.5, @noon)
      end
    end
  end

  test "one handle serves many processes at once", context do
    fixture = Map.fetch!(context.messages, "sink-mixed")
    message = Fixtures.message(fixture)
    expected = MDL.decode(context.kitchen, "sink", message)

    results =
      1..64
      |> Task.async_stream(fn _ -> MDL.decode(context.kitchen, "sink", message) end,
        max_concurrency: 16
      )
      |> Enum.map(fn {:ok, result} -> result end)

    assert Enum.all?(results, &(&1 === expected))
  end

  # A store's artifacts: each a file named by its digest, beside the
  # manifest (spec/store.md section 1).
  defp artifact_files(store) do
    store
    |> Path.join("*.json")
    |> Path.wildcard()
    |> Enum.reject(&(Path.basename(&1) == "manifest.json"))
  end

  defp copy_store(name) do
    target =
      Path.join(System.tmp_dir!(), "mdl-test-#{name}-#{System.unique_integer([:positive])}")

    File.cp_r!(Fixtures.store(:kitchen), target)
    on_exit(fn -> File.rm_rf!(target) end)
    target
  end
end
