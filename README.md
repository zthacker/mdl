# MDL: the Morpheus Definition Language

MDL describes what the bytes from a heterogeneous fleet mean, and turns
them into one canonical model. Definitions say where each value sits in
each vendor's messages, how to convert it, and which canonical name it
maps to; MDL validates them against your catalog, builds them into
content-addressed artifacts, and decodes telemetry and encodes commands
with them.

- **Any source.** Write definitions in TOML, import a vendor's XTCE (MDL
  reads OMG XTCE 1.0 to 1.2 space system definitions, checked against
  Yamcs's reading of them), or emit the IR as JSON from any tool, in any
  language. Bundles from several frontends merge into one, with a curated
  catalog taking precedence.
- **Canonical by design.** Every mapped value is checked against the
  catalog for type, unit and frame, so four vendors' bus voltages all
  arrive as `eps.bus_voltage` in volts, and one `eps.set_mode` command
  reaches every spacecraft that can switch modes.
- **Protocol-free.** Envelopes (CCSDS headers, MAVLink framing) belong to
  the link. MDL describes payloads: binary at any bit offset in either byte
  order, or JSON.
- **Identities you can keep.** Each artifact is named by the SHA-256 of
  its canonical JSON. A set's digest is the `def_version` stamped on what
  it decodes, and rebuilding after a compatible change keeps it.
- **Every problem at once.** Validation reports every error, with its file,
  line and column wherever it has one, in TOML and XTCE alike.

> Status: pre-0.1, under active development. The IR, APIs and CLI will
> change.

## Quick start

The crates aren't on crates.io yet, so morphc is installed from a clone:

```sh
cargo install --path crates/morphc
cd examples

morphc build toml/mission xtce/demosat.xml --out store    # definitions to artifacts
morphc build -c fleet/morphc.toml --out store              # or a build file listing them
morphc inspect param eps.bus_voltage store                 # who sends it, and how
morphc encode store --set demosat --command eps.set_mode --arg mode=SAFE

# A drone's capture; one message comes from a system no binding names,
# so it's reported and the exit code is 1.
morphc decode store --family drone --records fleet/drone.jsonl --bindings fleet/bindings.toml
```

[`examples/README.md`](examples/README.md) walks through these and more,
with their output; [`crates/morphc`](crates/morphc/README.md) is morphc's
reference. `just check` runs fmt, clippy and every test; it needs Rust 1.88
or later and [`just`](https://github.com/casey/just).

## Concepts

| Term | Meaning |
| --- | --- |
| Catalog | The canonical params and commands every source maps to: names, types, units, states, frames, limits. Names are a schema: deprecated, never removed. |
| Family | One kind of source on one link: its encoding, byte order, where each message says which asset sent it (the alias) and, optionally, which layout version it uses. |
| Set | One layout version of a family: its packets, struct types and command packets, and how each entry maps to the catalog. |
| Bundle | A catalog with families and sets: what frontends produce and validation checks. |
| Artifact | A catalog, family or set written as canonical JSON, named by its digest ([spec](spec/canonical-json.md)). |
| Store | A directory of artifacts and a manifest naming the latest build ([spec](spec/store.md)). |
| Bindings | Which asset each native id is over time, and which set decodes it ([spec](spec/bindings.md)). |

## The library

morphc is a thin layer over the `mdl` crate, which any ground software can
use directly: read definitions, validate and build them, then decode with
the artifacts as a consumer loads them. The packages are named
`morpheus-mdl`, `morpheus-mdl-toml`, `morpheus-mdl-xtce` and
`morpheus-morphc`, since `mdl` and `morphc` are taken on crates.io; code
imports them as `mdl`, `mdl_toml` and `mdl_xtce`, and the binary is
`morphc`.

```rust
use mdl::artifact::{self, Written};
use mdl::bindings::Bindings;
use mdl::decode::SampleValue;
use mdl::message::{Message, Payload};
use mdl::read::read_artifact;
use mdl::route::FamilySets;
use mdl::source::Sources;
use mdl::validate::validate;

// A beacon: a node id, then the bus voltage in millivolts.
let definitions = r#"
mdl_version = "2.0"

[catalog.params."eps.bus_voltage"]
type = "f64"
unit = "V"

[[families]]
name       = "beacon"
encoding   = "binary"
byte_order = "big"
alias      = { kind = "bits", offset_bits = 0, size_bits = 8 }

[[sets]]
name   = "beacon_v1"
family = "beacon"

[[sets.packets]]
name = "BEACON"

[[sets.packets.entries]]
kind        = "field"
name        = "bus_mv"
type        = "uint"
offset_bits = 8
size_bits   = 16
unit        = "mV"
canonical   = "eps.bus_voltage"
"#;
let mut sources = Sources::new();
sources.add("beacon.toml", definitions);
let parsed = mdl_toml::read(sources.iter()).expect("the definitions read");
let validated = validate(parsed.bundle).expect("the definitions are valid");
let artifacts = artifact::build(&validated).expect("they build");

// A consumer reads the artifacts back (from a store, say) and decodes.
let load = |written: &Written| read_artifact(written.canonical.as_bytes()).expect("it reads");
let beacons = FamilySets::new(
    &load(&artifacts.families["beacon"]),
    &[load(&artifacts.sets["beacon_v1"])],
    &[load(&artifacts.catalog)],
)
.expect("the family assembles");
let bindings = Bindings::from_toml(
    r#"
mdl_version = "2.0"

[[binding]]
family         = "beacon"
native_id      = 7
asset          = "NODE-7"
set            = "beacon_v1"
effective_from = "2026-01-01T00:00:00Z"
"#,
)
.expect("the bindings read");

let received = "2026-06-01T12:00:00Z".parse().expect("an instant");
let message = Message::new(Payload::Binary(&[0x07, 0x6E, 0xFA]), received);
let decoded = beacons.decode(&message, &bindings).expect("it decodes");
assert_eq!(decoded.route.binding.asset, "NODE-7");
assert_eq!(decoded.samples[0].param, "eps.bus_voltage");
assert_eq!(decoded.samples[0].value, SampleValue::F64(28.41));
```

Encoding is the same shape: `FamilySets::encode` with a `CommandRequest`.
This example runs as a test of the TOML crate.

## Contributing

`just check` runs what CI runs: formatting, clippy, every test (including
the walkthrough and the README's example) and the docs. Issues and pull
requests are welcome at <https://github.com/zthacker/mdl>.
[`ROADMAP.md`](ROADMAP.md) says what's next, such as build files for
several imports.

## Layout

| Path | What |
| --- | --- |
| `crates/mdl` | the library: the IR, reading and validating it, artifacts, decoding, encoding, routing, bindings, layouts |
| `crates/mdl-toml` | the TOML frontend: definitions written as the IR spelled in TOML |
| `crates/mdl-xtce` | the XTCE importer: OMG XTCE space system definitions as bundles |
| `crates/morphc` | the command line: build, inspect, decode, encode |
| `examples` | a fleet in TOML and XTCE, the files an operator adds, and a walkthrough |
| `spec` | normative specifications: [canonical JSON and artifact identity](spec/canonical-json.md), [frontends](spec/frontends.md), [TOML](spec/toml.md), [XTCE](spec/xtce.md), [decoding](spec/decoding.md), [encoding](spec/encoding.md), [units](spec/units.md), [bindings files](spec/bindings.md), [artifact stores](spec/store.md), [build files](spec/build-files.md) |

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this project by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
