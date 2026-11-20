defmodule MDL.Sample do
  @moduledoc """
  One catalog param's value from a decoded message (spec/api.md
  section 3.1).

  * `:param`: the catalog param, a string.
  * `:value`: in the param's type and unit when `:quality` is `:good`;
    otherwise the raw value, as in the record. Check the quality before
    storing or plotting a value.
  * `:unit`: the param's unit, a string, or `nil`.
  * `:path`: the entry it came from, as a record path (`"batt.volts"`,
    `"samples[3]"`); for a composite, its members' paths joined by `,`.
  * `:index`: its position in its array, or `nil`.
  * `:time_offset_ns`: how long after the packet's time it was taken, or
    `nil`.
  * `:quality`: an atom, `t:MDL.quality/0`.
  """

  defstruct [:param, :value, :unit, :path, :index, :time_offset_ns, :quality]

  @type t :: %__MODULE__{
          param: String.t(),
          value: MDL.value(),
          unit: String.t() | nil,
          path: String.t(),
          index: non_neg_integer() | nil,
          time_offset_ns: integer() | nil,
          quality: MDL.quality()
        }
end
