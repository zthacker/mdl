# MDL definitions: a worked example (TOML syntax)

These definitions describe three simulated sources from
[morpheus-c2](https://github.com/zthacker/morpheus-c2), the C2 demo MDL was
built for: a CCSDS satellite (big-endian, bit-packed), a JSON-lines satellite
and a MAVLink v2 drone (little-endian). They double as the schema reference
until the MDL spec is written.

These are v1 definitions, which `morphc` no longer reads: the same fleet
in the v2 TOML format is in [`examples/toml/mission`](../toml/mission).
This directory goes when v1 is removed.

## Files

| File | Holds |
| --- | --- |
| `catalog.toml` | the canonical model: every `[[canonical]]` param any set maps to |
| `sets/<set>.toml` | one definition set (one vendor or system); the file name must be the set name |

The catalog is ours, not the vendors'. Vendors publish their own layouts
(ICDs, MAVLink XML, API docs); whoever writes a set reads those and maps each
field to a canonical param with `canonical = "..."`. A field without
`canonical` is still decoded and stored with its packet, just not
normalized; mapping it later is a new plan plus a replay (DDIA Ch. 11).

## What definitions do *not* describe: envelopes

CCSDS primary headers, MAVLink framing and CRCs, ground-station wrappers:
those belong to the **link** the data arrives on. Each link runs a protocol
stack in the consumer (a decoder service) that strips the envelope and hands over a **payload** plus
named **metadata**. Definitions describe the payload, and may refer to
metadata by name.

| Link stack | Metadata it provides | Payload handed over |
| --- | --- | --- |
| `[ccsds]` | `apid`, `seq` | the packet data field (after the 6-byte primary header) |
| `[mavlink2]` | `msgid`, `sysid`, `compid`, `seq` | the payload, zero-extended to full length |
| none (JSON lines) | none | one JSON document |

Offsets are bits from the start of that payload. Whether a link provides the
metadata a set uses is checked when the consumer pairs them, not by `morphc`.

## catalog.toml

```toml
[[canonical]]
name        = "eps.bus_voltage"   # subsystem.name: lowercase, dot-separated
type        = "f64"               # f64 | i64 | bool | str | enum | vec3 | quat
unit        = "V"                 # omit for unitless params
limits      = { red_low = 6.2, yellow_low = 6.6, yellow_high = 8.3, red_high = 8.5 }
description = "Main power bus voltage"

[[canonical]]
name   = "eps.mode"
type   = "enum"
states = ["SAFE", "NOMINAL", "PAYLOAD"]   # enum params only, required

[[canonical]]
name  = "nav.pos_ecef"
type  = "vec3"
unit  = "m"
frame = "ECEF"                    # vec3 and quat only, required
```

- `limits` only on `f64` / `i64`; any of the four may be omitted, and the
  ones present must not decrease from `red_low` to `red_high`.
- Frames: `ECEF`, `ECI_J2000`, `BODY`, `NED`, `GEODETIC` (source side only).
- Canonical names are a schema: never repurpose one; deprecate and add.

## A definition set

```toml
[set]
name     = "vendor_a"             # lowercase, digits, '_' (it ends up in subjects, topics and file names)
encoding = "binary"               # binary | json
endian   = "big"                  # binary only: big | little
time     = { ... }                # optional; see Time
alias    = { ... }                # optional; see Alias

[aliases]                         # vendor-native id → asset id
"101" = "SAT-1"
```

Asset ids follow the `assets` table: a letter or digit, then up to 31 letters,
digits, `_` or `-`. One asset belongs to one set.

### Alias: where the vendor-native asset id is

Exactly one form:

| Form | Use |
| --- | --- |
| `alias = { offset_bits = 48, size_bits = 16 }` | binary: bits in the payload |
| `alias = { metadata = "sysid" }` | binary: a value the link provides |
| `alias = { path = "/sat" }` | json: a JSON Pointer |
| *(no `alias`)* | a single-asset source: `[aliases]` must have exactly one entry |

### Time: where event time comes from

| Form | Use |
| --- | --- |
| *(no `time`)* | ground time only |
| `time = { kind = "cuc", offset_bits = 0, coarse_bytes = 4, fine_bytes = 2, epoch = "2000-01-01T00:00:00Z" }` | binary: CCSDS unsegmented time code (1-4 coarse bytes, 0-3 fine bytes) |
| `time = { kind = "iso8601", path = "/t" }` | json: an ISO 8601 string |
| `time = { kind = "boot_ms", correlate = { packet = "SYSTEM_TIME", unix_us = "time_unix_usec", boot_ms = "time_boot_ms" } }` | ms since boot; each packet names its field with `time_field`; the correlation packet carries both clocks |

## Packets

```toml
[[packet]]
name         = "EPS_HK"
identify     = { apid = 0x064 }   # all must match; see Identify
length_bytes = 15                 # binary only, required: the payload size
time_field   = "time_boot_ms"     # boot_ms time only
```

### Identify

| Set encoding | Keys |
| --- | --- |
| binary | link metadata (`apid`, `msgid`) **or** this packet's own integer field (`frame_type = 7`) |
| json | JSON Pointers: `{ "/ch" = "PWR.BUS.V" }` (quoted, because of the `/`) |

No two packets in a set may identify the same way.

## Fields

```toml
[[packet.field]]
name        = "bus_voltage_counts"
offset_bits = 64                  # binary: from the start of the payload
size_bits   = 16
type        = "uint"              # uint | int | float | bool | string (json only)
conversion  = { linear = { scale = 0.01 } }
unit        = "V"                 # after conversion; must equal the catalog's
canonical   = "eps.bus_voltage"   # optional
invalid     = [65535]             # optional: raw values meaning "no reading" (integers)
```

For json, replace `offset_bits` / `size_bits` with `path = "/v"`.

Bit numbering: bit 0 is the most significant bit of payload byte 0, counting
on through the bytes (as CCSDS does), so byte 14's low two bits are bits
118-119. `endian` decides how the bytes of a multi-byte field combine. Little-endian
fields (and a little-endian alias) must be whole bytes on byte boundaries, as
MAVLink's are, or fit inside one byte; `morphc` rejects anything else.

Sizes: `uint`/`int` 1-64 bits, `float` 32 or 64, `bool` 1. Fields may not
overlap or run past `length_bytes`.

### Conversions

| Form | Meaning |
| --- | --- |
| `{ linear = { scale = 0.01, offset = 0.0 } }` | `raw * scale + offset` (both optional: 1.0 and 0.0) |
| `{ poly = [0.0, 0.01, 1e-6] }` | `c0 + c1*x + c2*x^2 ...` |
| `{ states = { 0 = "SAFE", 1 = "NOMINAL" } }` | integer to state name (enum params) |

### Mapping to the catalog

| Catalog type | The field must be |
| --- | --- |
| `f64` | `uint`, `int` or `float`, with or without linear/poly |
| `i64` | `uint` or `int`, no conversion |
| `bool` | `bool`, no conversion |
| `str` | `string`, no conversion |
| `enum` | a `states` conversion whose names are all in the catalog's `states`, or a `string` field (no conversion) that already carries the state names |
| `vec3` / `quat` | not a single field: use a composite |

## Composites: several fields → one vec3 or quat

```toml
[[packet.composite]]
canonical = "nav.pos_ecef"
frame     = "ECI_J2000"           # the source's frame; consumers convert
fields    = ["pos_x", "pos_y", "pos_z"]   # vec3: x, y, z; quat: w, x, y, z
```

Members must be numeric fields of the same packet, each with the catalog's
unit, except a `GEODETIC` position: `[lat, lon, alt]` in `deg`, `deg`, then
the catalog's unit. Frames consumers are expected to convert: any frame to itself, and `ECI_J2000`,
`GEODETIC` or `NED` to `ECEF`.

## The sources in this example

The byte layouts are in morpheus-c2's `docs/SPEC.md` ("Simulated sources")
and its simulator (`services/sim/src/vendor_*.rs`).
