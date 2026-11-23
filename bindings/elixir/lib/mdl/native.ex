defmodule MDL.Native do
  @moduledoc false
  # The NIFs of native/mdl_nif, which replace these stubs when the module
  # loads. MDL wraps each.
  #
  # From Hex, whose package holds the checksum file, the native library is
  # one precompiled for this version and downloaded from its GitHub release,
  # its checksum checked. In a git checkout (the repository's tests and CI, a
  # git dependency), Rustler builds it from source, as it does for
  # MDL_BUILD=1 in a checkout that holds a checksum file. A Hex install has
  # no native source to build.

  version = Mix.Project.config()[:version]
  checksum_file = Path.expand("../../checksum-Elixir.MDL.Native.exs", __DIR__)
  force_build = System.get_env("MDL_BUILD") in ["1", "true"] or not File.exists?(checksum_file)
  @external_resource checksum_file
  @checksum_file checksum_file
  @force_build force_build

  use RustlerPrecompiled,
    otp_app: :mdl,
    crate: "mdl_nif",
    version: version,
    base_url: "https://github.com/zthacker/mdl/releases/download/v#{version}",
    targets: ~w(
      aarch64-apple-darwin
      x86_64-apple-darwin
      aarch64-unknown-linux-gnu
      x86_64-unknown-linux-gnu
      aarch64-unknown-linux-musl
      x86_64-unknown-linux-musl
    ),
    # Rustler's default, which every supported OTP loads; scripts/nif.sh
    # builds for it.
    nif_versions: ["2.15"],
    force_build: force_build

  # Mix tracks the checksum file but not MDL_BUILD, so it asks this, on
  # every compile, whether the mode the module was compiled in still holds.
  @doc false
  def __mix_recompile__? do
    force_build = System.get_env("MDL_BUILD") in ["1", "true"] or not File.exists?(@checksum_file)
    force_build != @force_build
  end

  def open(_dir, _bindings), do: :erlang.nif_error(:nif_not_loaded)
  def open_bytes(_manifest, _artifacts, _bindings), do: :erlang.nif_error(:nif_not_loaded)
  def warnings(_handle), do: :erlang.nif_error(:nif_not_loaded)
  def with_bindings(_handle, _bindings), do: :erlang.nif_error(:nif_not_loaded)
  def decode(_handle, _family, _message), do: :erlang.nif_error(:nif_not_loaded)
  def decode_many(_handle, _family, _messages), do: :erlang.nif_error(:nif_not_loaded)
  def encode(_handle, _family, _request), do: :erlang.nif_error(:nif_not_loaded)
  def decode_command(_handle, _family, _message), do: :erlang.nif_error(:nif_not_loaded)
  def instant(_event_time, _booted), do: :erlang.nif_error(:nif_not_loaded)
  def describe(_handle), do: :erlang.nif_error(:nif_not_loaded)
  def binding(_handle, _family, _native_id, _at), do: :erlang.nif_error(:nif_not_loaded)
  def asset(_handle, _family, _asset, _at), do: :erlang.nif_error(:nif_not_loaded)
end
