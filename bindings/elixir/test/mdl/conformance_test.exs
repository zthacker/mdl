defmodule MDL.ConformanceTest do
  # The language-neutral decoding and encoding cases in conformance/decoding
  # and conformance/encoding (spec/conformance.md sections 2 and 3),
  # through the binding's own API. morphc builds each file's definitions.
  # Each expected result, never an implementation's output, is read by
  # MDL.Test.Expected as the Elixir values the binding must return, and
  # compared with === with each float as its bits, so an integer and a
  # float of one value differ, and so do 0.0 and -0.0 on every OTP release.
  use ExUnit.Case, async: true

  alias MDL.Test.{Expected, Fixtures}

  @dir Path.expand("../../../../conformance/decoding", __DIR__)
  @files @dir |> Path.join("**/*.json") |> Path.wildcard() |> Enum.sort()
  @encoding_dir Path.expand("../../../../conformance/encoding", __DIR__)
  @encoding_files @encoding_dir |> Path.join("**/*.json") |> Path.wildcard() |> Enum.sort()
  @commands_dir Path.expand("../../../../conformance/decoding_commands", __DIR__)
  @command_files @commands_dir |> Path.join("**/*.json") |> Path.wildcard() |> Enum.sort()

  test "there are decoding, encoding and command decoding cases to run" do
    assert @files != [], "no case files in #{@dir}"
    assert @encoding_files != [], "no case files in #{@encoding_dir}"
    assert @command_files != [], "no case files in #{@commands_dir}"
  end

  for path <- @files do
    @external_resource path

    test "the decoding cases of #{Path.relative_to(path, @dir)}" do
      run(unquote(path))
    end
  end

  for path <- @encoding_files do
    @external_resource path

    test "the encoding cases of #{Path.relative_to(path, @encoding_dir)}" do
      run_encoding(unquote(path))
    end
  end

  for path <- @command_files do
    @external_resource path

    test "the command decoding cases of #{Path.relative_to(path, @commands_dir)}" do
      run_command_decoding(unquote(path))
    end
  end

  defp run(path) do
    file = path |> File.read!() |> JSON.decode!()
    definitions = file["definitions"]
    # A store of the file's own, named by its path under conformance/decoding.
    name = path |> Path.relative_to(@dir) |> Path.rootname() |> String.replace("/", "-")
    store = Fixtures.build_definitions!(definitions, name)

    options =
      case file["bindings"] do
        nil -> []
        lines -> [bindings: Enum.join(lines, "\n") <> "\n"]
      end

    {:ok, mdl} = MDL.open(store, options)

    # The warnings the open gives (spec/api.md section 2.1), by code and
    # path in any order; none when the file names none.
    given = mdl |> MDL.warnings() |> Enum.map(&{Atom.to_string(&1.code), &1.path}) |> Enum.sort()
    expected = (file["warnings"] || []) |> Enum.map(&{&1["code"], &1["path"]}) |> Enum.sort()

    assert given == expected,
           "the open gave the warnings #{inspect(given)}, not #{inspect(expected)}"

    {:ok, description} = MDL.describe(mdl)
    versions = Map.new(description["sets"], &{&1["name"], &1["def_version"]})
    cases = file["cases"]

    singly =
      for kase <- cases do
        actual = MDL.decode(mdl, kase["family"], message(kase["message"]))
        failure(kase, actual, definitions, versions, "")
      end

    # Each family's messages again, in the file's order, in one call.
    together =
      Enum.flat_map(group_in_order(cases), fn {family, of_family} ->
        results = MDL.decode_many(mdl, family, Enum.map(of_family, &message(&1["message"])))

        if length(results) == length(of_family) do
          Enum.zip_with(of_family, results, fn kase, actual ->
            failure(kase, actual, definitions, versions, " (decode_many)")
          end)
        else
          [
            "decode_many gave #{length(results)} results for #{length(of_family)} messages of #{family}"
          ]
        end
      end)

    failures = Enum.reject(singly ++ together, &is_nil/1)
    assert failures == [], Enum.join(failures, "\n\n")
  end

  # A case's message as MDL.decode/3 takes it: lowercase hex, spaces
  # between bytes allowed, or a JSON family's document as its text.
  defp message(message) do
    payload =
      case message do
        %{"hex" => hex} ->
          hex |> String.split(" ", trim: true) |> Enum.map_join(&Base.decode16!(&1, case: :lower))

        %{"json" => text} ->
          text
      end

    %{
      payload: payload,
      metadata:
        Map.new(message["metadata"], fn {name, value} ->
          {name, Expected.integer_or_text(value)}
        end),
      ground_time: Fixtures.nanos(message["ground_time"])
    }
  end

  # nil when the binding's result is the case's (spec/conformance.md
  # section 2.1), else what differs. A decoded message's def_version is
  # the digest describe gives for its set; an error's code is compared,
  # and its problems must be none.
  defp failure(kase, actual, definitions, versions, how) do
    case {kase, actual} do
      {%{"decoded" => decoded}, {:ok, _}} ->
        expected =
          decoded
          |> Map.put("def_version", Map.fetch!(versions, decoded["set"]))
          |> Expected.decoded(definitions)

        if Expected.exact(actual) === Expected.exact({:ok, expected}),
          do: nil,
          else:
            "#{kase["name"]}#{how}: expected #{inspect({:ok, expected})}, got #{inspect(actual)}"

      {%{"error" => code}, {:error, %MDL.Error{code: got, problems: []}}} ->
        if Atom.to_string(got) == code,
          do: nil,
          else: "#{kase["name"]}#{how}: expected the error #{code}, got #{inspect(actual)}"

      {%{"error" => code}, _} ->
        "#{kase["name"]}#{how}: expected the error #{code}, got #{inspect(actual)}"

      {%{"decoded" => _}, _} ->
        "#{kase["name"]}#{how}: expected a decoded message, got #{inspect(actual)}"
    end
  end

  # An encoding file (spec/conformance.md section 3): each case's request
  # encoded without bindings, the result compared as a decoded message's.
  defp run_encoding(path) do
    file = path |> File.read!() |> JSON.decode!()
    definitions = file["definitions"]
    # A store of the file's own, apart from the decoding files' stores.
    name =
      "encoding-" <>
        (path |> Path.relative_to(@encoding_dir) |> Path.rootname() |> String.replace("/", "-"))

    store = Fixtures.build_definitions!(definitions, name)
    {:ok, mdl} = MDL.open(store)
    {:ok, description} = MDL.describe(mdl)
    versions = Map.new(description["sets"], &{&1["name"], &1["def_version"]})

    failures =
      file["cases"]
      |> Enum.map(fn kase ->
        actual = MDL.encode(mdl, kase["family"], request(kase["request"]))
        encoding_failure(kase, actual, definitions, versions)
      end)
      |> Enum.reject(&is_nil/1)

    assert failures == [], Enum.join(failures, "\n\n")
  end

  # A case's request as MDL.encode/3 takes it, each argument's tagged value
  # as the binding's own (spec/conformance.md section 3). This reads a
  # well-formed file; the runtime's runner, in the same CI, is the one
  # that refuses a malformed value (a second key, an integer's other
  # spellings, a list of lists).
  defp request(request) do
    target =
      case request do
        %{"native_id" => id} -> Expected.integer_or_text(id)
        _ -> nil
      end

    %{
      set: request["set"],
      command: request["command"],
      arguments: Map.new(request["arguments"], fn {name, value} -> {name, argument(value)} end),
      native_id: target
    }
  end

  defp argument(%{"integer" => n}) when is_integer(n), do: n
  defp argument(%{"integer" => digits}) when is_binary(digits), do: String.to_integer(digits)
  defp argument(%{"float" => %{"non_finite" => "NaN"}}), do: :nan
  defp argument(%{"float" => %{"non_finite" => "inf"}}), do: :infinity
  defp argument(%{"float" => %{"non_finite" => "-inf"}}), do: :neg_infinity
  defp argument(%{"float" => x}) when is_float(x), do: x
  defp argument(%{"boolean" => b}) when is_boolean(b), do: b
  defp argument(%{"text" => text}) when is_binary(text), do: text
  defp argument(%{"bytes" => hex}), do: {:bytes, Base.decode16!(hex, case: :lower)}
  defp argument(%{"list" => values}) when is_list(values), do: Enum.map(values, &argument/1)

  defp encoding_failure(kase, actual, definitions, versions) do
    case {kase, actual} do
      {%{"encoded" => encoded}, {:ok, _}} ->
        expected =
          encoded
          |> Map.put("def_version", Map.fetch!(versions, encoded["set"]))
          |> Expected.encoded(definitions)

        if Expected.exact(actual) === Expected.exact({:ok, expected}),
          do: nil,
          else: "#{kase["name"]}: expected #{inspect({:ok, expected})}, got #{inspect(actual)}"

      {%{"error" => code}, {:error, %MDL.Error{code: got, problems: []}}} ->
        if Atom.to_string(got) == code,
          do: nil,
          else: "#{kase["name"]}: expected the error #{code}, got #{inspect(actual)}"

      {%{"error" => code}, _} ->
        "#{kase["name"]}: expected the error #{code}, got #{inspect(actual)}"

      {%{"encoded" => _}, _} ->
        "#{kase["name"]}: expected an encoded command, got #{inspect(actual)}"
    end
  end

  # A command decoding file (spec/conformance.md section 4): each case's
  # payload and metadata decoded as a command with its set.
  defp run_command_decoding(path) do
    file = path |> File.read!() |> JSON.decode!()
    definitions = file["definitions"]
    # A store of the file's own, apart from the other suites' stores.
    name =
      "commands-" <>
        (path |> Path.relative_to(@commands_dir) |> Path.rootname() |> String.replace("/", "-"))

    store = Fixtures.build_definitions!(definitions, name)
    {:ok, mdl} = MDL.open(store)
    {:ok, description} = MDL.describe(mdl)
    versions = Map.new(description["sets"], &{&1["name"], &1["def_version"]})

    failures =
      file["cases"]
      |> Enum.map(fn kase ->
        message = message(Map.put(kase["message"], "ground_time", "1970-01-01T00:00:00Z"))

        command_message = %{
          set: kase["set"],
          payload: message.payload,
          metadata: message.metadata
        }

        actual = MDL.decode_command(mdl, kase["family"], command_message)
        command_failure(kase, actual, definitions, versions)
      end)
      |> Enum.reject(&is_nil/1)

    assert failures == [], Enum.join(failures, "\n\n")
  end

  defp command_failure(kase, actual, definitions, versions) do
    case {kase, actual} do
      {%{"decoded" => decoded}, {:ok, _}} ->
        expected =
          decoded
          |> Map.put("def_version", Map.fetch!(versions, decoded["set"]))
          |> Expected.decoded_command(definitions)

        if Expected.exact(actual) === Expected.exact({:ok, expected}),
          do: nil,
          else: "#{kase["name"]}: expected #{inspect({:ok, expected})}, got #{inspect(actual)}"

      {%{"error" => code}, {:error, %MDL.Error{code: got, problems: []}}} ->
        if Atom.to_string(got) == code,
          do: nil,
          else: "#{kase["name"]}: expected the error #{code}, got #{inspect(actual)}"

      {%{"error" => code}, _} ->
        "#{kase["name"]}: expected the error #{code}, got #{inspect(actual)}"

      {%{"decoded" => _}, _} ->
        "#{kase["name"]}: expected a decoded command, got #{inspect(actual)}"
    end
  end

  defp group_in_order(cases) do
    families = cases |> Enum.map(& &1["family"]) |> Enum.uniq()
    Enum.map(families, fn family -> {family, Enum.filter(cases, &(&1["family"] == family))} end)
  end
end
