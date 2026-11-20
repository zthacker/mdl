# Changelog

Each release of MDL, newest first. A release states the versions it reads
and writes: the IR, the artifact store's format, and the runtime API's
result format ([spec/compatibility.md](spec/compatibility.md) says what
each covers and what an upgrade can break). `morphc --version` prints
them too.

## [Unreleased]

## [0.1.0] - 2026-11-20

The first release. It reads and writes MDL IR 2.2, store format 1 and
result format 1, and builds on Rust 1.88 or later.

### Definitions

- The IR: a catalog of canonical params and commands, families that say
  how a link's payloads are framed and who sent them, and sets that lay
  out each version of a family's packets and command packets. Binary
  payloads at any bit offset in either byte order, and JSON payloads read
  by pointer.
- Frontends: TOML, written by hand; XTCE 1.0 to 1.2, imported as Yamcs
  reads it, with MDL aliases mapping its parameters and meta-commands to
  catalog names; and the IR as JSON, from any tool. Bundles from several
  frontends merge into one.
- Validation that reports every problem at once, located in its source
  file wherever it has one, with the closest catalog name suggested for a
  misspelt one.
- Build files listing a project's inputs, each XTCE input an import of its
  own, and catalog owners that close the catalog to every other input.
- Per-packet argument limits (IR 2.2): a command packet can narrow an
  argument's range and default for its own asset.

### Artifacts and stores

- Content-addressed artifacts: each catalog, family and set named by the
  SHA-256 of its canonical JSON (RFC 8785). A set's digest is the
  `def_version` stamped on what it decodes, and a compatible rebuild keeps
  it.
- Artifact stores that keep every artifact a build ever wrote, written
  crash-safely, with a manifest naming the latest build.

### Decoding and encoding

- Routing by each family's alias and version locators, and bindings files
  that say which asset each native id is, over time.
- Decoding to the raw record, engineering values through linear,
  polynomial, state, table and spline conversions, and canonical samples
  with a quality each; composites (vectors and quaternions with their
  frames); and event time from CUC, GPS week and seconds, elapsed counts,
  ISO 8601 text, or time since boot correlated to absolute time, across
  UTC, TAI, GPS and TT with leap seconds.
- Encoding catalog commands as each set's command packets, with
  arguments checked against their types, states, ranges and lists, and
  checksums.

### Tools and the runtime API

- `morphc`: build, inspect, decode and encode, for people or as one JSON
  document for tools.
- The runtime API every language binding calls ([spec/api.md](spec/api.md)):
  open a store, decode, encode, describe the build, and look up bindings,
  with results whose shape and values are specified, and which of them a
  binding may give its language's own forms.
- The Elixir binding (`bindings/elixir`, Hex package `mdl`): the runtime
  API for Elixir 1.18 or later. Its results are structs with atom fields,
  MDL's own enumerations are atoms, and every name from definitions or
  messages is a string, so reloading stores never grows the atom table.
  Every call that does MDL's work runs on a dirty scheduler. Built from
  source rather than downloaded, it needs Rust 1.91 or later.

### Releases

- Prebuilt `morphc` for Linux on x86_64 and aarch64 (statically linked)
  and macOS on Apple silicon, with SHA-256 checksums and build provenance
  attestations ([RELEASING.md](RELEASING.md)).
- Precompiled native libraries for the Elixir binding, which the Hex
  package downloads, so it needs no Rust: Linux on x86_64 and aarch64 with
  glibc 2.28 or later or with musl, and macOS on Apple silicon and Intel.
  Each is tested with the binding's suite on its platform before release,
  and has a checksum and an attestation too.

[Unreleased]: https://github.com/zthacker/mdl/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/zthacker/mdl/releases/tag/v0.1.0
