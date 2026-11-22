defmodule MDL.ConformanceTest do
  # The language-neutral decoding cases in conformance/decoding
  # (spec/conformance.md section 2), through the binding's own API. morphc
  # builds each file's definitions. Each expected result, worked out from
  # the spec by hand, is read by MDL.Test.Expected as the Elixir values
  # the binding must return, and compared with === with each float as its
  # bits, so an integer and a float of one value differ, and so do 0.0 and
  # -0.0 on every OTP release.
  use ExUnit.Case, async: true

  alias MDL.Test.{Expected, Fixtures}

  @dir Path.expand("../../../../conformance/decoding", __DIR__)
  @files @dir |> Path.join("*.json") |> Path.wildcard() |> Enum.sort()

  test "there are decoding cases to run" do
    assert @files != [], "no case files in #{@dir}"
  end

  for path <- @files do
    @external_resource path

    test "the decoding cases of #{Path.basename(path)}" do
      run(unquote(path))
    end
  end

  defp run(path) do
    file = path |> File.read!() |> JSON.decode!()
    definitions = file["definitions"]
    store = Fixtures.build_definitions!(definitions, Path.basename(path, ".json"))

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

  defp group_in_order(cases) do
    families = cases |> Enum.map(& &1["family"]) |> Enum.uniq()
    Enum.map(families, fn family -> {family, Enum.filter(cases, &(&1["family"] == family))} end)
  end
end
