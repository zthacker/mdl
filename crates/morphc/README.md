# morphc

The MDL compiler: definitions from any frontend into content-addressed
artifacts, and messages through them. [`examples/README.md`](../../examples/README.md)
walks through every command on a small fleet; this page is the reference.

```sh
cargo install --path crates/morphc    # or: just morphc <command> ...
```

| Command | Does |
| --- | --- |
| `morphc build INPUT... [--out STORE]` | Read, merge and validate definitions, and write their artifacts to a store. |
| `morphc inspect INPUT...` | Every family and set with its identity; `inspect set NAME`, `inspect param NAME`, `inspect command NAME` for one of them. |
| `morphc decode INPUT... --hex/--payload/--lines/--records` | Decode messages into catalog samples, or command payloads into arguments. |
| `morphc encode INPUT... --command NAME --arg NAME=VALUE` | Encode a command as a set's command packet. |

Exit codes: 0 done, 1 the definitions have problems (or a message didn't
decode, a command didn't encode), 2 morphc couldn't run.

## Inputs

Every command reads the same inputs: `.toml` (MDL TOML), `.xml` (XTCE)
and `.json` (an IR bundle from any tool) files and directories, or `-` for
a bundle on stdin. They merge into one bundle, then validate, and every
problem is reported with its file and position.

- Earlier inputs take precedence on descriptions, deprecation notes and
  limits ([frontends](../../spec/frontends.md) §5). Within a directory,
  TOML files come first, then JSON bundles, then XTCE documents, so a
  hand-written catalog wins over a vendor's import whatever the files are
  called.
- An XTCE import's family, set and alias are set with `--import-family`,
  `--import-set` and `--import-alias` (`fixed:<id>`, `metadata:<name>`,
  `path:<pointer>`, `bits:<offset>:<size>`), its naming with
  `--xtce-naming`. On the command line every XTCE document is part of one
  import.
- A build file ([spec](../../spec/build-files.md)) lists the inputs in
  their place, each XTCE input an import of its own with its own options:
  `-c FILE`, or `morphc.toml` in the working directory when no inputs are
  given. Directory walks leave build files out.
- `inspect`, `decode` and `encode` also take one store, whose latest build
  they use.
- Warnings of one kind are shown once with a count; `--all-warnings` shows
  them all.

## build

With `--out`, each artifact is written to a [store](../../spec/store.md)
under its digest, with a manifest naming the build. Building into a store
again is a rebuild: sets that haven't changed keep their identity, and a
catalog change that would break them is refused. `--fresh` builds without
the store's previous build; `--previous STORE` continues another store's.

## inspect

Nothing is decoded. The overview names every family and set with its
digest; `inspect set NAME` is a datasheet of each packet, command packet and
struct type: where each entry starts, its size, type and mapping. Positions
come from the library's layout API, so they're the ones validation and
decoding use; a start marked `*` is the same in every message without being
a static position (decoding spec §5.2). `inspect param NAME` lists every
entry that sends a catalog param, `inspect command NAME` every command
packet that encodes a command. Views come before the inputs.

Definitions show the identities a fresh build gives them; a store shows the
ones its rebuilds kept. Either way a set is shown as its artifact holds it,
packets in name order. Entries are named by path through struct entries and
arrays (`pos.x`, `cells[].v`).

## decode

Messages are given as `--hex`, `--payload FILE`, `--lines FILE` (hex lines
for a binary family, JSON lines for a JSON one) or `--records FILE`, a
capture whose lines are JSON records with each message's own metadata and
ground time:

```json
{"hex": "000000000000002a0bb8", "meta": {"apid": 100, "fmt": 2}, "time": "2026-06-01T12:00:00Z"}
{"text": "{\"sat\":\"PX-7\",\"v\":212}", "time": "2026-06-01T12:00:31Z"}
```

A JSON family's payload is `text`, the document exactly as sent, or `json`,
the document as a value for records written by hand (written again, so its
key order and number spelling are serde_json's, and it can't hold a
duplicate key). `--meta NAME=VALUE` and `--time` give what the link
provides, or what a record leaves out. Blank lines and lines starting with
`#` are skipped.

`--family` names the family when the build has several. With `--bindings`,
each native id resolves to its asset and set as in operations; without,
decoding is a bench test: each native id stands in for an asset of the same
name (marked `stand_in` and "no bindings"), decoded by the family's version
locator, `--set` or its only set. Every message's samples, or the error
that stopped it, are reported. `--command` decodes command payloads into
their arguments with `--set`; `--raw` adds each entry's raw and engineering
value. Results are printed when every message is done.

## encode

`--arg NAME=VALUE` is read as the catalog types the argument: a number,
`true` or `false`, a state's name, text, bytes in hex, or a list as a JSON
array, whose elements may be written as decode writes them (large integers
as decimal text). Arguments left out take their defaults. `--set` picks the
set when more than one encodes the command; `--target` gives the asset's
native id, for a packet that carries one. The output is the payload (hex,
or the text to send for a JSON family), the metadata its link frames it
with, the `def_version` to record, and the arguments that took their
defaults. A command that can't be encoded is reported with the library's
code; a deprecated one encodes with a warning.

## JSON output

`--format json` prints one document on stdout, whatever the outcome, except
for a command line clap can't parse (an unknown flag, a missing argument),
which prints its usage on stderr and exits 2:

| Key | Holds |
| --- | --- |
| `report` | The document's format version, 1. |
| `ok` | Whether everything succeeded. |
| `error` | Why morphc couldn't run (exit code 2); null otherwise. |
| `diagnostics` | Each with `severity`, `code`, `message`, `context`, `path` (a JSON Pointer into the bundle, or into the file `context` names, such as a bindings file), and `location` and `first` as `file`, byte offsets `start` and `end`, and lines and columns from 1. |
| `notes` | What to do next, for people. |
| `build`, `inspect`, `decode` or `encode` | The command's result: the artifacts with their digests; a view tagged by `view`, with IR values as the IR writes them and a set's layout; each message with its line and its telemetry, command or error; the packet or the error. Null when the definitions have problems, and left out when morphc couldn't run. |

Values: integers past ±(2^53 - 1) are decimal text, non-finite floats
`{"non_finite": "NaN"}`, bytes hex, and a kind of value this morphc
doesn't know `{"unsupported": ...}`. Codes that aren't the library's start
with `morphc.`.
