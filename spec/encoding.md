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
- An argument left out takes its default, the packet's where it gives
  one (§2.6); one without a default is [`missing_argument`], as is one
  the packet requires (§2.6), whatever the catalog's default. The result
  lists the defaulted arguments.
- Arguments are checked in two passes, and the first problem is the
  error. First the given names, in the order given [`unknown_argument`,
  `duplicate_argument`]; then every argument of the command, in catalog
  order: a given value by its type, range and list (§2.2, §2.3, §2.5),
  then by §2.4, or a left-out one by its default [`missing_argument`].
  So a request built from a map,
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
| `enum` | text naming one of its states [`unknown_state`] that the packet accepts (§2.6) [`unsupported_state`] |

Anything else is [`wrong_type`]. A double for an `f64` argument MUST be
finite [`non_finite`].
Defaults are values like any other: a bytes default is written in the
catalog as lowercase hex, and an integer past ±(2^53 − 1) as a decimal
string. A list argument's default is a JSON array of them (§2.5). A
number with no fraction is an integer when it's in the i64 range and a
double past it, as canonical JSON reads it, so an `f64` default of 1e21
is written as a number and an integer argument's wide default as a string.

Validation guarantees that a default encodes in every command packet that
encodes its argument and doesn't require it (§2.6): it becomes a raw value
its field holds (§3.1 to
§3.4), and a length its length field holds (§3.5); each value of a list
default does so in the array's element, or in its record's field. So a
request that leaves an argument out never fails on the argument's default.

### 2.3 Ranges

A numeric argument with a `range` MUST lie within it, ends included, and
within the packet's where it gives one (§2.6) [`out_of_range`]. A range's ends are values of the argument's type,
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

### 2.6 A packet's own limits

A command packet MAY narrow what it accepts of the arguments it encodes
(`arguments`, IR 2.2), as a vendor's packet under a fleet's catalog does:
by argument name, a `range`, a `default` or both, in `unit`, the
argument's when it's left out; for an enum argument, the `states` it
accepts; and whether it's `required` (both IR 2.3).

- A value is checked against the catalog's range and the packet's, so
  it MUST lie within both [`out_of_range`], whose message names the
  range the two leave (`0 to 60`) and, when the catalog's alone would
  allow the value, says that range is the set's and gives the catalog's.
  A packet range wider than the catalog's narrows nothing. A list
  argument's values are each checked.
- An enum argument's `states` are those of the catalog's states the
  packet accepts, as a vendor whose flight software lacks a mode does. A
  state the catalog has and the packet doesn't accept is
  [`unsupported_state`], whose message names the states it accepts; a
  name the catalog doesn't have is [`unknown_state`], as without them. So
  a consumer can tell a state no asset has from one this packet doesn't.
  A list argument's values are each checked. Listing every state isn't
  the same as leaving `states` out: it fixes what the packet accepts, so a
  state a later catalog adds isn't accepted there and its field needn't
  give it a raw value, where without `states` the packet accepts every
  state its catalog has.
- The packet's default is the argument's default with that packet; the
  catalog's is used only where the packet gives none.
- `required: true` means every request sent with the packet gives the
  argument, as a vendor's packet needs a value the fleet's catalog lets
  other vendors leave out. Left out, it's [`missing_argument`], whose
  message says the set requires it, even where the catalog has a
  default: that default isn't sent with the packet. `false` is the
  default. An argument the catalog gives no default may be required too:
  that changes nothing now, and keeps the packet's meaning if a later
  catalog gives it one.
- A `unit` other than the argument's converts the range's ends and the
  default into the argument's unit (`units.md` §4) before anything is
  compared, which only an `f64` argument's limits can need: an integer
  argument's are in its own unit.
- Validation guarantees that each names an argument of the command
  [`unknown_argument`] that the packet encodes [`unmapped_argument`] and
  gives a range, a default, states or `required` [`empty`], in a unit other
  than the argument's own, which is what leaving it out means
  [`explicit_default`, when building: a stored build an earlier release
  wrote with it still opens, `compatibility.md` §7], that converts
  [`unit_mismatch`], given only beside
  a range or a default, the values it's for [`command_shape`]; that
  `required` is written only
  as `true` [`explicit_default`] and never beside a default of the
  packet's own, which would never be sent [`required_default`]; that the
  two ranges share a value [`value_range`]; that states are given only
  for an enum argument [`param_shape`], at least one [`empty`], each one
  of the argument's states [`unknown_state`, suggesting the closest name]
  listed once [`duplicate_name`] and in the catalog's order
  [`not_sorted`], so a set of states has one spelling and a set one
  digest; and that the default
  the packet sends is one both ranges allow and one of its states
  [`default_value`]. A packet that requires an argument sends no default,
  so the catalog's isn't checked against it. Its other guarantees (§2.2,
  §2.5, §3.1) hold for the argument as the packet narrows it, so a packet
  that keeps an argument to what its field holds encodes it, whatever the
  catalog allows.
- Decoding a command (§7.4) tests a value against the narrowed range and
  states, and an argument the packet leaves out decodes to its default as
  above.

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
- **`states`:** the raw value listed for the state's name. Validation
  guarantees that the conversion names only the argument's states, each
  once, and lists a raw value for each state the packet accepts (§2.6),
  every state of the argument where the packet doesn't say
  [`canonical_type`]. It may list one for a state the packet doesn't
  accept, which decoding a command names and then refuses (§7.4);
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
  them, in the field's `byte_order` or else the family's, an `int` in its
  size in its signed form (two's complement unless `signed` says
  otherwise), 0 always as the zero with no bits set; floats as IEEE 754
  binary32 or binary64, in the same byte order; strings and bytes from a
  byte boundary. An `int` in sign-magnitude or ones' complement holds
  ±(2^(n−1) − 1), one negative value fewer than two's complement, which
  is the range its arguments are checked against.
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

The text is written as the JSON Canonicalization Scheme (RFC 8785) writes
JSON: no whitespace, object members in order of their keys' UTF-16 code
units, strings escaped as it requires, and doubles as ECMAScript's
`Number.prototype.toString` writes them with ECMA-262's Note 2, as RFC
8785 requires: the fewest digits that read back as the double; of those,
the one nearest it; and of two equally near, the even one (`1.5`, `0` for
0.0 and −0.0, `1e+21`; `5e-324` for the least subnormal, which `3e-324` to
`7e-324` all read back as; and `1214.1973266601562` for
1214.19732666015625, not the equally near `1214.1973266601563`). The one
difference is integers: an integer field's value is written in exact
decimal digits whatever its size, where RFC 8785 would write the nearest
double. So a payload whose integers are all within ±(2^53 − 1) is RFC 8785
canonical JSON.

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
- JSON: integer and string fields, by pointer, compared exactly: a
  string with a string, a number by its value (`decoding.md` §2.3, so
  `42.0` is the key 42), never a string with a number.

Fields inside arrays are never keys.

A constant key is the bits its field writes, so in sign-magnitude or ones'
complement a key of 0 matches only the zero with no bits set, which is
what an encoder writes; a telemetry key of 0 compares values and matches
either zero (`decoding.md` §4). A constant that isn't a key and holds the
other zero reads as 0, so it passes §7.3's check.

Validation guarantees that every two command packets of a set share a key
with different values, so a payload matches at most one. A payload
matches a packet when every key's value in the message equals the
packet's; a key that can't be read (missing metadata, bits past the end)
doesn't match. No match is [`no_packet`].

### 7.3 The packet

The packet's entries are read as `decoding.md` §5 reads a packet's, with
the same errors (`past_end`, `bad_checksum`, `missing_value` and the
rest; a JSON payload that isn't an object is `wrong_type`, §5.3). A command packet has no length: the payload is the packet, and
bytes after its last entry, or keys of a JSON payload that no field
reads, are ignored, so layouts may grow at the end. A checksum covers
only bytes before it, so bytes past the packet are covered by the link's
framing alone; a `remaining` value or array takes in whatever follows it.
Every constant field MUST hold its value, including those that took no
part in identification and those in every record of an array
[`wrong_constant`]. The error names the field by its path, as
`decoding.md` §9 names an entry: `waypoints[1].kind` for a record's.

### 7.4 Arguments

A float raw value MUST be finite [`non_finite`], which is checked before
its conversion: NaN and ±infinity are `non_finite` through any
conversion, as `decoding.md` §7.3 ranks `non_finite` above
`out_of_calibration` for telemetry. Each argument the packet encodes is
then its field's raw value:

- through a `states` conversion, the name of the state listing the raw
  value [`unknown_state`], which the packet MUST accept
  [`unsupported_state`];
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

The value MUST be one the catalog allows, as the packet narrows it
(§2.6), by §2.2 and §2.3, with the same codes, but an `f64` argument
with a range is tested on its raw value, finite by then, in this order:

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

A list argument is its array's elements in order, each converted as above
from its raw value: a scalar element's, or in each record the field that
names the argument. The list's length MUST be one `list` allows
[`list_length`]; then the first value the catalog doesn't allow is the
error, naming its index. A `null` element, which `decoding.md` §5.3 reads
as no reading, is no value: it's [`wrong_type`], naming its index. A
`null` record gives none to any of its lists, so the first of them in
catalog order is refused at that index; it has no constants to check
(§7.3).

An argument the set doesn't encode is its default, and the result lists
it as defaulted. A packet with a target field gives the target's native
id: an integer field's integer, or a string field's text (text in an
integer's one decimal spelling is that integer).

## 8. Error codes

| Code | Where |
| --- | --- |
| `unknown_set`, `unknown_command` | the request (§1) |
| `unknown_argument`, `duplicate_argument`, `missing_argument`, `wrong_type`, `non_finite`, `unknown_state`, `unsupported_state`, `out_of_range`, `not_default`, `list_length` | an argument (§2, §7.4); the error names it, and a list's value by its index |
| `raw_range`, `missing_target`, `target_type`, `invalid_text`, `contains_terminator`, `bad_length`, `bad_count` | a field or array (§3); the error names it, and an array's element by its index |
| `no_packet`, `wrong_constant`, and the routing and entry codes of `decoding.md` | decoding a command (§7) |
| `invalid_layout` | a definition that didn't pass validation; never for one a build accepts, though a store an earlier release built can give it for a checksum that starts before its `from_bits` (`decoding.md` §5.6) |
