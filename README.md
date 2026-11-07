# MDL: the Morpheus Definition Language

MDL describes what the bytes from a heterogeneous fleet mean. You write
definitions for each source: where each field sits, how to convert it, and
which canonical name it maps to. MDL validates them against your canonical
catalog and compiles them into a content-addressed plan. A small interpreter
then applies the plan to raw messages.

- **Format-agnostic.** Definitions are written in TOML today. Any tool, in
  any language, can produce the MDL IR as JSON instead.
- **Protocol-free.** Envelopes (CCSDS headers, MAVLink framing, CRCs) belong
  to the link. MDL describes the payload, which can be binary at arbitrary
  bit offsets, either endianness, or JSON.
- **Canonical by design.** Every mapped field is checked against your
  catalog at compile time, for type, unit and frame, so three vendors'
  battery voltages all arrive as `eps.bus_voltage` in volts.
- **Every problem at once.** The compiler reports every error with its file
  and location, not just the first one.

> Status: pre-0.1, under active development. The IR, APIs and CLI will
> change.

## Quick start

```sh
just morphc build examples/definitions
# ok: 3 sets, 14 packets, 53 fields, 17 canonical params (MDL IR v1)

just check    # fmt, clippy, tests
```

Requires Rust 1.88 or later and [`just`](https://github.com/casey/just).

## Layout

| Path | What |
| --- | --- |
| `crates/mdl` | the library: IR, frontends, validation, plans, interpreter |
| `crates/morphc` | the compiler CLI |
| `examples/definitions` | a worked example (a CCSDS satellite, a JSON satellite, a MAVLink drone) and, for now, the schema reference |
| `spec` | normative specifications: [canonical JSON and artifact identity](spec/canonical-json.md), [bindings files](spec/bindings.md), [decoding](spec/decoding.md), [encoding](spec/encoding.md), [units](spec/units.md), [frontends](spec/frontends.md) |

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this project by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
