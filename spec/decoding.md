# Decoding

Status: normative from IR v2 (`mdl_version` 2) on. The key words MUST,
MUST NOT, SHOULD and MAY are to be interpreted as described in RFC 2119.

Decoding turns one message into who sent it, which packet it is, every
value the packet defines, its canonical samples and its event time. Two
implementations given the same artifacts, bindings and message MUST
produce the same result, or the same error code. Decoding is a pure
function of those inputs: decoding a message again later gives the same
result, which is what makes replaying raw data safe.

A message decodes completely or not at all. Any failure in this document
rejects the whole message with the code in brackets; nothing partial is
returned. Bad *values* are different: they become samples with a quality
flag (§7.3), and the message still decodes.

## 1. Inputs

- **Artifacts:** a family, its sets, and the catalogs they pin, loaded and
  validated. Sets loaded together for one family MUST have unique names
  and versions.
- **Bindings:** an asset registry as `spec/bindings.md` defines it.
- **The message:**
  - a payload: binary bytes, or one JSON document as bytes;
  - metadata: values the link provides, by name, each an unsigned
    integer, a signed integer or text, each name at most once;
  - ground time: when the message was received.

## 2. Values

### 2.1 Bits

Bits are numbered from the most significant bit of the first byte, in
both byte orders: bit 0 is the top bit of byte 0, bit 8 the top bit of
byte 1. A value of 1 to 64 bits at bit `o` is read as an unsigned integer:

- **big-endian:** bits `o` to `o + size - 1`, most significant first;
- **little-endian:** whole bytes on byte boundaries, least significant
  byte first, or, for a value inside one byte, as big-endian (byte order
  doesn't apply within a byte). A little-endian value that is neither (8
  bits from bit 3, 12 bits from a byte boundary) has no reading, and
  validation refuses it.

A field or scalar array element is read in its own `byte_order` when it
has one, and in its family's otherwise; the rules above apply by that
order. This describes, for example, a little-endian payload behind a
big-endian header. Only `uint`, `int` and `float` values have a byte
order. Each layout has one spelling: a value's `byte_order` MUST NOT be
its family's, and MUST NOT be given on a value that lies inside one byte
at every start residue it has (§5.2). `bits` locators (§3.1) and
checksums (§5.6) always use the family's order, or the algorithm's own.

A signed (`int`) value is the unsigned value read as two's complement in
its size. A `float` is IEEE 754 binary32 or binary64. A `bool` is one bit,
true if set. A read past the end of the packet is `past_end`.

### 2.2 Text and bytes

Strings and bytes start on a byte boundary and are a whole number of
bytes. Text is decoded strictly: UTF-8 MUST be valid, ASCII MUST be below
0x80, and Latin-1 maps each byte to the code point of the same value;
anything else is `invalid_text`. A fixed length keeps every byte, padding
included, unless it names a `terminator`: then the value is the bytes
before the first terminator byte among them, or all of them when there's
none, and the rest is padding, which isn't decoded (a C `char[16]`). The
entry takes its whole length either way. The value ends at the first
terminator, so it can't hold one; trailing padding to strip from a value
that may contain the padding byte (`"HELLO WORLD   "`) is another thing,
which a terminator doesn't describe.

### 2.3 JSON payloads

A JSON payload MUST be valid UTF-8 JSON (RFC 8259) with no duplicate
object member names at any depth, comparing names after unescaping;
otherwise the message is `invalid_json`. Parsers differ on these, so the
rules are explicit:

- of two duplicate names, parsers keep different ones, so a payload with
  any is refused;
- a number whose value is beyond the binary64 range (`1e400`) is refused;
- a string with an unpaired surrogate, escaped (`\ud800`) or not, is
  refused;
- arrays and objects nested 128 or more deep are refused (127 levels are
  accepted).

A JSON number is **an integer** when its value is an exact integer: an
integer literal within 64 bits, or a number with no fractional part within
±(2^53 − 1). So `42`, `42.0` and `4.2e1` are all 42; `1.5` and `1e20`
are not integers, and neither is an integer literal beyond 64 bits.

## 3. Routing

### 3.1 Locators

A family's alias and version locators read one value from the message:

| Locator | Reads |
| --- | --- |
| `bits` | an unsigned integer at its position (§2.1), in the family's byte order, so in a little-endian family it is whole bytes on byte boundaries or inside one byte |
| `metadata` | the named metadata value; a name given more than once can't be read |
| `path` | the JSON value at its pointer: a string, or a number that is an integer (§2.3) |
| `fixed` | its text |

A locator that finds nothing usable is `unreadable_locator`.

### 3.2 Comparing

- **Native ids** compare by value (`spec/bindings.md` §3): text in an
  integer's one decimal spelling (optional `-`, no leading zeros, not
  `-0`) between −2^63 and 2^64 − 1 is that integer.
- **Versions** compare the way validation compares set versions: read by
  a `path` locator, exactly (the string `"2"` and the number 2 differ);
  read by `bits` or `metadata`, by value (`"2"` is 2; `"02"` is text).

### 3.3 Order

The checks run in this order, and the first failure is the message's
error:

1. the payload is in the family's encoding [`wrong_encoding`];
2. the alias locator [`invalid_json`, `unreadable_locator`];
3. the asset: the binding in effect at the message's ground time
   (`spec/bindings.md` §4) [`unknown_asset`];
4. the set:
   - for a family with a version locator, a binding that names a set is
     [`unexpected_set`]; then the version locator [`invalid_json`,
     `unreadable_locator`], and a loaded set with that version
     [`unknown_version`];
   - for a family without one, the binding's set [`missing_set`], which
     is loaded [`unknown_set`].

After routing, a decoder identifies the packet (§4), extracts it (§5),
then reads its event time (§8); here too, the first failure is the error.
A JSON payload is parsed once: by routing, if a locator reads it, and
otherwise as decoding starts, so a payload that isn't valid JSON is
`invalid_json` before identification (never `no_packet`).

## 4. Identification

Each packet's identify keys resolve as validation resolves them: a JSON
Pointer; a metadata name the family declares; or a top-level integer or
bool field of the packet, by its static position (§5.2) if it has one.
Values compare as in §3.2: pointers exactly, metadata and fields by value.

A field key at a static position is its position, size, signedness and
byte order (§2.1), whatever the field is named: two packets whose keys
read the same bits in the same way share a key. Read in different byte
orders, the same bits are different keys, except inside one byte, where
both orders read the same.

- A message matches a packet when every key's value in the message equals
  the packet's. Validation guarantees that the packets one message can
  match have strictly nested keys, so a decoder MUST choose the match with
  the most keys. An empty identify (the catch-all) matches every message.
- A key that can't be read (missing metadata, bits past the end, nothing
  at a pointer, a field that can't be decoded) doesn't match; a more
  specific packet then gives way to a less specific one.
- A field key without a static position is read by decoding its packet up
  to that field and no further. A failure before the field means the key
  can't be read; a failure after it MUST NOT affect identification (if the
  packet is chosen, decoding it reports the failure).
- No matching packet is [`no_packet`].
- A packet's length takes no part in identification: a payload whose keys
  match a packet but that's shorter than its length is [`too_short`], not
  a less specific packet.

Implementations MAY try packets in any order that gives this result. MDL's
groups packets by key set, tries groups from most keys to fewest, reads
cheap keys first, and stops a group as soon as no packet in it can match.

## 5. Extraction

### 5.1 Length

A packet's `length`:

- `exact`: a payload shorter than it is [`too_short`]; a longer one is
  accepted and only the first `bytes` bytes are the packet, so layouts may
  grow at the end;
- `at_least`: a payload shorter than it is [`too_short`];
- none: the payload is the packet.

Everything below reads within the packet.

### 5.2 Binary entries

Entries are decoded in order within their container (a packet, or a
struct).

- **Position.** An entry with `offset_bits` starts there, relative to its
  container's start; one without starts where the previous entry ended,
  or at the container's start.
- **Conditions.** An entry with `when` is present only if its condition
  holds (§5.4). An absent entry ends where it would have started: an
  absent entry with `offset_bits` still moves the next sequential entry to
  that offset.
- **Static positions.** An entry's position is static when no message
  can move it: it has `offset_bits`, or it is its container's first
  entry, or the entry before it has a static position and an exact size.
  Exact sizes are those of an entry without `when` that is a field with
  `size_bits` or a `fixed` length, padding, an array with a `fixed` count
  of elements with an exact size, a struct whose type has an exact size,
  or any checksum. A struct type has an exact size when every entry of it
  has a static position and an exact size; its size is then the furthest
  end of its entries. Identification (§4) and checksums (§5.6) depend on
  static positions, so every decoder MUST compute them this way.
- **Starts within a byte.** The alignment rules of §2.1 and §2.2, and
  whether a narrow value's `byte_order` can matter, depend on the bits
  within a byte (0 to 7) where an entry can start: its *start residues*.
  They decide which definitions are valid, so every validator MUST
  compute them this way:
  - an entry with `offset_bits` starts at `offset_bits mod 8`; a
    container's first sequential entry at 0; any other sequential entry at
    every `(s + z) mod 8`, for a start residue `s` and a size residue `z`
    of the entry before it;
  - an entry's size residues are: for a field or padding of `n` bits,
    `n mod 8`; for strings, bytes and checksums, 0; for an array, 0 if its
    element size is whole bytes, else `c × e mod 8` for a `fixed` count
    `c` of elements of exact size `e`, every `k × e mod 8` for `k` from 0
    up to its largest count when the count varies, and every residue when
    the element size varies; for a struct, its type's exact size `mod 8`,
    else every residue where its last entry can end (where any of its
    entries can end, if one has `offset_bits`). An entry with `when` adds
    0 to its size residues;
  - a struct type's entries are computed from residue 0, so a struct MUST
    start on a byte when its type holds (directly, in an array or in a
    nested struct) a string, bytes, a little-endian value of whole bytes,
    a little-endian value narrower than a byte, or a value of at most 8
    bits that names its own `byte_order`.
- **Fields** read a value (§2.1, §2.2). A string or bytes field's length
  is `fixed` (with a `terminator` inside it, or not), taken from a field
  (§5.4), up to a `terminator` byte (which ends the value but isn't part
  of it; none before the packet's end is [`no_terminator`]), or the rest
  of the packet (`remaining`).
- **Structs** decode their type's entries from their start. A struct's
  size is the furthest end of its present entries, or zero if none is
  present.
- **Arrays** decode their elements one after another. The count is
  `fixed`, taken from a field (§5.4), or `remaining`: elements until the
  packet ends. A packet ends on a byte, so an array with a `remaining`
  count MUST start on a byte and have elements of whole bytes; bits left
  after its last element would otherwise be read as another.
- **Padding** skips its bits. It has no path: a packet that ends inside
  it is [`past_end`], named by its container (an array element's or a
  struct's path, or none at a packet's top level), and the error says
  it's padding.
- **Checksums** are verified (§5.6).

### 5.3 JSON entries

Entries have pointers relative to their container (the document, a
struct's object, or an array element).

- A field reads the value at its pointer: a `uint` or `int` field takes a
  number that is an integer (§2.3) within its type's 64-bit range
  [`out_of_range`]; a `float` field any number; `bool` and `string` fields
  their JSON type [`wrong_type`].
- A struct decodes its type's entries against the object at its pointer;
  an array, each element of the JSON array at its pointer [`wrong_type`].
- **Missing values.** If the pointer finds nothing, or `null`, an entry
  with `optional: true` is absent; any other entry is [`missing_value`].
- **`null` elements.** A `null` element of an array is no reading at that
  position: it keeps its index (so later elements keep theirs), and gives
  no sample.

### 5.4 References

Conditions, counts and lengths use the raw values (before any conversion)
of earlier fields of the same container. Validation keeps those fields
unconditional and not optional.

- `compare` and `in` compare a field with integers by value (a bool is 0
  or 1), or a string field with strings exactly. `mask_any` holds if
  `field & value` isn't 0; `mask_all` if it equals `value`.
- A count or length from a field is `raw * multiply + add` (defaults 1
  and 0). A negative result is [`bad_length`], named by the entry whose
  count or length it is.

### 5.5 Safety

A decoder MUST NOT allocate in proportion to a count or length before
checking it against the bits left in the packet. Before reading any
element of an array with a `fixed` or `field` count, the count is
checked against the bits left: as `count * size` bits for elements of
one size, else as `count` bits, since every element takes at least one.
A count past them is [`past_end`], named by the array. An element that
takes no bits is [`empty_element`]. Strings and bytes are checked before
they're copied. A decoder MUST NOT recurse deeper than the definition
nests (validation caps it at 32), and MUST NOT panic or crash on any
payload.

### 5.6 Checksums

A `checksum` entry sits at a packet's top level, starts on a byte
boundary, and holds an unsigned integer of its algorithm's width, in the
family's byte order unless the algorithm fixes its own, whatever byte
order the packet's fields name. It covers the
packet's bytes from `from_bits` (default 0, a byte boundary) up to its own
start. A decoder computes the checksum of those bytes; if the stored value
isn't one the algorithm accepts for it (the computed value, for all but
`iso8473`), the message is [`bad_checksum`].

A checksum at a static position (§5.2) is verified before any other entry
of the packet is extracted, so damage anywhere it covers is
`bad_checksum`, not whatever error the damaged entry would cause; those
checksums are verified in order of position. One that reaches past the
payload's end isn't a problem yet: the entries are then read in order,
and the first that reaches past the end is the error [`past_end`]. Any
other checksum is verified when the decoder reaches it.

- `crc`, with `width` (8 to 64 bits, whole bytes), `poly`, `init`,
  `reflect_in`, `reflect_out` and `xor_out`, the parameters of the CRC
  catalogue's model. A register of `width` bits starts at `init`. For each
  byte (with its bit order reversed if `reflect_in`), the byte is
  exclusive-ored into the register's top 8 bits; then, 8 times, the
  register shifts left one bit and, if the bit shifted out was 1, is
  exclusive-ored with `poly`. After the last byte, the register's bit
  order is reversed if `reflect_out`, and it is exclusive-ored with
  `xor_out`. For the bytes `123456789`, `width` 16, `poly` 0x1021, `init`
  0xFFFF, no reflection and `xor_out` 0 (CRC-16/IBM-3740), the result is
  0x29B1. Reflected, the register still starts at `init` as written: with
  `init` 0xB2AA, `reflect_in` and `reflect_out` and the rest the same
  (CRC-16/RIELLO), the result is 0x63D0.
- `sum`, with `width` (8 to 64 bits, whole bytes): the bytes added as
  unsigned integers, modulo 2^width.
- `xor`: the bytes exclusive-ored together, 8 bits.
- `iso8473`: the ISO checksum of ISO 8473-1 (Fletcher's checksum, modulo
  255) that ECSS PUS uses for packet error control, as ECSS-E-70-41A
  Annex A.2 specifies it. C0 and C1 start at 0; for each byte, C0 becomes
  C0 + byte and C1 becomes C1 + C0, modulo 255. The first check byte is
  −(C0 + C1) and the second is C1, modulo 255, each written as 255 where it
  would be 0. The two bytes are stored first then second, whatever the
  family's byte order, so as a 16-bit value the first is the high byte. A
  decoder accepts what the standard's decoding procedure accepts: not
  exactly one check byte 0, and C0 and C1 both 0 after the covered bytes
  and both check bytes. That is the computed value, and also 0x0000 when
  the computed value is 0xFFFF. For the bytes `AB CD EF 01` the check bytes
  are `9C F8`; for `00 00`, `FF FF`.

Names like `CRC-32` are a frontend convenience; an artifact always holds
the parameters.

## 6. Records

The decoded record lists each present entry by name, in definition order,
with its raw value: unsigned, signed, float, bool, string, bytes, array,
nested record, or null (§5.3). Padding and absent entries aren't listed.
Paths name entries: names joined by `.`, array elements by `[index]`, as
in `cells[2].volts`.

## 7. Meaning

### 7.1 Conversions

- `linear`: `raw * scale + offset`;
- `poly`: `c0 + c1·x + c2·x² + …`, evaluated by Horner's rule;
- `states`: the state whose `raw` equals the raw integer (a `raw` past
  ±(2^53 − 1) is written as a decimal string, and compares as its
  integer);
- `table`: through its points (below);
- `spline`: the natural cubic spline through its points (below).

An integer becomes the nearest binary64 value before any conversion but
`states`.

**Tables and splines.** Their points `(r₀, v₀) … (rₙ, vₙ)` are in strictly
increasing raw order; validation guarantees at least two. A raw value `x`:

- equal to a point's raw value is that point's value, exactly;
- otherwise between `r₀` and `rₙ`, falls in the segment from point `i` to
  point `i + 1` where `rᵢ < x < rᵢ₊₁`;
- otherwise, below `r₀` or above `rₙ`, is outside the points. Without
  `extrapolate: true` it has no engineering value (quality
  `out_of_calibration`, §7.3). With it, the first or last segment's formula
  continues past its end, as below.

NaN is never outside: it goes through as NaN (quality `non_finite`). An
infinity is outside, below `r₀` or above `rₙ`, but as a value read its
quality is `non_finite`, which ranks above `out_of_calibration` (§7.3).

A `table` with `linear` interpolation (the default) gives, in segment `i`
(and extrapolated, in the first or last segment),
`vᵢ + ((x − rᵢ) × (vᵢ₊₁ − vᵢ)) / (rᵢ₊₁ − rᵢ)`. With `step` interpolation it
gives `vᵢ`, the value of the last point below `x`; extrapolated, the
value of the nearest end point.

A `spline` gives the natural cubic spline: second derivatives `M₀ = Mₙ =
0` at the ends, and inside, with `hᵢ = rᵢ₊₁ − rᵢ` and `sᵢ = (vᵢ₊₁ −
vᵢ) / hᵢ`, the solution of `hᵢ₋₁Mᵢ₋₁ + 2(hᵢ₋₁ + hᵢ)Mᵢ + hᵢMᵢ₊₁ = 6(sᵢ −
sᵢ₋₁)` by this sweep, with `c₀ = d₀ = 0`:

- for `i` from 1 to `n − 1`: `p = 2 × (hᵢ₋₁ + hᵢ) − hᵢ₋₁ × cᵢ₋₁`,
  `cᵢ = hᵢ / p`, `dᵢ = (6 × (sᵢ − sᵢ₋₁) − hᵢ₋₁ × dᵢ₋₁) / p`;
- then `Mₙ₋₁ = dₙ₋₁`, and for `i` from `n − 2` down to 1: `Mᵢ = dᵢ − cᵢ ×
  Mᵢ₊₁`.

In segment `i`, with `L = rᵢ₊₁ − x` and `R = x − rᵢ`, the value is
`(Mᵢ × L × L × L + Mᵢ₊₁ × R × R × R) / (6 × hᵢ) + ((vᵢ − Mᵢ × hᵢ × hᵢ / 6) ×
L) / hᵢ + ((vᵢ₊₁ − Mᵢ₊₁ × hᵢ × hᵢ / 6) × R) / hᵢ`. Extrapolated, the
spline goes on straight at its end slopes: below `r₀`, `v₀ + (x − r₀) ×
(s₀ − h₀ × M₁ / 6)`; above `rₙ`, `vₙ + (x − rₙ) × (sₙ₋₁ + hₙ₋₁ × Mₙ₋₁ /
6)`. With two points, `M₀ = M₁ = 0` and the segment formula above gives a
straight line; it is still that formula, not the table's.

**Arithmetic.** Conversions use IEEE 754 binary64 arithmetic, each
operation rounded to nearest, ties to even, on its own and in the order
written: a linear conversion multiplies, then adds; Horner's rule
multiplies, then adds, coefficient by coefficient from the highest;
tables and splines evaluate their formulas with the usual precedence
(× and ÷ before + and −), parentheses first, and left to right among
operations of equal precedence: `Mᵢ × L × L × L` is
`((Mᵢ × L) × L) × L`. An
implementation MUST NOT fuse a multiply and an add into one operation,
reorder operations, or replace a division with a multiplication by a
reciprocal; any of these changes the last bit of some results.
`encoding.md` relies on the same rule.

### 7.2 Sentinels

A raw value that matches one of its entry's `invalid` values means "no
reading". The value's type picks the comparison: a string compares with
string sentinels exactly (so `"9999"` on a string field is text); a float
compares numerically with integer sentinels; an integer or bool compares
with integer sentinels by value (a decimal string is its integer).

### 7.3 Samples

Each present field or scalar array element mapped to a catalog param
(through any depth of structs, and through arrays of structs) gives one
sample, in record order; composites follow, in the order the set artifact
lists them, by canonical name. A sample is
`{param, value, path, index, time_offset, quality}`:

- `path` is the record path of the value, every index included; `index`
  is its position in its innermost array, for array elements.
- `time_offset`, for an array with `sample_interval_ms`: element *i* is
  taken *i* × interval after the packet's event time, in whole
  nanoseconds: `round(i × interval_ms × 1000000)`, evaluated in binary64
  from left to right, with ties rounding away from zero. Otherwise there
  is none.
- `quality`, from most to least trustworthy:

  | Quality | Meaning |
  | --- | --- |
  | `good` | |
  | `unknown_state` | no state matches the raw integer, or a string isn't one of the param's states |
  | `out_of_calibration` | the raw value is outside a table's or spline's points, and it doesn't extrapolate |
  | `out_of_range` | the param's type can't hold the raw value (a JSON integer past an i64 param) |
  | `non_finite` | NaN or an infinity, read or produced by a conversion or a unit conversion |
  | `invalid` | a sentinel (§7.2) |

  The first that applies, from the bottom up, is the quality. Any quality
  added later takes its place in this order.
- `value` is the param's value when the quality is `good`, and the raw
  value otherwise. By param type: `f64`, the number through its
  conversion, then from the field's unit into the param's (`units.md`
  §4); `i64` and `u64`, the integer; `bool`, a bool, or
  an integer that isn't 0; `str` and `bytes`, as read; `enum`, the state
  name from a `states` conversion, or a string field's text.

### 7.4 Composites

A composite gives one `vec3` (x, y, z) or `quat` (w, x, y, z) sample from
its members, numbers through their conversions and then into the param's
unit (a geodetic position's latitude and longitude into `deg`), in the
composite's frame.
If any member is absent (a conditional or optional entry, an array index
past a JSON array's end, a `null` element), there is no sample. Its
quality is that of its least trustworthy member; if that isn't `good`,
every member is its raw value. Its `path` is the members' paths joined
by `,`.

### 7.5 Engineering values

Any field or scalar element, mapped or not, has an engineering value: its
raw value through its conversion (a state name for `states`), with a
quality by the same rules, or its raw value if it has no conversion. It
is in the field's own unit, not a param's.
Implementations SHOULD compute these on request, not while decoding.

## 8. Event time

A set's `time` gives each message's event time, a UTC instant (or, for
`boot`, a time since boot):

- **none:** ground time only.
- **`cuc`:** a CCSDS unsegmented time code at `offset_bits`:
  `coarse_bytes` of whole seconds, then `fine_bytes` of binary fraction,
  both big-endian (as CCSDS defines them, whatever the family's byte
  order), counted from `epoch` on its `scale`. The fraction is truncated
  to whole nanoseconds: `floor(fine * 10^9 / 2^(8 * fine_bytes))`.
- **`elapsed`:** the packet's `time_field`, a count of `unit`s (`s`,
  `ms`, `us`, `ns`) since `epoch` on its `scale`. An integer count is
  exact; a float count is multiplied by the unit in nanoseconds in
  binary64 and rounded down to whole nanoseconds. Unix time is seconds
  since `1970-01-01T00:00:00Z` on the `utc` scale. A packet without a
  `time_field` has ground time only.
- **`gps`:** GPS time: the week in the packet's field named by the source's
  `week_field`, and the time of week, in `unit`s, in its `time_field`
  (converted as for `elapsed`), from `1980-01-06T00:00:00` on the `gps`
  scale. With `week_bits`, the week field's value is taken modulo
  2^`week_bits` and the full week is the one congruent to it nearest the
  message's ground time: with `G` the ground time's week (its GPS label,
  §8.1, less the epoch, divided by 604,800 s and rounded down), the week
  is `w + c × 2^week_bits`, where `c` is `floor((G − w + 2^(week_bits − 1))
  / 2^week_bits)` and not below 0. A packet without a `time_field` has
  ground time only.
- **`iso8601`:** the string at `pointer`, which MUST be an RFC 3339
  instant: any UTC offset from −23:59 to +23:59, `T`, `t` or a space
  between date and time, `Z` or `z`, and any number of fraction digits
  (truncated to nanoseconds). A seconds value of 60 is accepted in any
  minute and reads as the last nanosecond of second 59 of that minute, as
  Unix time has no leap seconds (`23:59:60Z` is `23:59:59.999999999Z`).
  The range check applies to the instant in UTC, not the written date:
  `0000-12-31T23:00:00-02:00` is `0001-01-01T01:00:00Z` and accepted,
  while `9999-12-31T23:59:59-01:00` is past year 9999 and refused.
- **`boot`:** the packet's `time_field`, time since the asset booted, in
  the time source's unit, returned as it is; a packet without a
  `time_field` has ground time only.

A `time_field` and a GPS week field are top-level fields that are
unconditional and not optional; validation refuses anything else
(`reference_type`). They, and a correlation's `clock_field` and
`boot_field`, are read raw, as counts: validation refuses a `conversion`
on them, and any `unit` but the one the source counts them in
(`reference_type`): a `time_field`'s and a `boot_field`'s the source's
`unit`, a `clock_field`'s the correlation's `clock_unit`, a GPS week
field's `wk`. Restating it lets a time field map to a param in another
unit (uptime in `ms` to a param in `s`) while event time reads the count.
A `time_field` or GPS week field that holds one of its sentinels (§7.2)
has no reading, so the packet has ground time only, whatever the other
field holds: the sentinel check comes first. By source, a `time_field` is a `uint` (`boot`); a
`uint`, `int` or `float` (`elapsed`); a `uint` or `float` (`gps`); a GPS
week field and a correlation's `clock_field` and `boot_field` are `uint`.
Every packet with a `time_field` in a `gps` set has the week field.

An event time that can't be read (bits past the end, no string at the
pointer, text that isn't an RFC 3339 instant, a count that isn't a
number, an instant outside years 0001 to 9999) is [`invalid_time`].

### 8.1 Time scales and leap seconds

A `cuc` or `elapsed` source, and a correlation (§8.2), states its `scale`:
`utc`, `tai`, `gps` or `tt` (`gps` time is always on the `gps` scale).
The epoch's date and time are read on that scale, whatever the `Z` its
RFC 3339 form ends with: `1958-01-01T00:00:00Z` on `tai` is 1958-01-01
00:00:00 TAI. The count is added to it, giving a *label*: nanoseconds
since 1970-01-01T00:00:00 on the scale, every day counted as 86,400
seconds.

- **`utc`:** the label is the Unix time of the instant. Like Unix time it
  has no leap seconds; a clock that counts them is on another scale.
- **`tai`:** International Atomic Time, which counts every second and so
  runs ahead of UTC by the leap seconds. With `A` the TAI − UTC in effect,
  the instant is the label less `A`. TAI − UTC is the offset of the last
  table entry whose UTC start is at or before the instant, which as a TAI
  label is the last entry whose start plus its offset is at or before the
  label. A label at or after an entry's start plus the previous entry's
  offset (the first entry's previous offset is its own, so no leap second
  precedes it) but before its start plus its own offset falls inside that
  entry's leap second (`23:59:60` UTC), and is the instant before the
  entry starts: `23:59:59.999999999`. An entry whose offset falls (a
  negative leap second) has no such interval.
- **`gps`:** GPS time, 19 seconds behind TAI: the label plus 19 s,
  converted as `tai`.
- **`tt`:** Terrestrial Time, 32.184 seconds ahead of TAI: the label less
  32.184 s, converted as `tai`.

Before the table's first entry, its offset (10 s) applies; after its last,
the last. MDL doesn't reproduce UTC before 1972-01-01, whose offsets were
fractional and drifting (and 0 at the 1958 TAI epoch): instants on `tai`,
`gps` or `tt` before then apply 10 s, and may differ from historical UTC
by up to 10 s. Implementations MUST use this table, which follows IERS
Bulletin C 72 (July 2026): no leap second at the end of 2026, and 37 s
from 2017 on. It is known to be right until **2027-07-01T00:00:00Z**, the
next date a leap second could be inserted; event times on `tai`, `gps` or
`tt` from then on use its last offset, and may differ by a second from a
newer table. A new IERS bulletin is a new version of this table.

| From (UTC) | TAI − UTC | From (UTC) | TAI − UTC |
| --- | --- | --- | --- |
| 1972-01-01 | 10 s | 1985-07-01 | 23 s |
| 1972-07-01 | 11 s | 1988-01-01 | 24 s |
| 1973-01-01 | 12 s | 1990-01-01 | 25 s |
| 1974-01-01 | 13 s | 1991-01-01 | 26 s |
| 1975-01-01 | 14 s | 1992-07-01 | 27 s |
| 1976-01-01 | 15 s | 1993-07-01 | 28 s |
| 1977-01-01 | 16 s | 1994-07-01 | 29 s |
| 1978-01-01 | 17 s | 1996-01-01 | 30 s |
| 1979-01-01 | 18 s | 1997-07-01 | 31 s |
| 1980-01-01 | 19 s | 1999-01-01 | 32 s |
| 1981-07-01 | 20 s | 2006-01-01 | 33 s |
| 1982-07-01 | 21 s | 2009-01-01 | 34 s |
| 1983-07-01 | 22 s | 2012-07-01 | 35 s |
| | | 2015-07-01 | 36 s |
| | | 2017-01-01 | 37 s |

Going the other way, for `gps` week resolution (§8), a UTC instant's
label on a scale is the instant plus the TAI − UTC in effect at it (less
19 s for `gps`, plus 32.184 s for `tt`).

### 8.2 Correlation

Turning boot time into an instant takes state across messages, which a
decoder doesn't keep. A consumer that does (MDL offers one) keeps, per
family and asset, the instant the asset booted, from the latest
`correlate` packet it has observed, in the order messages arrive: the
instant its `clock_field` gives, a count of `clock_unit`s since the
correlation's `epoch` on its `scale` (converted as §8.1; Unix time is
`1970-01-01T00:00:00Z` on `utc`), minus its `boot_field` (in the time
source's unit), both read at one moment. If either clock's quality isn't
`good`, the packet teaches nothing: its engineering quality (§7.5),
whether or not it's mapped, which for these unconverted fields is
`invalid` when it holds one of its sentinels and `good` otherwise. A
boot time's instant is the boot instant plus the boot time. Messages
before an asset's first correlation have no instant; those between a
reboot and the next correlation get the previous boot's. Boot instants
are ordinary instants, so a consumer can save and restore them across
restarts.

## 9. Error codes

| Code | Where |
| --- | --- |
| `wrong_encoding`, `invalid_json`, `unreadable_locator`, `unknown_asset`, `unknown_version`, `unknown_set`, `missing_set`, `unexpected_set` | routing (§3) |
| `no_packet` | identification (§4) |
| `too_short` | length (§5.1) |
| `past_end`, `no_terminator`, `invalid_text`, `empty_element`, `bad_length`, `missing_value`, `wrong_type`, `out_of_range`, `bad_checksum` | an entry (§5); the error names the entry's path |
| `invalid_time` | event time (§8) |
| `invalid_layout` | a definition that didn't pass validation; never for a validated one |
