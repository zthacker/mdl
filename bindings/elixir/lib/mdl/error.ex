defmodule MDL.Error do
  @moduledoc """
  Why an operation failed (spec/api.md section 4), returned as
  `{:error, %MDL.Error{}}`, never raised by MDL itself. It's an exception,
  so a caller may raise it.

  * `:code`: an atom that names the problem and keeps its meaning across
    releases (`:unknown_family`, `:past_end`, `:invalid_build`). Match on
    it, not on the message.
  * `:message`: for people; it may change in any release.
  * `:problems`: for an error that stands for several (a build that
    doesn't validate, bindings with problems on several rows), each as an
    `MDL.Problem`; otherwise empty.
  """

  defexception [:code, :message, problems: []]

  @type t :: %__MODULE__{code: atom(), message: String.t(), problems: [MDL.Problem.t()]}
end
