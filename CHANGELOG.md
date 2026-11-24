# Changelog

Each release of MDL, newest first. A release states the versions it reads
and writes: the IR, the artifact store's format, and the runtime API's
result format ([spec/compatibility.md](spec/compatibility.md) says what
each covers and what an upgrade can break). `morphc --version` prints
them too.

## [Unreleased]

### Definitions

- MDL IR 2.4: a telemetry field or scalar array element that maps a param
  through a conversion that doesn't invert exactly (a polynomial, a
  spline, a step table, a table whose values rise and fall) may have an
  `inverse`, engineering value to raw, as a command packet's field has
  since IR 2.0, for encoding telemetry
  ([spec/frontends.md](spec/frontends.md) §7). One on any other telemetry
  entry is refused. A set that uses one states 2.4; every other artifact
  keeps its version and its digest.

### Decoding and encoding

- Telemetry encodes: values for one of a set's packets become the payload,
  and the metadata a link frames it with, that decoding reads back as
  those values, sent by a given native id: what a simulated asset does to
  send telemetry ([spec/encoding.md](spec/encoding.md) §9).
  - Params' values go through their fields' inverse conversions, units,
    states and composites, a param the message holds several times taking
    its values in record order; raw values give entries as decoding's
    record holds them, and an entry without a value is refused
    (`missing_value`).
  - The family's locators, the packet's identify keys, counts, lengths and
    checksums come from the definitions, and a value given for one of
    them must agree (`wrong_constant`).
  - The payload is decoded before it's returned, and refused if decoding
    gives anything else, such as another packet whose identify keys its
    values match too (`decodes_otherwise`).
  - New codes: `unknown_packet`, `native_id`, `unknown_param`,
    `absent_entry`, `unknown_entry`, `duplicate_value`, `no_inverse`,
    `invalid_raw` and `decodes_otherwise`.

  A set whose time source reads the packet's time where no entry holds it
  (a CUC time code, or an ISO 8601 pointer no top-level field has) can't
  be encoded from these inputs.

### Compatibility

- A set that uses a telemetry inverse states IR 2.4, which 0.3.0 doesn't
  read (`unsupported_version`), so a store with one needs this release.
  The result format is still 1: encoding telemetry is a new operation.

### Tools and the runtime API

- The runtime API gains `encode_telemetry` ([spec/api.md](spec/api.md)
  §2.9) and its result, an encoded telemetry packet (§3.6). In Rust,
  `FamilySets::encode_telemetry` and `Runtime::encode_telemetry`.
- `morphc encode --packet NAME` encodes a telemetry packet, from `--value
  PARAM=VALUE`, `--raw ENTRY=VALUE` and the sender `--from ID`, and
  prints its payload, metadata and `def_version`. Its JSON report gives it
  under `telemetry` and stays version 3.

## [0.3.0] - 2026-11-22

For the Elixir binding's users, command payloads decode through the
runtime API, and boot time turns into instants. Conformance suites give
any binding the spec's results to check itself against, and XTCE imports
gain 1.3, container checks and sizes, and composites. It reads and
writes MDL IR 2.3 (and reads 2.0 to 2.2), store format 1 and result
format 1, so a store an earlier release built still opens; morphc's JSON
report is version 3. It builds on Rust 1.88 or later.

### Definitions

- An XTCE aggregate or array parameter can be a composite, one vector
  sample rather than one sample per member or element
  ([spec/xtce.md](spec/xtce.md) §4.1). Ancillary data in the MDL
  namespace says so, which Yamcs leaves out:
  - `MDL.frame`, on the parameter or its type, makes three numeric
    members a vec3 and four a quat in that frame, an array's with a
    fixed count;
  - `MDL.members` names the members in MDL's order, for a quaternion sent
    scalar last: an aggregate's by name, an array's by the indices XTCE
    numbers its elements with.

  An import that owns its catalog defines the vec3 or quat param, in the
  composite's frame and its members' unit; a GEODETIC composite, or one
  whose members are in different units, needs a referring import whose
  catalog defines it. A marked parameter in a repeated entry, an
  embedded container or an array's elements is unsupported. Unmarked
  aggregates and arrays import as before.

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
- A command packet that gives its own limits (IR 2.2, 2.3) for an
  argument named `kind` was refused when read, from JSON or TOML: the
  reader took the argument's name for a tagged object's `kind`. It reads
  now, and a store holding one opens.
- A checksum covers at least one byte in every message: its `from_bits`
  (0 when not given) is below the earliest bit the entries before it let
  it start at. 0.2.0 refused one only when its own `offset_bits` came
  before `from_bits`. A build now refuses (`checksum`) any other that can
  start at or before `from_bits`, in any frontend: in a message where it
  started at `from_bits` it covered no bytes, and in one where it started
  before, decoding and encoding gave `invalid_layout`.

### Decoding and encoding

- Opened without bindings, a family with several sets and no version
  locator refused every message as `missing_set` before reading it. Its
  messages now route as any other's, in the order `decoding.md` §3.3
  gives: a payload that isn't valid JSON is `invalid_json` and an alias
  that can't be read is `unreadable_locator`, and the rest are still
  `missing_set`. Looking up such a family's binding or asset without
  bindings is still `missing_set`.
- The specification now gives the order of a command payload's checks,
  as MDL has always run them, so the first failure is the same error in
  every implementation: the set, the payload's encoding, identification,
  every entry read, then the constants in entry order, then the arguments
  in the catalog's order, wherever the packet writes them
  ([spec/encoding.md](spec/encoding.md) §7.5).
- A native id is never empty, as a bindings file already required, and
  empty text names no asset ([spec/api.md](spec/api.md) §1):
  - an alias that reads it, at a `metadata` name or a `path` pointer, is
    `unreadable_locator`, where without bindings it stood for an asset of
    its own and with them was `unknown_asset`;
  - a command given it as its target is `missing_target`, where a string
    target field wrote empty text and an integer one gave `target_type`;
  - `binding` and `asset` find no asset for it, with bindings or
    without, where without bindings they gave a stand-in.

  A decoded command still reports a target field's empty text as the
  payload holds it.

### Compatibility

- The checksum rule above is the second a build refuses before 1.0 while
  a store an earlier release built with it still opens. The store reads
  as it did: in a message where such a checksum covers no bytes, it's
  compared with what its algorithm gives for none, and where it starts
  before `from_bits`, the message is `invalid_layout`. Such a rule now
  covers a definition that can fail to do what it's for in a message it
  allows, as well as a second spelling of one meaning
  ([spec/compatibility.md](spec/compatibility.md) §7). Rebuilding a set
  with one fails at the checksum, located in its source file: give it a
  `from_bits` before its earliest start, or put an entry before it.
- A new key in a morphc JSON report keeps the report's version, as a new
  key keeps the result format's
  ([spec/compatibility.md](spec/compatibility.md) §4).
- The empty native id rule above refuses at runtime what 0.2.0 accepted:
  a command to an empty target, which encoded, and, without bindings, a
  message whose alias reads empty text, which decoded as a stand-in, and
  `binding` and `asset` for empty text, which gave a stand-in and now
  give none. With bindings such a message was already refused, as
  `unknown_asset`, and its code is now `unreadable_locator`.

### Tools and the runtime API

- Decoding cases, in `conformance/decoding/`: language-neutral JSON files
  of definitions, bindings, messages and the results the spec gives for
  them, each worked out by hand and citing the sections it follows, for
  values, layouts, checksums, routing, bindings, JSON payloads,
  conversions and qualities, composites and event time, with the warnings
  opening with the bindings gives; and 377 more, in
  `conformance/decoding/oracle/`, generated from random binary and JSON
  layouts by the testkit's oracle, damaged messages among them and
  layouts with boot time whose correlation packets teach a boot instant
  or nothing, each with
  the result or error the oracle works out from the spec, in code
  separate from MDL's decoder ([spec/conformance.md](spec/conformance.md)
  §2.3 says what that does and doesn't check; `just conformance` writes
  them). Encoding cases, in `conformance/encoding/`: requests, each
  argument's value tagged with its kind, and the encoded command or error
  the spec gives, worked out by hand, for argument types and resolution,
  ranges and lists, a packet's own limits, raw values, binary and JSON
  payloads; and 262 more, in `conformance/encoding/oracle/`, generated
  from random command layouts by the testkit's oracle, refused requests
  among them. A binding of the runtime API must pass both through its own
  API ([spec/conformance.md](spec/conformance.md), a new specification);
  the runtime and the Elixir binding run them in their tests.
- The runtime decodes command payloads: `decode_command` takes a family,
  the set that decodes the payload and the message, its payload and
  metadata, and gives the catalog command and every argument's value, the
  arguments the set leaves to their defaults, the target and the
  payload's record, or the error that stopped it
  ([spec/api.md](spec/api.md) §2.7, §3.5). It's what a simulated asset
  does with a command it receives, and what command history records. In
  Elixir, `MDL.decode_command/3` gives an `%MDL.DecodedCommand{}`. The
  result format is still 1: the operation and its result are new.
- Command decoding cases, in `conformance/decoding_commands/`: payloads
  with their metadata, and the command or error the spec gives for them,
  worked out by hand, for which packet a payload and its metadata are,
  constants, arguments through their conversions and the catalog's
  limits, defaults, targets, checksums, lists, binary and JSON payloads,
  and the order of the checks; and 263 more, in
  `conformance/decoding_commands/oracle/`, from the encoding cases'
  generated layouts, each request's payload decoded back and garbled
  payloads among them ([spec/conformance.md](spec/conformance.md) §4). A
  binding must pass them too; the runtime and the Elixir binding run
  them in their tests.
- `morphc decode --command --format json` writes each command as the
  runtime API's `decode_command` gives it, so the document's format
  version is 3: a command gained `family` and `record`, its `arguments`
  are an object of each argument's value, by name, where they were a list
  of names, values and `defaulted` flags, `defaulted` lists the arguments
  that took their defaults, and an integer `target` is written as
  integers are, where it was always text. The other commands' results are
  unchanged, so a consumer of them need only accept version 3. What
  morphc prints for people is unchanged.
- A decoded message gains `booted`: the instant its asset booted, which
  the message teaches as its set's correlation packet, when both clocks
  read good and the instant is within years 0001 to 9999, and absent
  otherwise ([spec/api.md](spec/api.md) §3.1). Correlating boot time
  takes state across messages, which the runtime doesn't keep: a
  consumer keeps the latest `booted` per family and asset, a message's
  own included, in the order messages arrive, and a `since_boot` event
  time's instant is that plus its `value` in its `unit`
  ([spec/decoding.md](spec/decoding.md) §8.2). The result
  format is still 1: the key is new. In Elixir, `%MDL.Decoded{}` gains
  `:booted`. morphc's JSON report gains it too and stays version 3, and
  morphc prints the boot instant a correlation packet teaches. In Rust,
  `mdl::decode::boot_instant` gives it for a decoded message, and the
  `Correlator` keeps it. Every decoding case's result has the key, and
  new hand-written cases correlate on the UTC and TAI scales, across a
  leap second, past year 9999 and before year 0001, and with a clock that
  holds its sentinel; the generated cases gain binary and JSON layouts
  that always have boot time.
- A new runtime operation, `instant`, turns an event time into an
  instant, from the boot instant the caller keeps for its asset: an `at`
  time's own, a `since_boot` time's after the boot instant, counted
  exactly, within years 0001 to 9999, and absent for a ground time, for
  time since boot without a boot instant, and past that range
  ([spec/api.md](spec/api.md) §2.8). It's pure and takes no handle. In
  Rust it's `mdl::runtime::instant`, which the `Correlator` now uses; in
  Elixir, `MDL.instant/2`, which takes any instant MDL gives, as an
  integer of any size. Instant cases, in `conformance/instants/`, give
  event times and boot instants with the instant the spec gives, worked
  out by hand, for each kind and unit, the ends of the range, and counts
  past 2^53 and past what 64 bits of nanoseconds hold
  ([spec/conformance.md](spec/conformance.md) §5); a binding must pass
  them too.
- The specification now says what the runtime has always given for a
  composite sample that isn't `good`: each member is its raw value as a
  float, unconverted, so a composite's value is always its frame and
  floats ([spec/decoding.md](spec/decoding.md) §7.4).

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

[Unreleased]: https://github.com/zthacker/mdl/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/zthacker/mdl/releases/tag/v0.3.0
[0.2.0]: https://github.com/zthacker/mdl/releases/tag/v0.2.0
[0.1.0]: https://github.com/zthacker/mdl/releases/tag/v0.1.0
