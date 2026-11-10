# Build files

Status: normative for `morphc`. The key words MUST, MUST NOT, SHOULD and
MAY are to be interpreted as described in RFC 2119.

A build file lists the definition inputs of one bundle, each with its own
import options, so one build can import several XTCE systems as families
of their own. `morphc` reads the one given with `-c`, or `morphc.toml` in
the working directory (not a parent of it) when a command is given no
inputs.

```toml
version = 1

[[input]]
path = "catalog"

[[input]]
path   = "vendors/demosat.xml"
family = "demosat"
alias  = { kind = "fixed", value = "DemoSat" }

[[input]]
path   = "vendors/imager"
family = "imager"
naming = "qualified"
```

## 1. Format

A build file is a TOML document with these top-level keys and no others:

| Key | Type | |
| --- | --- | --- |
| `version` | integer | The format's version, `1`; 1 when left out. A reader MUST refuse another version, naming it, before it checks the other keys, so a newer format is reported as one. |
| `input` | array of tables | Required, at least one. |

Each `input` table has these keys and no others:

| Key | Type | |
| --- | --- | --- |
| `path` | string | Required. A definition file or directory, as an input on morphc's command line. A relative path is taken from the directory the build file really lives in (after following links to it), so a link to a build file lists the same inputs as the file. Not `-`. |
| `family` | string | The imported family's name (XTCE import option). |
| `set` | string | The imported set's name. |
| `alias` | table | Where an imported asset's own id is, written as a family's `alias` is in the IR: `{ kind = "fixed", value = "..." }`, `{ kind = "metadata", name = "..." }`, `{ kind = "path", pointer = "..." }` or `{ kind = "bits", offset_bits = ..., size_bits = ... }`. |
| `naming` | string | `short` or `qualified`: how names are made from XTCE names. |

Each option left out takes the importer's default ([xtce](xtce.md)): the
family named after the first root SpaceSystem, the set after the family,
a fixed alias of the first root SpaceSystem's name, `short` naming.

A reader MUST reject an unknown key, a missing `path`, and an input with
import options (`family`, `set`, `alias`, `naming`) but no XTCE document
to apply them to.

## 2. Meaning

- The inputs are read as the same paths would be on the command line, in
  order. Within a directory, TOML files come first, then JSON bundles, then
  XTCE documents. A file reached by more than one input is read once, as
  part of the first, with that input's options.
- Every TOML file is read into one bundle, and every JSON file is a bundle
  of its own, as on the command line. Each input's XTCE documents are one
  import, with that input's options: one family and set per input, where
  the command line makes every XTCE document part of one. References
  between XTCE documents resolve within one import, so documents that
  refer to each other are one input (their directory, say).
- The bundles merge in the order of their first file, earlier ones taking
  precedence ([frontends](frontends.md) §5). The TOML bundle's place is
  its first file's, so a TOML input listed after an XTCE or JSON input
  that follows an earlier TOML input still takes precedence over it; morphc
  says so when a build file lists inputs that way.
- A build file holds only what decides the bundle. Where artifacts are
  written, which bindings resolve assets and how results are reported are
  the command's.
- A directory walk leaves out files named `morphc.toml`, so a build file
  kept beside definitions isn't read as one.
