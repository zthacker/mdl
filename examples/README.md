# Examples

Definitions for a small fleet, in every form MDL reads, and the files an
operator adds to run it.

| Path | What it holds |
| --- | --- |
| [`toml/mission`](toml/mission) | One catalog shared by three very different assets, in MDL's TOML: a CCSDS satellite (CUC time, an APID per packet, a calibration table, a command with a CRC), a satellite sending JSON lines (in Fahrenheit, converted to Celsius), and a MAVLink drone (little-endian, time since boot). |
| [`toml/mixed_versions`](toml/mixed_versions) | Two firmware versions on one link. A format byte in every message picks the set that decodes it. |
| [`xtce/demosat.xml`](xtce/demosat.xml) | A CCSDS satellite as XTCE tools describe one: inherited headers, APIDs, calibrators, a spline, an enumeration, vectors, commands with defaults, ranges, calibrated currents and a list of waypoint records. Its bus voltage, battery temperature and mode command map to the mission catalog's names through MDL aliases. |
| [`xtce/imager.xml`](xtce/imager.xml) | A payload whose packets have sizes the data gives: an optional secondary header, a row of pixels counted by the packet, Pascal strings, record arrays. |
| [`fleet`](fleet) | What an operator adds: a build file listing the fleet's definitions, a bindings file naming each asset, a drone's capture with each message's own metadata, and a param added to the catalog. |

The TOML and XTCE examples are also read, validated and decoded by their
crates' own tests (`cargo test -p morpheus-mdl-toml --test examples`,
`cargo test -p morpheus-mdl-xtce --test examples`); `fleet` is used by the
walkthrough below.

## Walkthrough: a fleet from definitions to commands

Four assets: the mission's CCSDS satellite, JSON satellite and MAVLink
drone, written by hand in TOML, and DemoSat, described by its vendor in
XTCE. The commands below run in this directory. Install morphc
with `cargo install --path crates/morphc` from the repository root, or put
`cargo run -q -p morpheus-morphc --` where they say `morphc`. Each one runs in CI
exactly as written, and its output is checked against what's shown here
(`cargo test -p morpheus-morphc --test walkthrough`).

### 1. Build the definitions into a store

```console
$ morphc build toml/mission xtce/demosat.xml --out store
warning[xtce.ignored]: SpaceSystem EPS's shortDescription isn't imported: MDL keeps descriptions of catalog params, commands and arguments only
   --> xtce/demosat.xml:106:27
    |
106 |   <SpaceSystem name="EPS" shortDescription="Electrical power">
    |                           ^^^^^^^^^^^^^^^^
    = and 2 more xtce.ignored warnings (--all-warnings shows them)

warning[overridden]: param eps.bus_voltage has description "Main bus voltage" here, but the earlier definition's "Main power bus voltage" takes precedence
   --> xtce/demosat.xml:155:38
    |
155 |                    shortDescription="Main bus voltage">
    |                                      ^^^^^^^^^^^^^^^^
    = the first is at toml/mission/catalog.toml:11:15
    = and 1 more overridden warning (--all-warnings shows them)

morphc: 5 warnings (3 xtce.ignored, 2 overridden)
catalog           33ee14711221d7edceeec1f978fcf73d0f26e7e7cf7a548665ad13314e6e3e40  (written)
family ccsds_sat  74cc510c26ac7222d96e9b55facd66f5a961b36782835ff363a83ac1ac676846  (written)
family demosat    3bec4e53d0c57acc228cfd6bb6645eb2bd240ff95804513d7706ac860b809597  (written)
family drone      1ae855cd51e6c6e1c42f03330f733d6e1fb77e8919a16eca2345b22dff3c699f  (written)
family json_sat   797620de34906b6d4570b7f0fdacdeabf930e9279c3609e1fb57276dc624132c  (written)
set ccsds_sat_v1  6293e3d77620f2f6f26f359bddb24e4c71a7d87e623947aa7d644378d8f365aa  (written)
set demosat       aaba1c63ceedf56546115fcb909204a46c023548ea92dbcb09d185366637e15d  (written)
set drone_v1      9cdb54d367681f74df4f0223c2b3cbbb8cbdd5909ca64839dc2dcfc8d7458617  (written)
set json_sat_v1   0121b63b4ecd51ef2d7ad449785df82eab1b0fbf227a7c5a0d0fb864236fa00d  (written)
ok: 17 params, 5 commands, 4 families, 4 sets (MDL IR 2.0); 9 new artifacts written to store
```

The TOML catalog comes first, so its curated descriptions win where
DemoSat's XTCE describes the same param differently; each override is a
warning, and warnings of one kind are shown once with a count. DemoSat's
parameters that alias catalog names (`eps.bus_voltage`, `eps.batt_temp`)
join them, and its others get names of their own (`demosat.eps.uptime`).
Both define the `eps.set_mode` command the same way, so it's one command
for both satellites.

`store/` now holds each artifact as `<digest>.json`, never rewritten, and a
`manifest.json` naming this build ([spec/store.md](../spec/store.md)). A set's
digest is the `def_version` stamped on everything it decodes.

### 2. Look inside

```console
$ morphc inspect store
catalog  33ee14711221d7edceeec1f978fcf73d0f26e7e7cf7a548665ad13314e6e3e40  17 params, 5 commands

family ccsds_sat  74cc510c26ac7222d96e9b55facd66f5a961b36782835ff363a83ac1ac676846
  binary, big-endian; asset id: bits 48..64; set chosen by binding
  metadata: apid
  set ccsds_sat_v1  6293e3d77620f2f6f26f359bddb24e4c71a7d87e623947aa7d644378d8f365aa  1 packet, 1 command

family demosat  3bec4e53d0c57acc228cfd6bb6645eb2bd240ff95804513d7706ac860b809597
  binary, big-endian; asset id: fixed DemoSat; set chosen by binding
  set demosat  aaba1c63ceedf56546115fcb909204a46c023548ea92dbcb09d185366637e15d  2 packets, 5 commands

family drone  1ae855cd51e6c6e1c42f03330f733d6e1fb77e8919a16eca2345b22dff3c699f
  binary, little-endian; asset id: metadata sysid; set chosen by binding
  metadata: msgid, sysid
  set drone_v1  9cdb54d367681f74df4f0223c2b3cbbb8cbdd5909ca64839dc2dcfc8d7458617  2 packets, 0 commands

family json_sat  797620de34906b6d4570b7f0fdacdeabf930e9279c3609e1fb57276dc624132c
  json; asset id: path /sat; set chosen by binding
  set json_sat_v1  0121b63b4ecd51ef2d7ad449785df82eab1b0fbf227a7c5a0d0fb864236fa00d  2 packets, 0 commands
```

```console
$ morphc inspect set ccsds_sat_v1 store
set ccsds_sat_v1  6293e3d77620f2f6f26f359bddb24e4c71a7d87e623947aa7d644378d8f365aa
  family ccsds_sat: binary, big-endian
  time: CUC 4+2 bytes at bit 0 from 2000-01-01T00:00:00Z (tai)

packet EPS_HK; exact 13 bytes; identified by apid = 100
  bit  size  entry             type    maps to          notes
  64   16    bus_mv            uint16  eps.bus_voltage  unit mV; invalid 65535
  80   16    batt_temp_counts  uint16  eps.batt_temp    unit Cel; table of 3 points
  96   8     mode              uint8   eps.mode         states SAFE=0, NOMINAL=1, PAYLOAD=2

command SET_MODE; encodes eps.set_mode; metadata apid = 200
  bit  size  entry   type   from  notes
  0    8     opcode  uint8  = 26
  8    8     mode    uint8  mode  states SAFE=0, NOMINAL=1
  16   16    crc     crc16
```

Positions are bits from the packet's start, worked out by the same walk
validation and decoding use; in the store, packets are listed by name.

```console
$ morphc inspect param eps.bus_voltage store
param eps.bus_voltage  f64, V
  Main power bus voltage

sent by
  set ccsds_sat_v1  packet EPS_HK  entry bus_mv  (unit mV)
  set demosat  packet EPS_HK  entry BusVoltage  (unit V; linear scale 0.001 offset 0)
  set drone_v1  packet SYS_STATUS  entry voltage_battery  (unit mV)
  set json_sat_v1  packet PWR_BUS_V  entry v  (unit mV)
```

Four packets in three formats, in millivolts or through a conversion,
under one name: decoding gives volts from each.

```console
$ morphc inspect command eps.set_mode store
command eps.set_mode
  Switch the power mode
  arguments
    mode  enum, states SAFE, NOMINAL
    delay  u64, s, range 0..3600, default 0  (Seconds to wait before switching)

encoded by
  set ccsds_sat_v1  command SET_MODE  (mode)
  set demosat  command SetMode  (mode in Mode, delay in Delay)
```

### 3. Decode a capture

[`fleet/drone.jsonl`](fleet/drone.jsonl) is a drone's link, one JSON record
per message with the metadata MAVLink framing gives it (`msgid`, `sysid`)
and its ground time. [`fleet/bindings.toml`](fleet/bindings.toml) says
which asset each native id is.

```console
$ morphc decode store --family drone --records fleet/drone.jsonl --bindings fleet/bindings.toml
line 3: set drone_v1  packet SYS_STATUS  asset UAV-1 (native id 1)
  def_version 9cdb54d367681f74df4f0223c2b3cbbb8cbdd5909ca64839dc2dcfc8d7458617
  ground time 2026-06-01T12:00:00Z
  param               value  unit  from
  eps.bus_voltage     15.2   V     voltage_battery
  eps.batt_remaining  76.0   %     battery_remaining

line 4: set drone_v1  packet SYSTEM_TIME  asset UAV-1 (native id 1)
  def_version 9cdb54d367681f74df4f0223c2b3cbbb8cbdd5909ca64839dc2dcfc8d7458617
  60000 ms since boot
  no catalog samples

line 5: error[unknown_asset]: native id 2 of family drone is no asset at 2026-06-01T12:00:02Z; bind it in the asset registry

decoded 2 of 3 messages
(exit 1)
```

The third message comes from system 2, which no binding names, so it's
reported and the exit code is 1; the others decode. Without `--bindings`,
morphc stands each native id in for an asset of the same name, which is
handy on a bench and marked as such.

### 4. Send a command

```console
$ morphc encode store --set demosat --command eps.set_mode --arg mode=SAFE --arg delay=30
set demosat  command SetMode  encodes eps.set_mode
  def_version aaba1c63ceedf56546115fcb909204a46c023548ea92dbcb09d185366637e15d
  payload 1064c00000000101001e
```

```console
$ morphc encode store --set ccsds_sat_v1 --command eps.set_mode --arg mode=SAFE
set ccsds_sat_v1  command SET_MODE  encodes eps.set_mode
  def_version 6293e3d77620f2f6f26f359bddb24e4c71a7d87e623947aa7d644378d8f365aa
  payload 1a00f1b7
  metadata apid = 200
  defaulted delay
```

The CCSDS satellite's packet doesn't carry the delay, so it takes its
default; asking that satellite for a delay is refused rather than dropped:

```console
$ morphc encode store --set ccsds_sat_v1 --command eps.set_mode --arg mode=SAFE --arg delay=30
error[not_default]: command eps.set_mode, argument delay: the set doesn't encode it, so it can only be its default, 0
(exit 1)
```

A payload decodes back into the command that made it:

```console
$ morphc decode store --command --set demosat --hex 1064c00000000101001e
set demosat  command SetMode  encodes eps.set_mode
  def_version aaba1c63ceedf56546115fcb909204a46c023548ea92dbcb09d185366637e15d
  mode   SAFE
  delay  30
```

### 5. Change the definitions

Adding a param ([`fleet/solar.toml`](fleet/solar.toml)) changes the catalog
but no set, so building into the store again keeps every set's identity:
data already decoded stays stamped with a `def_version` the store still
serves.

```console
$ morphc build toml/mission xtce/demosat.xml fleet/solar.toml --out store
...
morphc: 5 warnings (3 xtce.ignored, 2 overridden)
catalog           926e73cdc2c3eeac1a6a048bee514098fd4445b8bc5d60a84f7154ed0ff3caee  changed  (written)
family ccsds_sat  74cc510c26ac7222d96e9b55facd66f5a961b36782835ff363a83ac1ac676846  unchanged
family demosat    3bec4e53d0c57acc228cfd6bb6645eb2bd240ff95804513d7706ac860b809597  unchanged
family drone      1ae855cd51e6c6e1c42f03330f733d6e1fb77e8919a16eca2345b22dff3c699f  unchanged
family json_sat   797620de34906b6d4570b7f0fdacdeabf930e9279c3609e1fb57276dc624132c  unchanged
set ccsds_sat_v1  6293e3d77620f2f6f26f359bddb24e4c71a7d87e623947aa7d644378d8f365aa  unchanged
set demosat       aaba1c63ceedf56546115fcb909204a46c023548ea92dbcb09d185366637e15d  unchanged
set drone_v1      9cdb54d367681f74df4f0223c2b3cbbb8cbdd5909ca64839dc2dcfc8d7458617  unchanged
set json_sat_v1   0121b63b4ecd51ef2d7ad449785df82eab1b0fbf227a7c5a0d0fb864236fa00d  unchanged
ok: 18 params, 5 commands, 4 families, 4 sets (MDL IR 2.0); 1 new artifact written to store
```

Catalog names are a schema: removing one would break whatever still uses
it, so a rebuild refuses.

```console
$ morphc build toml/mission xtce/demosat.xml --out store
...
error[removed_param]: eps.solar_current is gone; canonical names are never removed (mark it deprecated instead)
  = catalog, param eps.solar_current

note: the catalog isn't compatible with the one in store, so its sets can't keep their identities; --fresh builds new ones
morphc: 1 error, 5 warnings (3 xtce.ignored, 2 overridden)
(exit 1)
```

`--fresh` builds without the store's previous build: it's how a name is
retired for good. Consumers holding a set built against the older catalog
still find both in the store, since artifacts are never removed. Here the
fresh build is the first build again, whose artifacts the store still has,
so nothing new is written and every set has its first identity back.

```console
$ morphc build toml/mission xtce/demosat.xml --out store --fresh
...
morphc: 5 warnings (3 xtce.ignored, 2 overridden)
catalog           33ee14711221d7edceeec1f978fcf73d0f26e7e7cf7a548665ad13314e6e3e40  changed
family ccsds_sat  74cc510c26ac7222d96e9b55facd66f5a961b36782835ff363a83ac1ac676846  unchanged
family demosat    3bec4e53d0c57acc228cfd6bb6645eb2bd240ff95804513d7706ac860b809597  unchanged
family drone      1ae855cd51e6c6e1c42f03330f733d6e1fb77e8919a16eca2345b22dff3c699f  unchanged
family json_sat   797620de34906b6d4570b7f0fdacdeabf930e9279c3609e1fb57276dc624132c  unchanged
set ccsds_sat_v1  6293e3d77620f2f6f26f359bddb24e4c71a7d87e623947aa7d644378d8f365aa  unchanged
set demosat       aaba1c63ceedf56546115fcb909204a46c023548ea92dbcb09d185366637e15d  unchanged
set drone_v1      9cdb54d367681f74df4f0223c2b3cbbb8cbdd5909ca64839dc2dcfc8d7458617  unchanged
set json_sat_v1   0121b63b4ecd51ef2d7ad449785df82eab1b0fbf227a7c5a0d0fb864236fa00d  unchanged
ok: 17 params, 5 commands, 4 families, 4 sets (MDL IR 2.0); 0 new artifacts written to store
```

### 6. Several vendors' XTCE in one build

On the command line, every XTCE document is part of one import, so DemoSat's
and the imager's packets would share one family and set, and collide.
[`fleet/morphc.toml`](fleet/morphc.toml) lists the inputs instead
([spec/build-files.md](../spec/build-files.md)), each XTCE input an import
of its own with its own options, so the imager joins as a family next to
DemoSat. Its params are new names, a compatible change, so
the fleet's sets keep their identities.

```console
$ morphc build -c fleet/morphc.toml --out store
...
morphc: 6 warnings (4 xtce.ignored, 2 overridden)
catalog           3c23957ef63980c72006ecfd8fcde71339419746d8e3201d1c784d04e0b641d9  changed  (written)
family ccsds_sat  74cc510c26ac7222d96e9b55facd66f5a961b36782835ff363a83ac1ac676846  unchanged
family demosat    3bec4e53d0c57acc228cfd6bb6645eb2bd240ff95804513d7706ac860b809597  unchanged
family drone      1ae855cd51e6c6e1c42f03330f733d6e1fb77e8919a16eca2345b22dff3c699f  unchanged
family imager     1c196f2c7eab2ffbd80957564a3303fbeb2724f5be15c67625abb0b18c59100f  new  (written)
family json_sat   797620de34906b6d4570b7f0fdacdeabf930e9279c3609e1fb57276dc624132c  unchanged
set ccsds_sat_v1  6293e3d77620f2f6f26f359bddb24e4c71a7d87e623947aa7d644378d8f365aa  unchanged
set demosat       aaba1c63ceedf56546115fcb909204a46c023548ea92dbcb09d185366637e15d  unchanged
set drone_v1      9cdb54d367681f74df4f0223c2b3cbbb8cbdd5909ca64839dc2dcfc8d7458617  unchanged
set imager        4ebda139934f13204474433f2d6e5733a8cb088721f779b94392ffb8e5319ca9  new  (written)
set json_sat_v1   0121b63b4ecd51ef2d7ad449785df82eab1b0fbf227a7c5a0d0fb864236fa00d  unchanged
ok: 28 params, 5 commands, 5 families, 5 sets (MDL IR 2.0); 3 new artifacts written to store
```
