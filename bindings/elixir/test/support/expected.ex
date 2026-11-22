defmodule MDL.Test.Expected do
  @moduledoc false
  # What the binding must return for a result morphc writes as JSON, by
  # spec/api.md sections 1, 3 and 4 and the binding's Elixir forms (lib/mdl.ex),
  # worked out here apart from the binding's own conversion. JSON spells
  # bytes as hex and integers past 2^53 as decimal strings, so a value is
  # read by the type the bundle's catalog or set declares for it. The JSON
  # is held to those sections as it's read: a result's object has exactly
  # the keys they list, and an integer has its one spelling.

  # The largest integer JSON writes as a number (spec/api.md section 1).
  @max_safe 9_007_199_254_740_991

  @decoded_keys ~w(family native_id asset stand_in set def_version packet event_time samples record engineering)
  @encoded_keys ~w(family set def_version packet command payload metadata defaulted deprecated record)
  @sample_keys ~w(param value unit path index time_offset_ns quality)

  @doc "An `MDL.Decoded` from a decoded message's JSON."
  def decoded(json, bundle) do
    keys!(json, @decoded_keys)
    set = set!(bundle, json["set"])
    packet = Enum.find(set["packets"], &(&1["name"] == json["packet"]))
    types = field_types(packet["entries"], set, "")
    params = bundle["catalog"]["params"]

    %MDL.Decoded{
      family: json["family"],
      native_id: integer_or_text(json["native_id"]),
      asset: json["asset"],
      stand_in: json["stand_in"],
      set: json["set"],
      def_version: json["def_version"],
      packet: json["packet"],
      event_time: event_time(json["event_time"]),
      samples: Enum.map(json["samples"], &sample(&1, params, types)),
      record: record(json["record"], packet["entries"], set),
      engineering:
        Enum.map(json["engineering"], fn engineering ->
          %{"path" => path, "value" => value, "quality" => q} =
            keys!(engineering, ~w(path value quality))

          {path, engineering(value, Map.fetch!(types, normal(path))), quality(q)}
        end)
    }
  end

  @doc "An `MDL.Encoded` from an encoded command's JSON."
  def encoded(json, bundle) do
    keys!(json, @encoded_keys)
    set = set!(bundle, json["set"])
    packet = Enum.find(set["commands"], &(&1["name"] == json["packet"]))

    %MDL.Encoded{
      family: json["family"],
      set: json["set"],
      def_version: json["def_version"],
      packet: json["packet"],
      command: json["command"],
      # Hex for a JSON family too: its payload is the document's text.
      payload: Base.decode16!(json["payload"], case: :lower),
      metadata: Map.new(json["metadata"], fn {name, value} -> {name, integer_or_text(value)} end),
      defaulted: json["defaulted"],
      deprecated: json["deprecated"],
      record: record(json["record"], packet["entries"], set)
    }
  end

  @doc "An `MDL.Error` from an error's JSON."
  def error(json) do
    %MDL.Error{
      code: String.to_atom(json["code"]),
      message: json["message"],
      problems:
        Enum.map(json["problems"] || [], fn problem ->
          %MDL.Problem{
            code: String.to_atom(problem["code"]),
            message: problem["message"],
            path: problem["path"]
          }
        end)
    }
  end

  defp set!(bundle, name), do: Enum.find(bundle["sets"], &(&1["name"] == name))

  # An object of a result (spec/api.md section 3) has every key listed for
  # it, an absent value being null, and no others.
  defp keys!(json, keys) do
    given = json |> Map.keys() |> Enum.sort()

    if given != Enum.sort(keys) do
      raise ArgumentError, "expected the keys #{inspect(Enum.sort(keys))}, got #{inspect(given)}"
    end

    json
  end

  # Each field's and scalar element's type by its path, an array's indexes
  # written `[]` (`samples[]`, `cells[].volts`).
  defp field_types(entries, set, prefix) do
    Enum.reduce(entries, %{}, fn entry, types ->
      name = prefix <> (entry["name"] || "")

      case entry["kind"] do
        "field" ->
          Map.put(types, name, entry["type"])

        "checksum" ->
          Map.put(types, name, "uint")

        "struct" ->
          Map.merge(types, field_types(struct_entries(set, entry["struct"]), set, name <> "."))

        "array" ->
          case entry["element"] do
            %{"kind" => "scalar", "type" => type} ->
              Map.put(types, name <> "[]", type)

            %{"kind" => "struct", "name" => struct} ->
              Map.merge(types, field_types(struct_entries(set, struct), set, name <> "[]."))
          end

        "padding" ->
          types
      end
    end)
  end

  defp struct_entries(set, name), do: set["types"][name]["entries"]

  defp normal(path), do: String.replace(path, ~r/\[\d+\]/, "[]")

  # A record (spec/api.md section 3.1): each present entry by name.
  defp record(json, entries, set) do
    Map.new(json, fn {name, value} ->
      entry = Enum.find(entries, &(&1["name"] == name))
      {name, entry_value(value, entry, set)}
    end)
  end

  defp entry_value(value, %{"kind" => "field", "type" => type}, _set), do: raw(value, type)
  defp entry_value(value, %{"kind" => "checksum"}, _set), do: raw(value, "uint")

  defp entry_value(value, %{"kind" => "struct", "struct" => struct}, set) do
    record(value, struct_entries(set, struct), set)
  end

  defp entry_value(values, %{"kind" => "array", "element" => element}, set) do
    Enum.map(values, fn value ->
      case element do
        %{"kind" => "scalar", "type" => type} -> raw(value, type)
        %{"kind" => "struct", "name" => struct} -> record(value, struct_entries(set, struct), set)
      end
    end)
  end

  # A raw value of a field's type; `nil` for a JSON array's null element.
  defp raw(nil, _type), do: nil
  defp raw(hex, "bytes"), do: Base.decode16!(hex, case: :lower)
  defp raw(value, type) when type in ["uint", "int"], do: integer(value)
  defp raw(value, "float"), do: float(value)
  defp raw(value, type) when type in ["bool", "string"], do: value

  # A sample (spec/api.md section 3.1). A good value has the param's type;
  # any other quality's is the raw value, of the field it came from. A
  # composite's value is its frame and members whatever its quality.
  defp sample(json, params, types) do
    keys!(json, @sample_keys)

    %MDL.Sample{
      param: json["param"],
      value: sample_value(json, params[json["param"]]["type"], types),
      unit: json["unit"],
      path: json["path"],
      index: json["index"],
      time_offset_ns: json["time_offset_ns"],
      quality: quality(json["quality"])
    }
  end

  defp sample_value(%{"value" => %{"frame" => _} = value}, _param_type, _types),
    do: composite(value)

  defp sample_value(%{"value" => value, "quality" => "good"}, param_type, _types),
    do: good(value, param_type)

  defp sample_value(%{"value" => value, "path" => path}, _param_type, types),
    do: raw(value, Map.fetch!(types, normal(path)))

  defp good(value, "f64"), do: float(value)
  defp good(value, type) when type in ["i64", "u64"], do: integer(value)
  defp good(hex, "bytes"), do: Base.decode16!(hex, case: :lower)
  defp good(value, type) when type in ["bool", "str", "enum"], do: value

  # An engineering value (spec/api.md section 3.1): spelled as a sample's,
  # in the field's terms. A string is a state's name or text, but bytes for
  # a bytes field and an integer for an integer field without a state.
  defp engineering(%{"non_finite" => _} = value, _type), do: float(value)
  defp engineering(hex, "bytes") when is_binary(hex), do: Base.decode16!(hex, case: :lower)

  defp engineering(text, type) when is_binary(text) and type in ["uint", "int"] do
    if text =~ ~r/^-?\d+$/, do: integer(text), else: text
  end

  defp engineering(n, type) when is_integer(n) and type in ["uint", "int"], do: integer(n)

  defp engineering(value, _type), do: value

  defp composite(%{"frame" => frame} = value) do
    frame = frame |> String.downcase() |> String.to_atom()

    if Map.has_key?(value, "w") do
      %MDL.Quat{
        frame: frame,
        w: float(value["w"]),
        x: float(value["x"]),
        y: float(value["y"]),
        z: float(value["z"])
      }
    else
      %MDL.Vec3{frame: frame, x: float(value["x"]), y: float(value["y"]), z: float(value["z"])}
    end
  end

  defp event_time(%{"kind" => kind} = json) when kind in ["ground", "at"] do
    %{"time" => time} = keys!(json, ~w(kind time))
    {String.to_atom(kind), MDL.Test.Fixtures.nanos(time)}
  end

  defp event_time(%{"kind" => "since_boot"} = json) do
    %{"value" => value, "unit" => unit} = keys!(json, ~w(kind value unit))
    {:since_boot, integer(value), String.to_atom(unit)}
  end

  defp float(%{"non_finite" => "NaN"}), do: :nan
  defp float(%{"non_finite" => "inf"}), do: :infinity
  defp float(%{"non_finite" => "-inf"}), do: :neg_infinity
  defp float(x) when is_float(x), do: x

  # An integer's one JSON spelling (spec/api.md section 1): a number
  # within +-(2^53 - 1), and a decimal string past it.
  defp integer(n) when is_integer(n) and abs(n) <= @max_safe, do: n

  defp integer(digits) when is_binary(digits) do
    n = String.to_integer(digits)

    if abs(n) > @max_safe and Integer.to_string(n) == digits do
      n
    else
      raise ArgumentError, "#{inspect(digits)} isn't how JSON spells an integer"
    end
  end

  @doc """
  A native id or metadata value from JSON (spec/api.md section 1): an
  integer, or text, but a string in an integer's one decimal spelling
  within -2^63 to 2^64 - 1 is that integer.
  """
  def integer_or_text(n) when is_integer(n) and abs(n) <= @max_safe, do: n

  def integer_or_text(text) when is_binary(text) do
    if text =~ ~r/^(0|-?[1-9]\d*)$/ and
         String.to_integer(text) in -0x8000000000000000..0xFFFFFFFFFFFFFFFF do
      String.to_integer(text)
    else
      text
    end
  end

  defp quality(name), do: String.to_atom(name)

  @doc """
  A value with each float replaced by its 64 bits, so that === compares
  floats as spec/conformance.md section 2.2 does: before OTP 27, ===
  holds between 0.0 and -0.0. A struct keeps its module, so structs of two
  kinds still differ.
  """
  def exact(x) when is_float(x), do: {:float, <<x::float-64>>}
  def exact(%module{} = struct), do: {module, struct |> Map.from_struct() |> exact()}

  def exact(map) when is_map(map),
    do: Map.new(map, fn {key, value} -> {exact(key), exact(value)} end)

  def exact(list) when is_list(list), do: Enum.map(list, &exact/1)

  def exact(tuple) when is_tuple(tuple),
    do: tuple |> Tuple.to_list() |> exact() |> List.to_tuple()

  def exact(other), do: other
end
