defmodule MDL.Quat do
  @moduledoc """
  A quaternion param's value, scalar first, in the frame of the entries it
  came from.
  """

  defstruct [:frame, :w, :x, :y, :z]

  @type t :: %__MODULE__{
          frame: MDL.frame(),
          w: MDL.float_value(),
          x: MDL.float_value(),
          y: MDL.float_value(),
          z: MDL.float_value()
        }
end
