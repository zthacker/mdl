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
path    = "catalog"
catalog = "owner"

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
| `alias` | table | Where an imported asset's own id is, written as a family's `alias` is in the IR: `{ kind = "fixed", value = "..." }`, `{ kind = "metadata", name = "..." }`, `{ kind = "path", pointer = "..." }` or `{ kind = "bits", offset_bits = ..., size_bits = ... }`. A `metadata` alias also declares its name in the family's `metadata`. |
| `naming` | string | `short` or `qualified`: how names are made from XTCE names. |
| `catalog` | string | `owner`: the input's definitions are the build's catalog (§3). Not an import option: any input may own the catalog. |

Each option left out takes the importer's default ([xtce](xtce.md)): the
family named after the first root SpaceSystem, the set after the family,
a fixed alias of the first root SpaceSystem's name, `short` naming.

A reader MUST reject an unknown key, a missing `path`, a `catalog` other
than `owner`, and an input with import options (`family`, `set`, `alias`,
`naming`) but no XTCE document to apply them to.

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

## 3. Catalog owners

An input with `catalog = "owner"` owns the build's catalog, and several
inputs may. A build with an owner has a closed catalog: its names are
those the owners' files define, and the other inputs map onto them, so a
misspelt name fails the build instead of becoming a param no dashboard
shows. A build with no owner is open, each input adding to the catalog.

- A file is an owner's when the input it's read as part of (§2) is an
  owner. A file an owner includes that an earlier input, not an owner,
  already reads is an error, since it would be read as that input's and
  own nothing: an owner is listed before a directory holding it.
- An XTCE input that isn't an owner is imported referring to the catalog
  ([xtce](xtce.md) §3, §6.1): a parameter's alias in the `MDL` namespace
  names an owner's param, and a parameter without one is a field with no
  catalog name; a meta-command's alias names an owner's command, whose
  packet keeps the vendor's argument ranges and initial values as its own
  limits, and a meta-command without one is left out, with a warning that
  names it [`xtce.ignored`].
- A catalog param or command that a file other than an owner's defines,
  under a name no owner's file defines, is an error at that definition
  (`catalog_owner`) ([frontends](frontends.md) §5). A JSON or XTCE file
  that restates an owner's name merges with it as definitions from
  several frontends do. TOML files are one bundle, so a TOML file that
  restates an owner's name is the TOML frontend's conflict
  (`toml.conflict`): a vendor's TOML maps onto the owner's names rather
  than restating them.
- A name that maps onto nothing in the catalog (an XTCE alias, a TOML
  field's `canonical`) is reported by validation (`unknown_canonical`,
  `unknown_command`), whose message suggests the catalog's closest name
  when one is a few letters off (an edit for each four letters, at least
  one, ignoring case), for the first hundred such names.
