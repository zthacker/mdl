defmodule MDL.Problem do
  @moduledoc """
  One of the problems an error or a handle's warning stands for.

  * `:code`: an atom, stable across releases.
  * `:message`: for people.
  * `:path`: where it is, a JSON Pointer into the bindings file or into
    the build as one bundle (empty for the whole of either).
  """

  defstruct [:code, :message, :path]

  @type t :: %__MODULE__{code: atom(), message: String.t(), path: String.t()}
end
