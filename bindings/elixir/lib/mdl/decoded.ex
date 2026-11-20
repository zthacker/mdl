defmodule MDL.Decoded do
  @moduledoc """
  A decoded message (spec/api.md section 3.1).

  * `:family`, `:asset`, `:set`, `:packet`: strings.
  * `:native_id`: what the family's alias locator read, an integer or a
    string.
  * `:stand_in`: true when the handle has no bindings and the native id
    stands for an asset of its own.
  * `:def_version`: the set artifact's digest, a string.
  * `:event_time`: when the values were taken: `{:ground, ns}` (the
    message's ground time, when the packet gives none), `{:at, ns}`, or
    `{:since_boot, count, unit}` with `unit` one of `:s`, `:ms`, `:us` and
    `:ns`, which isn't an instant until correlated. Instants are integer
    nanoseconds since 1970-01-01T00:00:00Z.
  * `:samples`: `MDL.Sample` structs, in the record's order.
  * `:record`: each entry's raw value, by name: a map with string keys.
  * `:engineering`: `{path, value, quality}` for each field and scalar
    element, in the record's order. A value whose quality isn't `:good` is
    the raw value.
  """

  defstruct [
    :family,
    :native_id,
    :asset,
    :stand_in,
    :set,
    :def_version,
    :packet,
    :event_time,
    :samples,
    :record,
    :engineering
  ]

  @type t :: %__MODULE__{
          family: String.t(),
          native_id: MDL.native_id(),
          asset: String.t(),
          stand_in: boolean(),
          set: String.t(),
          def_version: String.t(),
          packet: String.t(),
          event_time: MDL.event_time(),
          samples: [MDL.Sample.t()],
          record: %{optional(String.t()) => MDL.value()},
          engineering: [{String.t(), MDL.value(), MDL.quality()}]
        }
end
