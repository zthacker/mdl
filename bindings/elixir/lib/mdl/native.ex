defmodule MDL.Native do
  @moduledoc false
  # The NIFs of native/mdl_nif, which replace these stubs when the module
  # loads. MDL wraps each.

  use Rustler, otp_app: :mdl, crate: "mdl_nif"

  def open(_dir, _bindings), do: :erlang.nif_error(:nif_not_loaded)
  def open_bytes(_manifest, _artifacts, _bindings), do: :erlang.nif_error(:nif_not_loaded)
  def warnings(_handle), do: :erlang.nif_error(:nif_not_loaded)
  def with_bindings(_handle, _bindings), do: :erlang.nif_error(:nif_not_loaded)
  def decode(_handle, _family, _message), do: :erlang.nif_error(:nif_not_loaded)
  def decode_many(_handle, _family, _messages), do: :erlang.nif_error(:nif_not_loaded)
  def encode(_handle, _family, _request), do: :erlang.nif_error(:nif_not_loaded)
  def describe(_handle), do: :erlang.nif_error(:nif_not_loaded)
  def binding(_handle, _family, _native_id, _at), do: :erlang.nif_error(:nif_not_loaded)
  def asset(_handle, _family, _asset, _at), do: :erlang.nif_error(:nif_not_loaded)
end
