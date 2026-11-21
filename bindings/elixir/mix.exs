defmodule MDL.MixProject do
  use Mix.Project

  # The runtime release this binding bundles, whose version it shares
  # (spec/compatibility.md section 6).
  @version "0.2.0"

  def project do
    [
      app: :mdl,
      version: @version,
      elixir: "~> 1.18",
      elixirc_paths: elixirc_paths(Mix.env()),
      start_permanent: Mix.env() == :prod,
      deps: deps(),
      name: "MDL",
      description:
        "MDL's runtime for Elixir: decode telemetry and encode commands with the " <>
          "artifact stores morphc builds",
      source_url: "https://github.com/zthacker/mdl",
      package: package(),
      docs: [
        main: "MDL",
        source_url_pattern:
          "https://github.com/zthacker/mdl/blob/v#{@version}/bindings/elixir/%{path}#L%{line}"
      ]
    ]
  end

  def application do
    [extra_applications: []]
  end

  # The tests' fixtures and the reference converter, compiled for tests only.
  defp elixirc_paths(:test), do: ["lib", "test/support"]
  defp elixirc_paths(_), do: ["lib"]

  defp deps do
    [
      {:rustler_precompiled, "~> 0.10"},
      # Builds the native library from source in a git checkout
      # (lib/mdl/native.ex); from a Hex package it's downloaded instead.
      {:rustler, "~> 0.38", optional: true, runtime: false},
      {:ex_doc, "~> 0.40", only: :dev, runtime: false}
    ]
  end

  # Precompiled native libraries only (RELEASING.md): the native source
  # depends on the repository's mdl crate, which isn't in the package. The
  # checksum file says which libraries the package may load.
  defp package do
    [
      licenses: ["MIT", "Apache-2.0"],
      links: %{"GitHub" => "https://github.com/zthacker/mdl"},
      files: ~w(lib mix.exs README.md LICENSE-MIT LICENSE-APACHE checksum-*.exs)
    ]
  end
end
