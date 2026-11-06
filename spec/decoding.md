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
  doesn't apply within a byte).

A signed (`int`) value is the unsigned value read as two's complement in
its size. A `float` is IEEE 754 binary32 or binary64. A `bool` is one bit,
true if set. A read past the end of the packet is `past_end`.

### 2.2 Text and bytes

Strings and bytes start on a byte boundary and are a whole number of
bytes. Text is decoded strictly: UTF-8 MUST be valid, ASCII MUST be below
0x80, and Latin-1 maps each byte to the code point of the same value;
anything else is `invalid_text`. A fixed length keeps every byte, padding
included.

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
| `bits` | an unsigned integer at its position (§2.1), in the family's byte order |
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
   - for a family without one, the binding's set [`missing_set`];
5. the set is loaded [`unknown_set`].

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
- **Fields** read a value (§2.1, §2.2). A string or bytes field's length
  is `fixed`, taken from a field (§5.4), up to a `terminator` byte (which
  ends the value but isn't part of it; none before the packet's end is
  [`no_terminator`]), or the rest of the packet (`remaining`).
- **Structs** decode their type's entries from their start. A struct's
  size is the furthest end of its present entries, or zero if none is
  present.
- **Arrays** decode their elements one after another. The count is
  `fixed`, taken from a field (§5.4), or `remaining`: elements until the
  packet ends.
- **Padding** skips its bits.
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
  and 0). A negative result is [`bad_length`].

### 5.5 Safety

A decoder MUST NOT allocate in proportion to a count or length before
checking it against the bits left in the packet. Every array element
takes at least one bit, so a count is first checked as `count * size`
bits (or `count` bits, when elements vary in size), and an element that
takes no bits is [`empty_element`]. Strings and bytes are checked before
they're copied. A decoder MUST NOT recurse deeper than the definition
nests (validation caps it at 32), and MUST NOT panic or crash on any
payload.

### 5.6 Checksums

A `checksum` entry sits at a packet's top level, starts on a byte
boundary, and holds an unsigned integer of its algorithm's width, in the
family's byte order unless the algorithm fixes its own. It covers the
packet's bytes from `from_bits` (default 0, a byte boundary) up to its own
start. A decoder computes the checksum of those bytes; if the stored value
isn't one the algorithm accepts for it (the computed value, for all but
`iso8473`), the message is [`bad_checksum`].

A checksum at a static position (§5.2) is verified before any other entry
of the packet is extracted, so damage anywhere it covers is
`bad_checksum`, not whatever error the damaged entry would cause. Any
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
  0x29B1.
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
- `states`: the state whose `raw` equals the raw integer.

An integer becomes the nearest binary64 value before a linear or
polynomial conversion.

### 7.2 Sentinels

A raw value that matches one of its entry's `invalid` values means "no
reading". The value's type picks the comparison: a string compares with
string sentinels exactly (so `"9999"` on a string field is text); a float
compares numerically with integer sentinels; an integer or bool compares
with integer sentinels by value (a decimal string is its integer).

### 7.3 Samples

Each present field or scalar array element mapped to a catalog param
(through any depth of structs, and through arrays of structs) gives one
sample, in record order; composites follow, in definition order. A sample
is `{param, value, path, index, time_offset, quality}`:

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
  | `out_of_range` | the param's type can't hold the raw value (a JSON integer past an i64 param) |
  | `non_finite` | NaN or an infinity, read or produced by a conversion |
  | `invalid` | a sentinel (§7.2) |

  The first that applies, from the bottom up, is the quality. Any quality
  added later takes its place in this order.
- `value` is the param's value when the quality is `good`, and the raw
  value otherwise. By param type: `f64`, the number through its linear or
  polynomial conversion; `i64` and `u64`, the integer; `bool`, a bool, or
  an integer that isn't 0; `str` and `bytes`, as read; `enum`, the state
  name from a `states` conversion, or a string field's text.

### 7.4 Composites

A composite gives one `vec3` (x, y, z) or `quat` (w, x, y, z) sample from
its members, numbers through their conversions, in the composite's frame.
If any member is absent (a conditional or optional entry, an array index
past a JSON array's end, a `null` element), there is no sample. Its
quality is that of its least trustworthy member; if that isn't `good`,
every member is its raw value. Its `path` is the members' paths joined
by `,`.

### 7.5 Engineering values

Any field or scalar element, mapped or not, has an engineering value: its
raw value through its conversion (a state name for `states`), with a
quality by the same rules, or its raw value if it has no conversion.
Implementations SHOULD compute these on request, not while decoding.

## 8. Event time

A set's `time` gives each message's event time:

- **none:** ground time only.
- **`cuc`:** a CCSDS unsegmented time code at `offset_bits`:
  `coarse_bytes` of whole seconds, then `fine_bytes` of binary fraction,
  both big-endian (as CCSDS defines them, whatever the family's byte
  order), counted from `epoch`. The fraction is truncated to whole
  nanoseconds: `floor(fine * 10^9 / 2^(8 * fine_bytes))`.
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

A `time_field`, and a correlation's `unix_field` and `boot_field`, are
top-level fields that are unconditional, not optional, and `uint`;
validation refuses anything else (`reference_type`).

**Time scale.** Event times, like Unix time, have no leap seconds, and a
CUC code's seconds are added to its epoch as such. A mission clock that
counts TAI seconds (for example, since 1958) therefore comes out ahead of
UTC by the leap seconds since its epoch: 37 seconds for any instant since
2017. A time scale setting may come in a later version.

An event time that can't be read (bits past the end, no string at the
pointer, text that isn't an RFC 3339 instant, an instant outside years
0001 to 9999) is [`invalid_time`].

### 8.1 Correlation

Turning boot time into an instant takes state across messages, which a
decoder doesn't keep. A consumer that does (MDL offers one) keeps, per
family and asset, the instant the asset booted, from the latest
`correlate` packet: its `unix_field` (in `unix_unit`) minus its
`boot_field` (in the time source's unit), both read at one moment. If
either clock's quality (§7.3) isn't `good`, the packet teaches nothing. A
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
