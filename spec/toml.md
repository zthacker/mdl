# TOML

Status: normative from IR v2 (`mdl_version` 2) on. The key words MUST,
MUST NOT, SHOULD and MAY are to be interpreted as described in RFC 2119.

TOML is MDL's own authoring format. A TOML file is part of a bundle
(`frontends.md`) spelled in TOML v1.1.0: the same keys and nesting as the
IR's JSON, `kind` tags included. A file that uses only TOML v1.0.0 syntax
is also TOML v1.1.0, so 1.0 parsers read it too. There are no shorthands:
whatever the IR says, the TOML says with the same words, so the IR's
documentation (the `mdl::v2::ir` types) is the reference for both. The
reference implementation is the `mdl-toml` crate.

```toml
mdl_version = "2.0"

[[sets]]
name   = "vendor_x_v2"
family = "vendor_x"

[[sets.packets]]
name     = "HK"
identify = { apid = 0x064 }
length   = { kind = "at_least", bytes = 12 }

[[sets.packets.entries]]
kind        = "field"
name        = "bus_counts"
type        = "uint"
offset_bits = 64
size_bits   = 16
conversion  = { kind = "linear", scale = 0.01, offset = 0.0 }
unit        = "V"
canonical   = "eps.bus_voltage"
```

## 1. Files

A build reads one or more files. Each MUST state `mdl_version`, the IR
version it's written in, and MAY hold any of `catalog` (its `params` and
`commands`), `families` and `sets`, in any mix: a catalog split by
subsystem, a file per vendor with its family and sets, or everything in
one file.

Files are merged in the order given (`frontends.md` §4):

- **`mdl_version`:** the bundle's is the highest any file states, and its
  span is that file's `mdl_version`, so a version the reader can't read is
  reported there. Every file MUST state the same major version.
  [`toml.version`]
- **`catalog.params` and `catalog.commands`:** files add entries; each
  param or command is defined, whole, by one file.
- **`families` and `sets`:** each file's are appended, in file order. Two
  with one name are validation's `duplicate_name`.
- **Anything else** at the top level or in `catalog` is defined whole by
  one file.

How a table is written doesn't matter: `catalog.params` as a header in
one file, an inline table in another, and dotted keys in a third merge the
same way. A value defined by two files is an error at the later one, with
the earlier one as its first location (`frontends.md` §3). [`toml.conflict`]

## 2. Values

| TOML | In the bundle |
| --- | --- |
| string | the string |
| integer | the integer, written as a decimal string when it lies beyond ±(2^53 − 1), as the IR writes such integers. Hexadecimal, octal and binary are fine (`apid = 0x064`). An integer outside TOML's 64 bits is an error. [`toml.number`] |
| float | the number. NaN and infinities are errors. [`toml.number`] |
| boolean | the boolean |
| offset date-time | an instant, in MDL's spelling (`bindings.md` §2): UTC, `Z`, and the fraction without trailing zeros. `2000-01-01 00:00:00.250+00:00` is `"2000-01-01T00:00:00.25Z"`. A non-zero offset, second 60, year 0, a local date-time, a local date and a local time are errors. [`toml.datetime`] |
| array, table | the array, the object |

An instant may also be written as a string, which validation checks for
MDL's spelling.

## 3. Diagnostics

A file that isn't TOML is a syntax error, and every syntax error in every
file is reported. [`toml.syntax`]

| Code | What |
| --- | --- |
| `toml.syntax` | Text that isn't TOML, including a key defined twice in one file. |
| `toml.version` | A file without `mdl_version`, a version that isn't `"major.minor"`, or files with different major versions. |
| `toml.conflict` | Two files that define the same value. |
| `toml.number` | An integer outside 64 bits, or a float that isn't finite. |
| `toml.datetime` | A date or time that isn't an instant in UTC. |

The reader's and validation's diagnostics are reported at the TOML they
are about. A value's span is the value. A table's span is its header
(`[catalog.params."eps.bus_voltage"]`, `[[sets.packets]]`), its braces if
it's inline, or the first key that names it in a dotted key. A list or map
that several files add to has no span (`frontends.md` §4).
