# Bindings files

Status: normative from IR v2 (`mdl_version` 2) on. The key words MUST,
MUST NOT, SHOULD and MAY are to be interpreted as described in RFC 2119.

A family's alias locator reads a native id from each message: a spacecraft
id, a serial number, a MAVLink system id. A bindings file says which asset
each native id is over time and, for families without a version locator,
which set decodes its messages. Bindings are not part of any artifact:
definitions and the asset registry change and deploy independently.

```toml
mdl_version = "2.0"

[[binding]]
family = "vendor_b"
native_id = "PX-9"
asset = "SAT-57"
set = "vendor_b_v2"
effective_from = "2026-06-01T00:00:00Z"

[[binding]]
family = "vendor_b"
native_id = "PX-9"
retired = true
effective_from = "2027-01-01T00:00:00Z"
```

## 1. File format

A bindings file MUST be a TOML 1.0 document. Readers MAY accept later
TOML versions, but a file that needs one isn't portable: TOML 1.0 readers
reject it. It has these top-level keys and no others:

| Key | Type | |
| --- | --- | --- |
| `mdl_version` | string | Required. `"major.minor"`, read like an artifact's `mdl_version`: a reader accepts its own major version with a minor version no newer than its own. |
| `binding` | array of tables | Optional. One table per row; absent means no rows. |

Each row has these keys and no others:

| Key | Type | |
| --- | --- | --- |
| `family` | string | Required. A family name: lowercase letters, digits and `_`, starting with a letter. [`invalid_name`] |
| `native_id` | integer or string | Required. See §3. |
| `effective_from` | string | Required. The instant the row takes effect, in the form of §2. [`invalid_instant`] |
| `asset` | string | Required unless the row is retired; not empty. [`binding_shape`, `empty`] |
| `set` | string | A set name, for families without a version locator (§5). A retired row has none. [`binding_shape`, `invalid_name`] |
| `retired` | boolean | Only `true`, which ends the native id's binding (§4). `false` is the default and MUST be left out. [`explicit_default`] |

A reader MUST reject a file with an unknown key, a value of the wrong
type, a number with a fraction or exponent, or a TOML date or time value.
Instants are strings because TOML's own datetimes allow several spellings
of one instant. A `retired = false` row that names an asset still binds it
(and is reported, [`explicit_default`]).

## 2. Instants

An instant has one spelling: `YYYY-MM-DDTHH:MM:SS`, optionally followed by
`.` and 1 to 9 fraction digits with no trailing zero, then `Z`. `T` and `Z`
are uppercase; the time is UTC; seconds run from 00 to 59 (no leap
seconds); years run from 0001 to 9999; the date MUST exist in the
proleptic Gregorian calendar. `2026-06-01T12:00:00.25Z` is valid;
`2026-06-01T12:00:00.250Z`, `2026-06-01T14:00:00+02:00` and
`2026-06-01t12:00:00z` are not. The same rule applies to every instant in
MDL, including CUC epochs in set artifacts.

## 3. Native ids

- An integer native id MUST lie within ±(2^53 − 1). A wider integer is
  written as a decimal string. [`unsafe_integer`]
- A string that spells an integer within ±(2^53 − 1) in its one decimal
  spelling MUST be written as that integer: `42`, not `"42"`.
  [`integer_as_string`]
- A string native id MUST NOT be empty. [`empty`]
- Native ids compare by value. A string in an integer's one decimal
  spelling (an optional `-`, no leading zeros, not `-0`) whose value lies
  between −2^63 and 2^64 − 1 is that integer, so a message whose alias
  reads the number 42 and one whose alias reads the text `"42"` name the
  same id. Any other string, such as `"042"` or `"PX-9"`, is compared as
  text.

## 4. Timelines

Rows with the same `family` and `native_id` form a timeline, ordered by
`effective_from`. A row is in effect from its `effective_from` (inclusive)
until the next row's `effective_from` (exclusive), or forever if it is the
last. To resolve a native id at an instant, a reader uses the row in effect
at that instant:

- no row in effect (the instant is before the first row): no asset;
- a retired row: no asset;
- otherwise the row's `asset` and, if present, `set`.

Two rows with the same `family`, `native_id` and `effective_from` MUST be
rejected. [`duplicate_binding`] Of the rows sharing all three, every row
but the first in file order is reported, at its `effective_from`. Apart
from that, the order of rows in the file has no meaning.

Decoders resolve at the message's ground time, so decoding a message again
later gives the asset that was bound when it arrived.

## 5. Set selection

A family with a version locator reads a layout version from every message,
and that version picks the set. A family without one relies on the
bindings: each row that binds an asset names the set that decodes its
messages.

## 6. Checks against definitions

A file that reads correctly can still disagree with the definitions it is
used with. Tools SHOULD check, against a validated build:

| Check | Code |
| --- | --- |
| The family exists. | `unknown_family` |
| The family's alias locator can produce the native id: for a `bits` locator, an integer from 0 to 2^size − 1; for a `fixed` locator, its value, compared as a native id (§3), so a fixed value `"42"` is the native id `42`. | `native_id` |
| A row that binds an asset names a set if and only if the family has no version locator. | `binding_set` |
| A named set exists and belongs to the family. | `unknown_set` |

Decoders don't need these checks to be correct: an id no row binds
resolves to no asset, and a set the decoder doesn't have is an error when
a message needs it. But a binding that fails them stops that asset's
messages from decoding (a set named for a family with a version locator
is refused, not ignored), so tools SHOULD run them before bindings are
deployed, in CI and when a decoder loads new bindings.

## 7. Diagnostics

A file that can't be read as a whole is rejected with a single error and
one of these codes, before any row is checked. The checks run in the
order listed: a file that isn't TOML is `invalid_toml`, whatever else is
wrong with it, and `mdl_version` is checked before any other key (as in
IR documents), so a reader given a newer file says so even if that file
also uses keys or values it doesn't know.

| Code | |
| --- | --- |
| `invalid_toml` | The text isn't a TOML document. |
| `missing_version` | There is no `mdl_version`. |
| `invalid_version` | `mdl_version` isn't a string of the form `"major.minor"`. |
| `unsupported_version` | The reader can't read that version (§1). |
| `wrong_shape` | An unknown key, a value of the wrong type, a fractional number or a TOML date or time (§1). |

Otherwise every problem in every row is reported at once, each with a
code from the brackets above and a JSON Pointer (RFC 6901). Rows are
numbered from 0 in file order. The pointer names the row and the key at
fault, such as `/binding/3/effective_from`, or the row alone when the
problem is a key that's missing (`binding_shape` for a row with neither
an asset nor `retired = true`).

A row whose `effective_from` is invalid, or whose meaning is unclear (no
asset and not retired, or retired yet naming an asset or set), takes no
part in its timeline, so it can't also be reported as a duplicate. Its
other problems are still reported.
