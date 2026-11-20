defmodule MDL.Route do
  @moduledoc """
  What a native id is at an instant, as decoding resolves it (spec/api.md
  section 3.4).

  * `:family`, `:asset`: strings.
  * `:native_id`: an integer or a string.
  * `:set`: the set that decodes the asset's messages, or `nil` when the
    family's version locator chooses.
  * `:stand_in`: true when the handle has no bindings and the native id
    stands for an asset of its own.
  """

  defstruct [:family, :native_id, :asset, :set, :stand_in]

  @type t :: %__MODULE__{
          family: String.t(),
          native_id: MDL.native_id(),
          asset: String.t(),
          set: String.t() | nil,
          stand_in: boolean()
        }
end
