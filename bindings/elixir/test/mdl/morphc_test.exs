defmodule MDL.MorphcTest do
  # The binding's results against morphc's for the same inputs. morphc
  # writes the runtime's results as JSON (spec/api.md), which
  # MDL.Test.Expected reads as the Elixir values the binding must return.
  # Compared with === with each float as its bits, so an integer and a
  # float of one value differ, and so do 0.0 and -0.0 on every OTP release.
  use ExUnit.Case, async: true

  alias MDL.Test.{Expected, Fixtures}

  setup_all do
    %{bundle: Fixtures.kitchen_bundle(), store: Fixtures.store(:kitchen)}
  end

  test "each kitchen sink message decodes as morphc decodes it, with bindings", context do
    {:ok, mdl} = MDL.open(context.store, bindings: Fixtures.clean_bindings())
    assert_decodes_as_morphc(mdl, context, Fixtures.store(:clean_bindings))
  end

  # Without bindings morphc decodes only a family it can choose a set for,
  # one with a single set or a version locator; the runtime refuses the
  # others' messages one by one (missing_set, tested in MDLTest).
  test "each kitchen sink message decodes as morphc decodes it, without bindings", context do
    {:ok, mdl} = MDL.open(context.store)
    assert_decodes_as_morphc(mdl, context, nil, &chooses_set?(&1, context.bundle))
  end

  # family, set, command, the binding's arguments, morphc's, the target
  @requests [
    {"vendor_x", "vendor_x_v2", "eps.load_table",
     %{"table" => {:bytes, <<0xC0, 0xFF, 0xEE>>}, "slot" => 3}, ["table=c0ffee", "slot=3"], nil},
    {"vendor_x", "vendor_x_v2", "eps.set_mode", %{"mode" => "SAFE", "delay_s" => 1.5},
     ["mode=SAFE", "delay_s=1.5"], nil},
    # Vendor X's packet requires delay_s, so the catalog's default isn't
    # sent with it.
    {"vendor_x", "vendor_x_v2", "eps.set_mode", %{"mode" => "SAFE"}, ["mode=SAFE"], nil},
    {"vendor_j", "vendor_j_v1", "eps.set_mode", %{"mode" => "NOMINAL", "delay_s" => 2.5},
     ["mode=NOMINAL", "delay_s=2.5"], nil},
    {"legacy", "legacy_v1", "eps.set_mode", %{"mode" => "SAFE", "delay_s" => 10.0},
     ["mode=SAFE", "delay_s=10.0"], nil},
    {"sink", "sink_v1", "sink.configure",
     %{"enable" => false, "gain" => 2.0, "label" => "OK", "taps" => [1.0, 2.0]},
     ["enable=false", "gain=2.0", "label=OK", "taps=[1.0, 2.0]"], 7},
    {"sink", "sink_v1", "sink.mark", %{"stamp" => 1_152_921_504_606_846_975},
     ["stamp=1152921504606846975"], nil},
    {"sink", "sink_v1", "sink.mark", %{"stamp" => 0}, ["stamp=0"], nil},
    {"vendor_x", "vendor_x_v2", "eps.set_mode", %{"mode" => "BOGUS"}, ["mode=BOGUS"], nil},
    # A state of the catalog's that vendor X's packet doesn't accept, and
    # vendor J's string field writes.
    {"vendor_x", "vendor_x_v2", "eps.set_mode", %{"mode" => "PAYLOAD"}, ["mode=PAYLOAD"], nil},
    {"vendor_j", "vendor_j_v1", "eps.set_mode", %{"mode" => "PAYLOAD"}, ["mode=PAYLOAD"], nil}
  ]

  test "each command request encodes as morphc encodes it", context do
    {:ok, mdl} = MDL.open(context.store)

    for {family, set, command, arguments, morphc_args, target} <- @requests do
      request = %{set: set, command: command, arguments: arguments, native_id: target}
      actual = MDL.encode(mdl, family, request)

      expected =
        case Fixtures.morphc_encode(context.store, set, command, morphc_args, target) do
          {:ok, json} -> {:ok, Expected.encoded(json, context.bundle)}
          {:error, json} -> {:error, Expected.error(json)}
        end

      assert Expected.exact(actual) === Expected.exact(expected),
             "#{set} #{command} #{inspect(arguments)}"
    end
  end

  defp assert_decodes_as_morphc(mdl, context, bindings, family? \\ fn _ -> true end) do
    for {family, fixtures} <- Enum.group_by(Fixtures.messages(), & &1["family"]),
        family?.(family) do
      expected =
        context.store
        |> Fixtures.morphc_decode(family, fixtures, bindings)
        |> Enum.map(&expect(&1, context.bundle))

      messages = Enum.map(fixtures, &Fixtures.message/1)

      for {fixture, message, expected} <- Enum.zip([fixtures, messages, expected]) do
        assert Expected.exact(MDL.decode(mdl, family, message)) === Expected.exact(expected),
               fixture["name"]
      end

      assert Expected.exact(MDL.decode_many(mdl, family, messages)) === Expected.exact(expected)
    end
  end

  defp chooses_set?(family, bundle) do
    declared = Enum.find(bundle["families"], &(&1["name"] == family))
    sets = Enum.count(bundle["sets"], &(&1["family"] == family))
    Map.has_key?(declared, "version") or sets == 1
  end

  defp expect({:ok, json}, bundle), do: {:ok, Expected.decoded(json, bundle)}
  defp expect({:error, json}, _bundle), do: {:error, Expected.error(json)}
end
