defmodule MDL.DecodedCommand do
  @moduledoc """
  A decoded command (spec/api.md section 3.5).

  * `:family`, `:set`, `:packet`, `:command`: strings.
  * `:def_version`: the set artifact's digest.
  * `:arguments`: every argument of the catalog command, by name: a map of
    strings to values of each argument's type (an integer, a float, a
    boolean, a string for text or an enum's state, a binary for bytes, or
    a list of them). A float is always finite: a payload that gives one
    that isn't is the error `:non_finite`.
  * `:defaulted`: the arguments the set doesn't encode, which have their
    defaults, in the catalog's order.
  * `:target`: the target's native id, an integer or a string, for a
    packet that holds it; `nil` otherwise.
  * `:record`: each field's and checksum's raw value, as decoding the
    payload gives them.
  """

  defstruct [
    :family,
    :set,
    :def_version,
    :packet,
    :command,
    :arguments,
    :defaulted,
    :target,
    :record
  ]

  @type t :: %__MODULE__{
          family: String.t(),
          set: String.t(),
          def_version: String.t(),
          packet: String.t(),
          command: String.t(),
          arguments: %{optional(String.t()) => MDL.value()},
          defaulted: [String.t()],
          target: MDL.native_id() | nil,
          record: %{optional(String.t()) => MDL.value()}
        }
end
