defmodule MDL.MixProject do
  use Mix.Project

  # The runtime release this binding bundles, whose version it shares
  # (spec/compatibility.md section 6).
  @version "0.1.0"

  def project do
    [
      app: :mdl,
      version: @version,
      elixir: "~> 1.18",
      elixirc_paths: elixirc_paths(Mix.env()),
      start_permanent: Mix.env() == :prod,
      deps: deps(),
      name: "MDL",
      source_url: "https://github.com/zthacker/mdl",
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
      {:rustler, "~> 0.38", runtime: false},
      {:ex_doc, "~> 0.40", only: :dev, runtime: false}
    ]
  end
end
