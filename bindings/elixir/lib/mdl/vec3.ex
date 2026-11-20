defmodule MDL.Vec3 do
  @moduledoc """
  A vector param's value, in the frame of the entries it came from.
  """

  defstruct [:frame, :x, :y, :z]

  @type t :: %__MODULE__{
          frame: MDL.frame(),
          x: MDL.float_value(),
          y: MDL.float_value(),
          z: MDL.float_value()
        }
end
