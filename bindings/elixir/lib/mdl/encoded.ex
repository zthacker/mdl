defmodule MDL.Encoded do
  @moduledoc """
  An encoded command (spec/api.md section 3.2).

  * `:family`, `:set`, `:packet`, `:command`: strings.
  * `:def_version`: the set artifact's digest, to keep with what was sent.
  * `:payload`: what the link sends, a binary; a JSON family's payload is
    its UTF-8 text.
  * `:metadata`: the values the link frames the payload with, by name: a
    map of strings to integers or strings.
  * `:defaulted`: the arguments the request left out, which took their
    defaults, in the catalog's order.
  * `:deprecated`: the catalog's deprecation note, or `nil`.
  * `:record`: each field's and checksum's raw value, as decoding the
    payload gives them.
  """

  defstruct [
    :family,
    :set,
    :def_version,
    :packet,
    :command,
    :payload,
    :metadata,
    :defaulted,
    :deprecated,
    :record
  ]

  @type t :: %__MODULE__{
          family: String.t(),
          set: String.t(),
          def_version: String.t(),
          packet: String.t(),
          command: String.t(),
          payload: binary(),
          metadata: %{optional(String.t()) => integer() | String.t()},
          defaulted: [String.t()],
          deprecated: String.t() | nil,
          record: %{optional(String.t()) => MDL.value()}
        }
end
