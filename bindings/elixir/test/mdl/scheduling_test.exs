defmodule MDL.SchedulingTest do
  # MDL's work runs on dirty schedulers, so a decode that takes many
  # milliseconds never holds a normal one. The system monitor reports any
  # process that runs uninterrupted on a normal scheduler past its limit.
  # Not async: the system monitor is the node's.
  use ExUnit.Case, async: false

  alias MDL.Test.Fixtures

  test "a long decode leaves the normal schedulers free" do
    {:ok, mdl} = MDL.open(Fixtures.store(:kitchen), bindings: Fixtures.clean_bindings())
    # A dense JSON document: 131,072 array elements, about 256 KiB, which
    # takes tens of milliseconds to decode.
    values = Enum.map_join(1..131_072, ",", fn _ -> "1" end)
    json = ~s({"node":"node-7","type":"status","t":1.5,"armed":true,"pad":[#{values}]})
    message = %{payload: json, ground_time: Fixtures.nanos("2026-03-01T00:00:00Z")}

    previous = :erlang.system_monitor(self(), [{:long_schedule, 5}])

    try do
      # In a process of its own: the monitor reports on other processes, and
      # only a report on this one counts, so no other process's stall on a
      # busy machine can fail the test.
      task = Task.async(fn -> :timer.tc(fn -> MDL.decode(mdl, "sink_json", message) end) end)
      {elapsed, {:ok, _}} = Task.await(task)
      decoder = task.pid
      assert elapsed > 5_000, "the decode took #{elapsed} µs, too short to show anything"
      refute_receive {:monitor, ^decoder, :long_schedule, _}, 100
    after
      case previous do
        :undefined -> :erlang.system_monitor(:undefined)
        {pid, options} -> :erlang.system_monitor(pid, options)
      end
    end
  end
end
