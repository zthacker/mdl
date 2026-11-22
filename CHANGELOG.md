# Changelog

Each release of MDL, newest first. A release states the versions it reads
and writes: the IR, the artifact store's format, and the runtime API's
result format ([spec/compatibility.md](spec/compatibility.md) says what
each covers and what an upgrade can break). `morphc --version` prints
them too.

## [Unreleased]

### Definitions

- XTCE 1.3 documents (`http://www.omg.org/spec/XTCE/20250214`) are read,
  and the spellings of its namespace documents written for Yamcs use,
  with a warning, as for 1.2's. What 1.3 changes that MDL reads:
  - its time units: `milliseconds`, `microseconds`, `nanoseconds`,
    `picoseconds`, `minutes` and `hours` are MDL's `ms`, `us`, `ns`, `ps`,
    `min` and `h`, in any XTCE version; a `units` that's no XTCE time unit
    is now an error, where it was a warning;
  - an alarm marked `disabled` gives no limits;
  - an aggregate argument type's `initialValue`, a JSON object of its
    members, gives each member it names a default beneath the member's
    own initial value; before 1.3 it's still left out, as Yamcs reads
    those documents;
  - a SpaceSystem's `systemType` and `assetType` are left out, with a
    warning;
  - a binary parameter type's context alarms, which 1.3 renames
    `ContextAlarmList`, are left out with a warning under either name, as
    all alarms but a number type's limits are; XTCE 1.0 to 1.2's
    `BinaryContextAlarmList` was refused.
- A reference to an array's element (`Cells[2]`), or into an argument's
  members or elements, which XTCE 1.3 allows, is reported as unsupported
  (`xtce.unsupported`) rather than as naming nothing.
- An XTCE container's `BinaryEncoding`, which was refused, is read:
  - its `SizeInBits` pads the container's entries to that many bits,
    counted from the start of the packet as entry locations are (an
    embedded container's from its own start), in telemetry and command
    containers and in any XTCE version. Yamcs reads a size only for an
    embedded container, so padding any other is an `xtce.differs`
    warning;
  - XTCE 1.3's checks over a packet's bytes (`ErrorDetectCorrect`) that
    name the parameter holding their value (`parameterRef`) make it a
    checksum entry, which decoding verifies and encoding works out: a CRC
    of 8 to 64 bits, `sum8`, or XOR, covering the bytes from
    `bitsFromReference` up to the value. The parameter gives no sample and
    leaves the catalog unless another packet's field maps it. A check
    naming no parameter, as before 1.3 none can, is left out with a
    warning, as it is from a packet that doesn't hold the parameter;
    parity, `sum16` to `sum32` (XTCE doesn't say what they add), the other
    checksums XTCE names, and a check followed in bits by anything but
    another check are unsupported.

### Releases

- The Elixir binding is on Hex as `mdl`, from 0.2.0 on, and each
  release is published there after its GitHub release (RELEASING.md).
  The package downloads the native library the release built for its
  platform, Linux on x86_64 and aarch64, and macOS, so installing it
  needs no Rust; on other platforms the binding is still a git
  dependency on this repository.

## [0.2.0] - 2026-11-21

Command limits for each vendor in a mixed fleet, and a vendor's next
flight software imported from XTCE as another set of its family. It
reads and writes MDL IR 2.3 (and reads 2.0 to 2.2), store format 1 and
result format 1, and builds on Rust 1.88 or later.

### Definitions

- MDL IR 2.3. A command packet can list the states of an enum argument
  it accepts (`states`), for an asset whose flight software lacks some
  of the catalog's modes. The list holds catalog states, in the catalog's
  order, each once, and the default the packet sends must be one of them.
  A listed state the catalog's argument doesn't have is `unknown_state`,
  now a validation code too, with the closest name suggested.
- A command packet can require an argument (`required = true`, IR 2.3),
  for a vendor that needs a value the catalog lets other vendors leave
  out. The catalog's default is then never sent with that packet, and
  isn't checked against it. A new validation code, `required_default`,
  refuses a packet that requires an argument and gives it a default of
  its own.
- A set that uses neither key states the version it did before, and
  keeps its digest.
- A build file's XTCE input can join a family another input defines
  (`join = "vendor_x"`, naming its `set`, with `set_version` for a family
  with a version locator): a vendor's second flight software as another
  set of its family, which bindings or the version locator choose
  between. Its documents define no family, and take the family's byte
  order and alias. A new XTCE code, `xtce.options`, reports a join the
  import can't follow, at the import.
- An XTCE import that refers to a catalog owner's names now reads that
  catalog. An enumerated argument whose labels are some of the catalog
  argument's states gives the states its packet accepts, and an argument
  with no initial value whose catalog argument has a default is required,
  so the default the catalog keeps for other vendors is never sent with
  this vendor's packet (IR 2.3). Limits in the catalog argument's own
  unit no longer write it. A set rebuilt where any of these apply gets a
  new `def_version`, and a request that left such an argument to the
  catalog's default is refused (`missing_argument`) until it gives one.
- A command packet's `unit` that is the argument's own is refused when
  building (`explicit_default`), beside a range or a default too, where
  0.1.0 accepted it. Limits without a unit are in the argument's own
  already, so the two spellings meant the same packet and gave it two
  digests. Delete the line and rebuild: `morphc build` points at each
  one. XTCE imports no longer write it (above). A store 0.1.0 built with
  it still opens: the runtime reads it as the packet without it.

### Decoding and encoding

- A new problem code, `unsupported_state`, for a catalog state the packet
  doesn't accept, whether given to encode or held in a command payload.
  Its message names the states the packet does accept. A name the
  catalog doesn't have is still `unknown_state`, and a definition that
  lists no states gets the same codes as before.
- A required argument left out is `missing_argument`, as one without a
  default is, and its message says the set requires it.

### Compatibility

- Before 1.0, a release may refuse to build a definition only because
  it spells something two ways where the IR means one, naming each such
  rule in its release notes; a store an earlier release built with it
  still opens ([spec/compatibility.md](spec/compatibility.md) §7). The
  packet unit above is the first. From 1.0 on, a build refuses only what
  an earlier release handled wrongly.
- A frontend's output can change from one release to the next, so a set
  rebuilt from unchanged files can get a new `def_version`; the release
  notes say when ([spec/compatibility.md](spec/compatibility.md) §2). The
  XTCE changes above are this release's.
- Upgrade the services that read a store before rebuilding it with this
  release: a rebuilt set that uses IR 2.3 (`states`, `required`) is
  refused by 0.1.0 (`unsupported_version`), and this release opens every
  store 0.1.0 built.

### Tools and the runtime API

- `morphc` reads an XTCE input that refers to the catalog or joins a
  family after the inputs it's read with. One that joins a family no
  input that read defines waits while an input has problems, rather than
  report a family missing that the input with problems may define, and
  one whose catalog doesn't merge waits too; morphc names each, and why.
- Rust API: `mdl_xtce::read_with` gives an import what the rest of a
  build defines, the catalog it refers to and the families it can join.
  A referring import read without a catalog is refused (`xtce.options`).
  `mdl::validate::closest` is public, for a frontend that suggests the
  name meant.

### Releases

- A push to `main` that changes the workspace's version drafts its
  release; publishing the draft creates the tag. Run by hand, the release
  workflow is a dry run of any branch, or, ticked, a release of `main`'s
  head. Pushing a tag no longer starts a release (RELEASING.md).

## [0.1.0] - 2026-11-21

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
- The Elixir binding (`bindings/elixir`): the runtime API for Elixir 1.18
  or later, as a git dependency on this repository, which builds its
  native library with Rust 1.91 or later. Its results are structs with
  atom fields, MDL's own enumerations are atoms, and every name from
  definitions or messages is a string, so reloading stores never grows
  the atom table. Every call that does MDL's work runs on a dirty
  scheduler.

### Releases

- Prebuilt `morphc` for Linux on x86_64 and aarch64 (statically linked)
  and macOS on Apple silicon, with SHA-256 checksums and build provenance
  attestations ([RELEASING.md](RELEASING.md)).
- Precompiled native libraries for the Elixir binding, for Linux on
  x86_64 and aarch64 with glibc 2.28 or later or with musl, and macOS on
  Apple silicon and Intel, each tested with the binding's suite on its
  platform, with a checksum and an attestation. A Hex package that
  downloads them, so that installing needs no Rust, is planned.

[Unreleased]: https://github.com/zthacker/mdl/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/zthacker/mdl/releases/tag/v0.2.0
[0.1.0]: https://github.com/zthacker/mdl/releases/tag/v0.1.0
