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
    2^64 − 1), a double, text or bytes, or for a list argument a list of
    these;
  - the target: the native id of the asset the command is sent to,
    needed only by a packet with a target field (§3.3).

## 2. Arguments

### 2.1 Resolution

- Each given argument MUST be one of the command's
  [`unknown_argument`], given once [`duplicate_argument`].
- An argument left out takes its default; one without a default is
  [`missing_argument`]. The result lists the defaulted arguments.
- Arguments are checked in two passes, and the first problem is the
  error. First the given names, in the order given [`unknown_argument`,
  `duplicate_argument`]; then every argument of the command, in catalog
  order: a given value by §2.2 to §2.5, in that order, or a left-out one
  by its default [`missing_argument`]. So a request built from a map,
  whose keys come in no particular order, is refused the same way
  however they come.

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
string. A list argument's default is a JSON array of them (§2.5). A
number with no fraction is an integer when it's in the i64 range and a
double past it, as canonical JSON reads it, so an `f64` default of 1e21
is written as a number and an integer argument's wide default as a string.

Validation guarantees that a default encodes in every command packet that
encodes its argument: it becomes a raw value its field holds (§3.1 to
§3.4), and a length its length field holds (§3.5); each value of a list
default does so in the array's element, or in its record's field. So a
request that leaves an argument out never fails on the argument's default.

### 2.3 Ranges

A numeric argument with a `range` MUST lie within it, ends included
[`out_of_range`]. A range's ends are values of the argument's type,
written as defaults are (§2.2): an `f64` argument's are numbers, and a
`u64` or `i64` argument's are integers, written past ±(2^53 − 1) as
decimal strings, so `{ "min": 1, "max": "1152921504606846975" }` is
1 to 2^60 − 1. Validation reports an end as it reports a default: an end
past ±(2^53 − 1) written as a number is `unsafe_integer`, one within it
written as a string `integer_as_string`, and an end that isn't a value of
the argument's type (a fraction, other text, an integer the type doesn't
hold) `value_range`. An integer is compared with a bound exactly, not
after converting it to a double.

### 2.4 Arguments a set leaves out

A set may leave out an argument that has a default (an older layout, from
before the argument was added). With that set the argument can only be
its default; any other value is [`not_default`].

### 2.5 Lists

An argument with `list` takes a list of values, and one without takes a
single value [`wrong_type`]. The list's length MUST lie within `list`'s
`min` and `max`, ends included, an absent end being open
[`list_length`]. Then each value is checked as §2.2 and §2.3 check a
single value, against the argument's type, states and range; the first
value that fails is the error, naming its index (counted from 0).

A list argument's default is a list whose length lies within `list`, each
value one its type, states and range allow. Lists that go together
(§3.6) have defaults that do: those that fill one array's records have
one length, and those of arrays that share a count field give it one
value, which it can hold (one length when the arrays' `multiply` and
`add` agree). Validation guarantees both, so the defaults of a command's
lists encode together; a caller's list that disagrees with a defaulted
one is [`bad_count`]. As for any argument with a default, a set
may leave a list argument out (§2.4), and then only its default can be
sent: a list equal to it value by value, in order.

## 3. Raw values

Each field of the packet gets one raw value, from exactly one source, as
does each field of an array's records; scalar elements get theirs from a
list argument (§3.6).

Raw values are worked out entry by entry, in order: a field's from its
source, an array's elements in index order, and for an array of records,
first the comparison of its lists' lengths, then each record's fields in
entry order. Then, in a binary payload, length fields, in the order of
the entries whose lengths they hold (§3.5), and then arrays' counts, in
entry order (§3.6). The first problem is the error.

### 3.1 Arguments

An argument becomes its field's raw value:

- **unconverted:** as it is (a bool into an integer field is 0 or 1; an
  enum into a string field is the state's name);
- **`linear`:** `(value − offset) / scale`;
- **`states`:** the raw value listed for the state's name;
- **`table`** with `linear` interpolation whose values only rise or only
  fall: the table read backwards, from value to raw: its points with raw
  and value swapped (and reversed, if the values fall), as a linear table
  (`decoding.md` §7.1), extrapolating if the table does;
- **any other conversion** (`poly`, `spline`, a `step` table, a table whose
  values rise and fall): the field's `inverse`, a `linear`, `poly` or
  `table` conversion from value to raw, applied to the value. The first
  kinds invert exactly and take no `inverse`; these need one.

An `f64` argument's value is first converted from the argument's unit
into the field's (`units.md` §4), so 25 `Cel` into a `K` field is 298.15
before its inverse; ranges stay in the argument's unit. Other argument
types are in their field's unit already.

Arithmetic follows `decoding.md` §7.1: binary64, each operation rounded
on its own in the order written, no fused multiply-add, no reciprocal. A
linear inverse subtracts, then divides; it does not multiply by `1 /
scale`. So 0.15 at scale 0.1 is 1.4999999999999998, and an inverse
`[-0.1, 10]` at 0.36 is 3.4999999999999996.

Validation guarantees that an argument encoded through anything but a
`linear` or `states` conversion has a range with both ends; that both ends
go through the inverse (inside an inverse table's points, unless it
extrapolates); that the raw values they become, rounded as below, lie
inside the field's table's or spline's points, unless it extrapolates;
and that an explicit `inverse` only rises or only falls over the range:
that its values never both increase and decrease from one input to the
next, the inputs being, for a `table`, `min`, the raw value of every point
strictly between `min` and `max`, and `max` (which checks it exactly), and
for a `poly`, the 4097 points `min + (k × (max − min)) / 4096`, for k from
0 to 4096. A `linear` inverse always qualifies. It also guarantees that the
range's ends, in the field's unit, through the inverse and rounded as
below, fit an integer field (its size in a binary payload, 64 bits in a JSON one), so every
value in range does. An absent end is the argument type's own, which for an
unconverted `u64` or `i64` argument is compared as the integer it is: such
an argument needs no range to fill a 64-bit field. A `bool` argument's
integer field holds 1, which a 1-bit `int` doesn't. In a float field, each
end the range gives becomes a finite raw value, a finite binary32 for a
32-bit field; an absent end isn't checked, so an argument without a range
can still be [`raw_range`] past binary32's largest.

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
- `fixed` with a `terminator`: the bytes MUST be at most that many
  [`bad_length`] and MUST NOT contain the terminator byte
  [`contains_terminator`]; the terminator fills the rest of the length
  (none when the bytes fill it), so a NUL terminator pads as C's
  `strncpy` does and a space pads with spaces;
- `terminator`: the bytes MUST NOT contain the terminator byte
  [`contains_terminator`], which is written after them;
- `field`: any length the length field can give (§3.5);
- `remaining`: any length.

Validation guarantees that the name of every state of an `enum` argument
that a string field writes is text the field can hold, as above. The
catalog doesn't bound a `str` or `bytes` argument's length, so a caller's
value can still be [`bad_length`].

### 3.5 Lengths

A field that a later entry takes its length from holds that entry's byte
length `n` as `(n − add) / multiply`, which MUST be a whole number of at
least 0 that fits the length field [`bad_length`, on the entry whose
length it is].

### 3.6 Arrays

An array encodes list arguments, one element per value:

- **scalar elements:** element *i* is the value *i* of the array's
  `argument`, turned into a raw value as §3.1 to §3.4 turn a field's, the
  element taken as a field (its type, size, conversion, `inverse`, unit,
  text and length);
- **struct elements (records):** record *i* holds, in each field that
  names an argument, that list's value *i*, and in each field with a
  `value`, the constant, each as §3.1 to §3.4 say. The lists a record's
  fields take MUST have the same length [`bad_count`], which is the
  number of records.

The array's `count` then holds:

- `fixed`: the number of elements MUST be the count [`bad_count`];
- `field`: the count field holds the number of elements `n` as `(n −
  add) / multiply`, which MUST be a whole number of at least 0 that fits
  it [`bad_count`]. Several arrays may take their count from one field;
  each MUST give it the same value [`bad_count`, on the later array], so
  arrays with the same `multiply` and `add` hold lists of one length. A
  field gives one length or counts arrays, not both (validation
  guarantees it);
- `remaining`: any number of elements.

Validation guarantees that every list the catalog allows fits: a `fixed`
count is the list's `min` and `max` both, and a count field holds the
list's `max` and has an `add` no greater than its `min`. Only a
`multiply` the length doesn't divide, lists of different lengths in one
array's records, and arrays sharing a count that disagree remain
[`bad_count`].

A problem with one element is that element's error, naming its index,
and for a record the field.

## 4. Binary payloads

- **Positions.** Each entry is written where decoding reads it (§5.2 of
  `decoding.md`): at its `offset_bits`, or where the previous entry ended.
  Validation guarantees that no two entries overlap. An array's elements
  follow one another from the array's position: a scalar element takes
  the bits a field of its shape takes, and a record lays its entries out
  from its own start, as `decoding.md` §5 lays out a struct's, ending at
  the furthest end of its entries.
- **Values.** Integers and bools are written as `decoding.md` §2.1 reads
  them, in the field's `byte_order` or else the family's, an `int` in
  two's complement in its size; floats as IEEE 754 binary32 or binary64,
  in the same byte order; strings and bytes from a byte boundary.
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
the way; every pointer token is an object key. An array is written at its
pointer as a JSON array of its elements, in order: a scalar element as a
field's value, a record as an object with each of its fields at its
pointer inside it. Validation guarantees that no pointer of a command
field, array or record field is empty (the whole object) and that, among
the entries of one object, no pointer equals another or leads into it.

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
- the raw value of each field and checksum, and each array's elements
  (raw values, or records of them), as decoding the payload gives them;
- the defaulted arguments, in catalog order;
- the command's `deprecated` note, if it has one. A deprecated command
  still encodes; a caller SHOULD warn.

## 7. Decoding commands

Decoding a command turns a command payload back into its catalog command
and arguments: what a simulated asset does with a command it receives,
and what command history records. It is the inverse of §1 to §6: every
payload an encoder writes MUST decode, and its arguments MUST encode to
the same payload again wherever §7.4 says they do.

### 7.1 Inputs

The artifacts, the set that decodes the payload, by name (as for
encoding, the caller chooses) [`unknown_set`], and a message as
`decoding.md` §1 defines it: the payload and the link's metadata. The
payload MUST be in the family's encoding [`wrong_encoding`,
`invalid_json`]. A command packet's metadata is part of its identity
(§7.2), so the message MUST carry the metadata the link framed the
command with; command history keeps it with the payload.

### 7.2 Identification

A command packet's keys are its metadata, by value, and its constant
fields that can be read without decoding anything else:

- binary: integer and bool fields at static positions (`decoding.md`
  §5.2), as the raw unsigned bits they write, read in the field's byte
  order as `decoding.md` §4 defines a key (an `int` 8-bit −1 and a `uint`
  8-bit 255 are the same key and value); and fixed-length string fields
  at static byte-aligned positions, as the bytes they write. In a buffer
  with a `terminator`, only the value and the one terminator after it
  are certain, so those bytes are the key (all of the buffer when the
  value fills it): `"PING"` in an 8-byte NUL-terminated buffer is the
  5-byte key `PING\0`. Constants of one length then share a key, and
  constants of different lengths are different keys;
- JSON: integer and string fields, by pointer, compared exactly.

Fields inside arrays are never keys.

Validation guarantees that every two command packets of a set share a key
with different values, so a payload matches at most one. A payload
matches a packet when every key's value in the message equals the
packet's; a key that can't be read (missing metadata, bits past the end)
doesn't match. No match is [`no_packet`].

### 7.3 The packet

The packet's entries are read as `decoding.md` §5 reads a packet's, with
the same errors (`past_end`, `bad_checksum`, `missing_value` and the
rest). A command packet has no length: the payload is the packet, and
bytes after its last entry, or keys of a JSON payload that no field
reads, are ignored, so layouts may grow at the end. A checksum covers
only bytes before it, so bytes past the packet are covered by the link's
framing alone; a `remaining` value or array takes in whatever follows it.
Every constant field MUST hold its value, including those that took no
part in identification and those in every record of an array
[`wrong_constant`].

### 7.4 Arguments

A float raw value MUST be finite [`non_finite`], which is checked before
its conversion: NaN and ±infinity are `non_finite` through any
conversion, as `decoding.md` §7.3 ranks `non_finite` above
`out_of_calibration` for telemetry. Each argument the packet encodes is
then its field's raw value:

- through a `states` conversion, the name of the state listing the raw
  value [`unknown_state`];
- through any other conversion, as `decoding.md` §7.1 converts it; a raw
  value outside a table's or spline's points that doesn't extrapolate is
  [`out_of_range`]; an `f64` value then converts from the field's unit
  into the argument's (`units.md` §4), and the range test below takes the
  range's ends into the field's unit first;
- unconverted, as it is: an integer as the nearest double for an `f64`
  argument, 0 or 1 as false or true for a `bool` argument
  [`out_of_range`].

A decoded value has its argument's type, whatever its field's type or its
default's spelling: an `i64` argument read from a `uint` field is a
signed integer, and a `u64` default written `0` is unsigned.

The value MUST be one the catalog allows, by §2.2 and §2.3, with the same
codes, but an `f64` argument with a range is tested on its raw value,
finite by then, in this order:

1. the raw value MUST lie between what the range's two ends become by
   §3.1 (the inverse conversion and its rounding, without the fit check),
   ends included, in either order; an open end counts as the largest
   finite double of its sign. An integer raw value is compared with the
   ends exactly, as §2.3 compares integers with bounds, never after
   converting it to a double [`out_of_range`];
2. the argument is the converted raw value, held to the range: a value
   past an end, infinite included, is that end. Past is strictly beyond
   by value, so -0.0 at an end of 0 stays -0.0.

So 3600.06 s at 0.1 s per count encodes to raw 36001, which converts to
3600.1 s and decodes as 3600.06 s when the range ends there. With an
inverse that only rises or only falls over the range (linear ones always
do, as do tables read backwards; validation checks every explicit
inverse, §3.1), a raw value the encoder
writes for an in-range value is never refused, whether or not the inverse
undoes the conversion exactly.

Encoding the decoded arguments gives the same payload when every
converted field's inverse undoes its conversion: when the raw value,
converted and then encoded again by §3.1, is the raw value. That holds
for unconverted arguments and states, and for linear conversions into
integer fields whose offset doesn't swamp the scaled value. It need not
hold for a float field with a conversion, or for a polynomial whose
`inverse` is approximate; there the decoded argument is what the raw value
means by the field's conversion, and the record holds the raw value
exactly. A terminated buffer's padding isn't part of its value, so a
payload padded otherwise than §3.4 pads decodes, and encodes again with
§3.4's padding.

A list argument is its array's elements in order, each converted as
above from its raw value: a scalar element's, or in each record the
field that names the argument. The list's length MUST be one `list`
allows [`list_length`]; then the first value the catalog doesn't allow
is the error, naming its index.

An argument the set doesn't encode is its default, and the result lists
it as defaulted. A packet with a target field gives the target's native
id: an integer field's integer, or a string field's text (text in an
integer's one decimal spelling is that integer).

## 8. Error codes

| Code | Where |
| --- | --- |
| `unknown_set`, `unknown_command` | the request (§1) |
| `unknown_argument`, `duplicate_argument`, `missing_argument`, `wrong_type`, `non_finite`, `unknown_state`, `out_of_range`, `not_default`, `list_length` | an argument (§2, §7.4); the error names it, and a list's value by its index |
| `raw_range`, `missing_target`, `target_type`, `invalid_text`, `contains_terminator`, `bad_length`, `bad_count` | a field or array (§3); the error names it, and an array's element by its index |
| `no_packet`, `wrong_constant`, and the routing and entry codes of `decoding.md` | decoding a command (§7) |
| `invalid_layout` | a definition that didn't pass validation; never for a validated one |
