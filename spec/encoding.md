# Encoding

Status: normative from IR v2 (`mdl_version` 2) on. The key words MUST,
MUST NOT, SHOULD and MAY are to be interpreted as described in RFC 2119.

Encoding turns a canonical command and its arguments into the payload of
a set's command packet. Two implementations given the same artifacts and
request MUST produce the same payload byte for byte, or the same error
code. Encoding is a pure function of those inputs.

A command encodes completely or not at all. Any failure in this document
refuses the whole request with the code in brackets. An encoder never
clamps, truncates or substitutes a value.

`decoding.md` defines bits, byte orders, text encodings, positions and
checksums; this document uses those definitions and doesn't repeat them.

## 1. Inputs

- **Artifacts:** a family, its sets, and the catalogs they pin, loaded and
  validated, as for decoding.
- **The request:**
  - the set that encodes the command, by name [`unknown_set`]. An asset's
    binding names its set when its family has no version locator; with a
    version locator, the caller chooses;
  - the canonical command, which the set MUST encode
    [`unknown_command`];
  - arguments: values by name, each a bool, an integer (−2^63 to
    2^64 − 1), a double, text or bytes;
  - the target: the native id of the asset the command is sent to,
    needed only by a packet with a target field (§3.3).

## 2. Arguments

### 2.1 Resolution

- Each given argument MUST be one of the command's
  [`unknown_argument`], given once [`duplicate_argument`].
- An argument left out takes its default; one without a default is
  [`missing_argument`]. The result lists the defaulted arguments.
- Arguments are checked in the order given, then in catalog order; the
  first problem is the error.

### 2.2 Types

Numbers compare by value: `3`, `3.0` and an unsigned `3` are the same
number.

| Argument type | Accepts |
| --- | --- |
| `f64` | a double, or an integer a double holds exactly |
| `i64`, `u64` | an integer, or a double with no fraction, within the type's range [`out_of_range`] |
| `bool` | a bool |
| `str` | text |
| `bytes` | bytes |
| `enum` | text naming one of its states [`unknown_state`] |

Anything else is [`wrong_type`]. A double for an `f64` argument MUST be
finite [`non_finite`].
Defaults are values like any other: a bytes default is written in the
catalog as lowercase hex, and an integer past ±(2^53 − 1) as a decimal
string.

### 2.3 Ranges

A numeric argument with a `range` MUST lie within it, ends included
[`out_of_range`]. An integer is compared with a bound exactly, not after
converting it to a double.

### 2.4 Arguments a set leaves out

A set may leave out an argument that has a default (an older layout, from
before the argument was added). With that set the argument can only be
its default; any other value is [`not_default`].

## 3. Raw values

Each field of the packet gets one raw value, from exactly one source.

### 3.1 Arguments

An argument becomes its field's raw value:

- **unconverted:** as it is (a bool into an integer field is 0 or 1; an
  enum into a string field is the state's name);
- **`linear`:** `(value − offset) / scale`;
- **`poly`:** the field's `inverse` conversion applied to the value;
- **`states`:** the raw value listed for the state's name.

Arithmetic follows `decoding.md` §7.1: binary64, each operation rounded
on its own in the order written, no fused multiply-add, no reciprocal. A
linear inverse subtracts, then divides; it does not multiply by `1 /
scale`. So 0.15 at scale 0.1 is 1.4999999999999998, and an inverse
`[-0.1, 10]` at 0.36 is 3.4999999999999996.

A result for an integer field is rounded to the nearest integer, ties away
from zero (0.25 s at 0.1 s per count is raw 3; the two results above are
raw 1 and raw 3). A result for a 32-bit float field is rounded to the
nearest binary32, ties to even, subnormals kept. The raw value MUST fit
its field [`raw_range`]: an integer within the field's size (binary) or 64
bits (JSON), a float finite, a binary32 finite after rounding.

### 3.2 Constants

A field's `value` is its raw value. In a bool field 0 is false and 1 is
true.

### 3.3 Target

A field with `"target": true` holds the target's native id
[`missing_target`]. An integer field takes an integer id that fits it
[`target_type`, `raw_range`]; a string field takes the id's text, an
integer id written in decimal.

### 3.4 Strings and bytes

In a binary payload, text is written in the field's encoding: UTF-8 as it
is, ASCII only below 0x80, Latin-1 only up to U+00FF [`invalid_text`].
Then, by the field's length:

- `fixed`: the bytes MUST be exactly that many [`bad_length`]; the
  caller pads a shorter value, and decoding keeps the padding;
- `terminator`: the bytes MUST NOT contain the terminator byte
  [`contains_terminator`], which is written after them;
- `field`: any length the length field can give (§3.5);
- `remaining`: any length.

### 3.5 Lengths

A field that a later entry takes its length from holds that entry's byte
length `n` as `(n − add) / multiply`, which MUST be a whole number of at
least 0 that fits the length field [`bad_length`, on the entry whose
length it is].

## 4. Binary payloads

- **Positions.** Each entry is written where decoding reads it (§5.2 of
  `decoding.md`): at its `offset_bits`, or where the previous entry ended.
  Validation guarantees that no two entries overlap.
- **Values.** Integers and bools are written as `decoding.md` §2.1 reads
  them, an `int` in two's complement in its size; floats as IEEE 754
  binary32 or binary64; strings and bytes from a byte boundary.
- **Padding and gaps.** Bits no entry covers, padding included, are 0.
- **Size.** The payload ends where its furthest entry ends (after its
  last bit, or at its start for an entry that takes no bits, such as an
  empty `remaining` value), rounded up to a whole byte.
- **Checksums** are written after every other entry, in order of
  position, so a checksum covers the final value of any checksum before
  it. Each is computed as `decoding.md` §5.6 defines and stored in its
  algorithm's byte order. An `iso8473` checksum is always written as
  computed, never as 0x0000.

## 5. JSON payloads

The payload is one JSON object, `{}` for a packet with no entries. Each
field's raw value is written at its pointer, with objects created along
the way; every pointer token is an object key. Validation guarantees that
no command field's pointer is empty (the whole document) and that no
pointer equals another or leads into it.

The text is written as the JSON Canonicalization Scheme (RFC 8785)
writes JSON: no whitespace, object members in order of their keys' UTF-16
code units, strings escaped as it requires, and doubles as ECMAScript
writes them (`1.5`, `0` for 0.0, `1e+21`). The one difference is
integers: an integer field's value is written in exact decimal digits
whatever its size, where RFC 8785 would write the nearest double. So a
payload whose integers are all within ±(2^53 − 1) is RFC 8785 canonical
JSON.

## 6. The result

- the payload: bytes, or the JSON text as UTF-8. A binary packet with no
  entries has an empty payload, which the link frames on its own;
- the packet, whose `metadata` the link frames the payload with;
- the set's digest, to record with what was sent;
- the raw value of each field and checksum, as decoding the payload gives
  them;
- the defaulted arguments, in catalog order;
- the command's `deprecated` note, if it has one. A deprecated command
  still encodes; a caller SHOULD warn.

## 7. Error codes

| Code | Where |
| --- | --- |
| `unknown_set`, `unknown_command` | the request (§1) |
| `unknown_argument`, `duplicate_argument`, `missing_argument`, `wrong_type`, `non_finite`, `unknown_state`, `out_of_range`, `not_default` | an argument (§2); the error names it |
| `raw_range`, `missing_target`, `target_type`, `invalid_text`, `contains_terminator`, `bad_length` | a field (§3); the error names it |
| `invalid_layout` | a definition that didn't pass validation; never for a validated one |
