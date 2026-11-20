defmodule MDL.AtomsTest do
  # Nothing from definitions or messages becomes an atom, so a node that
  # keeps reloading stores and decoding new names never grows its atom
  # table. Not async: other tests' atoms would count.
  use ExUnit.Case, async: false

  alias MDL.Test.Fixtures

  test "reloading stores and decoding names never seen creates no atoms" do
    rounds = fn n ->
      {:ok, mdl} = MDL.open(Fixtures.store(:kitchen), bindings: Fixtures.clean_bindings())
      {:ok, %{}} = MDL.describe(mdl)
      [] = MDL.warnings(mdl)

      for fixture <- Fixtures.messages() do
        message = Fixtures.message(fixture)
        # A metadata name and text no definition has, new each round.
        metadata = Map.put(message.metadata, "unseen-#{n}", "value-#{n}")
        MDL.decode(mdl, fixture["family"], %{message | metadata: metadata})
      end

      {:ok, nil} = MDL.asset(mdl, "sink", "ASSET-#{n}", 0)
      {:ok, bare} = MDL.with_bindings(mdl, nil)
      {:ok, %MDL.Route{}} = MDL.binding(bare, "sink_json", "node-#{n}", 0)
      {:error, %MDL.Error{}} = MDL.decode(bare, "family-#{n}", %{payload: <<>>, ground_time: 0})
    end

    # The first round loads what it uses and makes MDL's own atoms.
    rounds.(0)
    before = :erlang.system_info(:atom_count)
    Enum.each(1..20, rounds)
    assert :erlang.system_info(:atom_count) == before
  end
end
