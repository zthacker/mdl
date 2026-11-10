# Frontends

Status: normative from IR v2 (`mdl_version` 2) on. The key words MUST,
MUST NOT, SHOULD and MAY are to be interpreted as described in RFC 2119.

A frontend reads definitions written in some format and produces a
*bundle*: one catalog, and the families and sets, in the IR's JSON shape.
Everything after that (validation, artifacts, decoding, encoding) is the
same whichever frontend wrote the bundle, so two frontends that describe
the same thing produce the same artifacts.

| Frontend | Where | Reads |
| --- | --- | --- |
| JSON | `mdl::json` | a bundle written as the IR itself (§6) |
| TOML | `mdl-toml` | the IR spelled in TOML, in any number of files (`toml.md`) |
| XTCE | `mdl-xtce` | OMG XTCE 1.0 to 1.2 documents: the subset `xtce.md` describes |

## 1. What a frontend produces

- The bundle MUST pass the reader's checks for a bundle: the number rules
  of `canonical-json.md` §1, the version check, and strict typed reading
  (unknown fields are errors). In Rust, a frontend builds a JSON value and
  reads it with `read::read_bundle_value`, rather than building IR types
  itself, so every frontend is held to the same contract.
- Units are UCUM codes from `units.md`. A frontend MAY accept the
  spellings its own format uses (`degC`, `°C`) and MUST write the code.
- Instants are written in MDL's one spelling (`bindings.md` §2).
- Integers are JSON numbers within ±(2^53 − 1); wider ones are decimal
  strings, as the IR says.

## 2. What a source has that MDL can't represent

A frontend MUST NOT drop anything silently. Whatever the source says that
MDL can't represent is a diagnostic that names the construct and where it
is:

- an **error** when leaving it out, or approximating it, would change any
  decoded or encoded value: an encoding, a calibration, a condition that
  selects a layout. The frontend MUST NOT produce a bundle;
- a **warning** when it carries something MDL doesn't model and decoded
  and encoded values are the same without it: alarm rules beyond limits,
  algorithms, command verifiers. The bundle is produced. Tools SHOULD
  offer a way to treat warnings as errors.

## 3. Diagnostics and locations

A diagnostic has:

- a **severity**, `error` or `warning`;
- a **code**. The reader's codes (`invalid_json`, `wrong_shape`,
  `unsupported_version` and the others) and validation's (`unit_mismatch`
  and the others) are used as they are. A frontend's own codes start with
  its name and a dot (`toml.syntax`), so they never collide with the
  core's or each other's. A code is never renamed or reused once released;
- a **message** and, where the location alone doesn't say what it is
  about, a **context** for people ("set vendor_x_v2, packet HK, entry
  mode");
- a **path**, the JSON Pointer (RFC 6901) into the bundle, when the
  problem is in the bundle;
- a **span**, the source file and byte range, when one is known;
- for something defined twice, a **first location**: where the first
  definition is, the diagnostic's own span being the second. Validation gives it as a
  JSON Pointer and the source map turns it into a span; a frontend's own
  duplicates give the span directly.

A frontend records a *source map*: for each JSON Pointer into the bundle,
the span of source text its value came from. It SHOULD record one for
every value it takes from a source. A diagnostic whose path has no span of
its own is reported at its nearest ancestor that has one, so a missing
field is reported where its object is, and a value the frontend wrote
itself where the construct it came from is. The climb stops below the
bundle's own lists and maps (the bundle, its `catalog`, the catalog's
`params` and `commands`, `families` and `sets`), whose spans can't stand
for one definition among many: a diagnostic about something the bundle
doesn't have, such as a param a newer catalog removed, has no position.

Positions for people are a line and a column, both counted from 1. The
column counts Unicode scalar values; a tool that needs another count (an
editor that counts UTF-16 code units) works it out from the byte range.

## 4. Frontends that read several files

A frontend that builds one bundle from several files (TOML does):

- takes the files in the order it's given them, which decides which of two
  definitions comes first;
- MUST report anything that two files would put at the same JSON Pointer
  (one param in two catalog files, a `catalog` key other than `params` and
  `commands` in two files) as an error at the later one, with the earlier
  one as its first location (§3). It MUST NOT keep either silently. List
  items never collide, since each file's are appended: two families or two
  sets with one name reach validation, which reports them
  (`duplicate_name`) at the later one, with the earlier one as its first
  location, as it would in one file;
- gives a list or map assembled from several files (the bundle's `sets`,
  the catalog's `params`) no span of its own. A diagnostic about one is
  reported with its path and no position;
- reports its own diagnostics in file order, then by position within each
  file. Validation's follow, in validation's order.

## 5. Bundles from several frontends

A program may read definitions with more than one frontend, a catalog in
TOML beside a vendor's packets imported from XTCE, say, and merge their
bundles into one before validation (the core's `merge`, which `morphc`
uses):

- The bundles merge in the order given, which is also their precedence.
  Families and sets are appended, so two with one name reach validation,
  which reports them (`duplicate_name`), as §4 says of one frontend's
  files.
- Catalog params and commands merge by name, so an imported parameter can
  map to a catalog's name. Two definitions of one param MUST agree on
  what decoding depends on: type, unit, states (in order, as the IR lists
  them) and frame. Two of one command MUST have the same arguments, in
  order, agreeing on everything but their descriptions: type, unit,
  states, range, list bounds and default. Any disagreement is an error
  (`duplicate_name`) at the later definition's value, with the earlier's
  as its first location (§3), and its message names both values.
- Limits, descriptions and deprecation notes change neither decoding nor
  encoding. One only the later definition gives is taken from it. When
  both give one and they differ, the earlier's is kept and the later's
  is reported as a warning (`overridden`) at the later value, with the
  earlier's as its first location, and its message names both values.
  Listing a curated catalog first makes its texts and limits win over an
  import's.
- The merged bundle's IR version is the newest any part has.
- Every part's spans index the same set of sources. Where two parts
  record a span for one JSON Pointer, the earlier part's is kept. The
  bundle and its `catalog` hold values from every part, so once two parts
  merge they have no span, as §4 says of a list or map assembled from
  several files; nor do the catalog's `params` or `commands`, `families`
  or `sets` once two parts give them items. A replaced value's spans are
  dropped, so a diagnostic points into the file its value came from.
- The diagnostics, each part's warnings with the merge's own, are in file
  order, then by position: the order of the files in the shared sources,
  which needn't be the order of the parts. A failed merge returns its
  warnings with its errors.

## 6. The JSON frontend

The JSON frontend reads one bundle document: `{ "mdl_version", "catalog",
"families", "sets" }`. Several files are not merged.

- The text is read with the input rules of `canonical-json.md` §1, then
  typed. A bundle, unlike an artifact, need not be in normal form: it may
  use `null` for an absent value and `[]` or `{}` for an empty list or map.
  That is the only latitude: every other value is spelled as the IR writes
  it, so a tagged object's `kind` is the string naming its kind, never a
  number.
- Every value's span runs from its first character to its last. An object
  member's pointer gets its value's span.
- A syntax error is reported where the parser stopped: at the character it
  couldn't accept or, when the text ends early, at its last character (at
  the very end, an empty span, if the last character is a line break). A
  file that starts with a byte order mark is refused at the mark, saying
  so. A duplicate key is reported at its second appearance, with the first
  as its first location (§3).
