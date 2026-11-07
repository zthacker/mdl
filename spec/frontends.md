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
| JSON | `mdl::v2::json` | a bundle written as the IR itself (§5) |

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
- a **span**, the source file and byte range, when one is known.

A frontend records a *source map*: for each JSON Pointer into the bundle,
the span of source text its value came from. It SHOULD record one for
every value it takes from a source. A diagnostic whose path has no span of
its own is reported at its nearest ancestor that has one, so a missing
field is reported where its object is, and a value the frontend wrote
itself where the construct it came from is.

Positions for people are a line and a column, both counted from 1. The
column counts Unicode scalar values; a tool that needs another count (an
editor that counts UTF-16 code units) works it out from the byte range.

## 4. Frontends that read several files

A frontend that builds one bundle from several files (TOML does):

- takes the files in the order it's given them, which decides which of two
  definitions comes first;
- MUST report anything that two files would put at the same JSON Pointer
  (one param in two catalog files, one family twice) as an error at the
  later one, with a message naming the earlier one's file, line and
  column. It MUST NOT keep either silently. List items never collide, since
  each file's are appended: two sets with one name reach validation, which
  reports them (`duplicate_name`) as it would from one file;
- gives a list or map assembled from several files (the bundle's `sets`,
  the catalog's `params`) no span of its own. A diagnostic about one is
  reported with its path and no position;
- reports its own diagnostics in file order, then by position within each
  file. Validation's follow, in validation's order.

## 5. The JSON frontend

The JSON frontend reads one bundle document: `{ "mdl_version", "catalog",
"families", "sets" }`. Several files are not merged.

- The text is read with the input rules of `canonical-json.md` §1, then
  typed. A bundle, unlike an artifact, need not be in normal form: it may
  use `null` for an absent value and `[]` or `{}` for an empty list or map.
- Every value's span runs from its first character to its last. An object
  member's pointer gets its value's span.
- A syntax error is reported where the parser stopped: at the character it
  couldn't accept or, when the text ends early, at its last character (at
  the very end, an empty span, if the last character is a line break). A
  file that starts with a byte order mark is refused at the mark, saying
  so. A duplicate key is reported at its second appearance, and the
  message names the line and column of the first.
