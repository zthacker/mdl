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
just morphc build examples/toml/mission
# catalog           16d971cf...
# family ccsds_sat  74cc510c...
# ...
# ok: 4 params, 1 command, 3 families, 3 sets (MDL IR 2.0)

# A TOML catalog beside a vendor's XTCE, written to an artifact store
just morphc build catalog.toml vendor.xml --out store/

# What a build holds: a set's datasheet, who sends a param
just morphc inspect set ccsds_sat_v1 examples/toml/mission
just morphc inspect param eps.bus_voltage store/

just check    # fmt, clippy, tests
```

`morphc build` reads `.toml` (MDL TOML), `.xml` (XTCE) and `.json` (an IR
bundle from any tool) files and directories, merges them into one bundle
and validates it, reporting every problem. Earlier inputs take precedence
on descriptions and limits ([frontends](spec/frontends.md) §5); within a
directory, TOML files come first, then JSON bundles, then XTCE documents,
so a hand-written catalog wins over a vendor's import whatever the files
are called. With `--out` it writes each
artifact to a [store](spec/store.md) under its digest, with a manifest
naming the build. Building into a store again is a rebuild: sets that
haven't changed keep their identity, and a catalog change that would break
them is refused unless you pass `--fresh`. Exit codes: 0 built, 1 the definitions
have problems, 2 morphc couldn't run.

`morphc inspect` reads the same inputs, or one store's latest build, and
shows what they hold without decoding anything: every family and set with
its identity (`morphc inspect store/`), one set's packets with where each
entry sits and what it maps to (`inspect set NAME`), every entry that sends
a catalog param (`inspect param NAME`) or encodes a command (`inspect
command NAME`). Positions come from the library's layout API, so they're
the ones validation and decoding use; a start marked `*` is the same in
every message without being a static position (decoding spec §5.2).
Definitions show the identities a fresh build gives them; a store shows the
ones its rebuilds kept. Either way a set's content is shown as its artifact
holds it, packets in name order. Entries are named by path through struct
entries and arrays (`pos.x`, `cells[].v`; MDL names hold no dots or
brackets).

`--format json` prints one JSON document on stdout for tools, whatever the
outcome: `report` (its format version, 1), `ok`, `error` (why morphc
couldn't run, exit code 2; null otherwise), `diagnostics` (each with `severity`, `code`,
`message`, `context`, `path`, and `location` and `first` as `file`, byte
offsets `start` and `end`, and lines and columns from 1), `notes`, and
the command's own result: `build` (the artifacts with their digests, and
what was written) or `inspect` (tagged by `view`, carrying IR values as
the IR writes them, and a set's layout).

Requires Rust 1.88 or later and [`just`](https://github.com/casey/just).

## Layout

| Path | What |
| --- | --- |
| `crates/mdl` | the library: IR, frontends, validation, plans, interpreter |
| `crates/mdl-toml` | the TOML frontend: definitions written as the IR spelled in TOML |
| `crates/mdl-xtce` | the XTCE importer: OMG XTCE space system definitions as bundles |
| `crates/morphc` | the compiler CLI: any frontends to artifacts |
| `examples/definitions` | a worked example (a CCSDS satellite, a JSON satellite, a MAVLink drone) and, for now, the schema reference |
| `examples/toml` | the same kind of fleet, and mixed versions on one link, in the v2 TOML format |
| `examples/xtce` | a CCSDS satellite described in XTCE, imported and decoded |
| `spec` | normative specifications: [canonical JSON and artifact identity](spec/canonical-json.md), [bindings files](spec/bindings.md), [decoding](spec/decoding.md), [encoding](spec/encoding.md), [units](spec/units.md), [frontends](spec/frontends.md), [artifact stores](spec/store.md), [TOML](spec/toml.md), [XTCE](spec/xtce.md) |

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this project by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
