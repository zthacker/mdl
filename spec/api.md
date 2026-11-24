# The runtime API

Status: normative for bindings. The key words MUST, MUST NOT, SHOULD and
MAY are to be interpreted as described in RFC 2119.

Every binding of MDL to another language calls one runtime, this
repository's, through one coarse API: open a store, decode messages,
encode commands and decode them back, encode telemetry, describe the
build, look up which asset a native id is, and turn an event time into an
instant. A binding
converts its language's inputs for the runtime and the runtime's results
into its language's values, and does nothing else. No MDL semantics live
in a binding, so a feature reaches every language when the runtime gains
it, and no second implementation has to be kept equal to this one.

This document specifies the operations, their inputs, and their results,
in JSON and as the values a binding returns. The `mdl` crate's `runtime`
module implements it; `morphc decode` and `morphc encode` write the same
results in their JSON reports. The result format's version is 1
([compatibility](compatibility.md) §4 says what changes it). A binding
MUST pass the decoding, encoding, command decoding and instant cases of
[conformance](conformance.md) §2 to §5 through its own API.

## 1. Values

Results are made of these values. Each has one JSON spelling, and a
binding returns its language's own value for it.

| Value | JSON | In a binding |
| --- | --- | --- |
| integer | a number within ±(2^53 − 1); a decimal string past it (`"18446744073709551615"`), which every JSON reader keeps exact | an integer, of any width the value needs |
| float | a number, the shortest text that reads back as the same double; NaN and the infinities as `{"non_finite": "NaN"}`, `{"non_finite": "inf"}` and `{"non_finite": "-inf"}`, which no other value is mistaken for | a float. A language whose floats have no NaN or infinities returns the values its binding names (Elixir: the atoms `:nan`, `:infinity` and `:neg_infinity`) |
| boolean | `true` or `false` | a boolean |
| text | a string | a string |
| bytes | a string of lowercase hex digits, two a byte (`"c0ff"`) | a byte string (Elixir: a binary) |
| instant | a string spelled as `bindings.md` §2 spells instants: UTC, exact to the nanosecond | an integer: nanoseconds since 1970-01-01T00:00:00Z, every day counted as 86,400 seconds (the label of `decoding.md` §8.1 on the `utc` scale). Elixir's `DateTime` holds microseconds, so the binding returns the integer |
| absent | `null` | the language's absent value (Elixir: `nil`) |
| list | an array | a list |
| object | an object; its keys' order means nothing | a map, keyed by text (Elixir: string keys, not atoms, so input can't exhaust the atom table); a result's fixed shape may take the language's own form instead, as below |

Text, bytes and an enum's state are all strings in JSON; a consumer tells
them apart by the param's or field's type, which the catalog and the set
give. A float is never written in the integer form, and an integer never
in the float form, whatever its value. An object always has every key this
document lists for it; a value that doesn't apply is the absent value, not
a missing key.

A binding MAY give a result's fixed shape (an object whose keys §3 or §4
lists) its language's own form, such as a struct whose fields are those
keys, and MDL's own enumerations its language's symbols: a quality, a
frame, an error's or a problem's code, an event time's kind, a time unit.
Each of these is a fixed set in a runtime release (a release may add to
them, as [compatibility](compatibility.md) §4 and §5 allow), so making
them symbols can't grow without bound. Everything else that is text stays
text: names, paths, units, states, metadata keys, and the keys of an
object that comes from definitions or messages (a record, metadata), so
no input can grow a table of symbols its language never frees. A binding
documents its forms. The Elixir binding's (`bindings/elixir`) are structs
with atom fields (`%MDL.Decoded{}`, `%MDL.Sample{}`, `%MDL.Encoded{}`,
`%MDL.DecodedCommand{}`, `%MDL.Route{}`, `%MDL.Error{}`, `%MDL.Problem{}`,
and `%MDL.Vec3{}` and `%MDL.Quat{}` for a vector's or quaternion's
value); atoms for the
enumerations, a frame's being its IR name in lower case (`:eci_j2000`);
an event time as `{:ground, ns}`, `{:at, ns}` or `{:since_boot, count,
unit}`; and an engineering value as `{path, value, quality}`. Elixir's
text and bytes are both binaries, so a bytes argument to encode is
`{:bytes, binary}`.

A native id or a metadata value is an integer or text. In JSON an integer
past ±(2^53 − 1) is a string too, so a string that is an integer's one
decimal spelling (no sign but a leading `-`, no leading zeros) within
−2^63 to 2^64 − 1 is that integer, as `bindings.md` §3 reads native ids;
any other string is text.

A native id is never empty: empty text names no asset. An alias that
reads it is `unreadable_locator` (`decoding.md` §3.1), encoding takes it
as no target (`missing_target`, `encoding.md` §3.3), and `binding` and
`asset` find no asset for it (§2.6). A decoded command reports a target
field's text as it is, empty or not (§3.5), as the payload holds it.

## 2. Operations

### 2.1 open

Inputs: a store, and optionally bindings.

- A store is a directory laid out as `store.md` §1 says, or the same
  files as bytes: the manifest's, and each artifact's. Artifacts given as
  bytes are found by their own identity (`canonical-json.md` §3), which
  the runtime computes, so they need no names. Either way, the result is
  the same.
- Bindings are the text of a bindings file (`bindings.md`).

The runtime reads the build the manifest names, as `store.md` §3 says:
the catalog, every family and set the manifest names, and the older
catalog each set pins when it isn't the manifest's, each artifact checked
against its digest and read as `canonical-json.md` says. Artifacts the
manifest doesn't need are never read, from a directory or from bytes, so
one of a newer version beside the build doesn't stop it. It then puts
them together as one build: each family with its sets, each set validated
against its family and the catalog it pins, by this release's rules
([compatibility](compatibility.md) §7).

Bindings, when given, are read, and bindings that can't be (not TOML, an
unsupported version, a row of the wrong shape) refuse the open. They're a
fleet's, and a store may carry some of its families, so rows for families
the build doesn't have are left out without a word. Anything else they say
that the build doesn't fit (a set it doesn't have, a set named for a
family whose version locator chooses) is a warning on the handle, a code,
a message and a path each, as §4's problems are: the open succeeds, and a
message those rows affect is refused when decoded, with its own code
(`unknown_set`, `unexpected_set`, `missing_set`). So a bindings change
can be deployed before the store that carries it. `morphc` checks bindings
against a build in full (`bindings.md` §6), which is where CI catches
them.

The result is a handle, with its warnings, or an error (§4).

A handle is immutable. Any number of threads MAY use one at once, and a
binding SHOULD let any number of processes share one. A handle holds
everything it needs and doesn't read the store again; opening the store
again gives a new handle, and an older handle keeps working, on the build
it opened, until it's dropped. A handle with other bindings MAY share the
build it was opened with, without assembling it again.

### 2.2 decode

Inputs: a handle, the name of the family that sent the message, and the
message:

- payload: the bytes for a binary family; the JSON text, as UTF-8 bytes,
  for a JSON family;
- metadata: what the link delivers with the payload, by name, each value
  an integer or text (`decoding.md` §1);
- ground time: an instant, when the ground received it.

A store holds many families, and routing starts from one family's alias
and identify keys, so the family is an input: a link knows which family
it carries.

The result is a decoded message (§3.1), or an error (§4).

Without bindings, each native id is an asset of its own, named by the
native id's text, and the decoded message says so (`stand_in`). Such an
asset is decoded by the family's set when the family has one set, or by
the set the family's version locator chooses (`decoding.md` §3). In a
family with several sets and no version locator, an asset's set comes
from its binding, so without bindings its messages are `missing_set` once
routing reaches the set: a payload or an alias that can't be read is that
error first, as `decoding.md` §3.3 orders the checks.

### 2.3 decode_many

Inputs: a handle, a family, and messages. The result is one result per
message, in order, each a decoded message or an error: one message's
error doesn't stop the others.

### 2.4 encode

Inputs: a handle, a family, and a command request: the set, the catalog
command, its arguments by name, and optionally the native id of the
target (`encoding.md` §1). Argument values are those of §1: an integer, a
float (NaN and the infinities included, which encoding refuses with
`non_finite`), a boolean, text, bytes, or a list of them.

The result is an encoded command (§3.2), or an error (§4).

### 2.5 describe

Inputs: a handle. The result is a description of the build it opened
(§3.3): the catalog, each family and each set, with their digests, and
where each entry of a binary set can be. The same store and runtime
release give the same description: the IR parts change only with the
manifest, and a release may compute a layout's bounds more tightly.

### 2.6 binding and asset

`binding` takes a handle, a family, a native id and an instant. Its result
is what the native id is at the instant (§3.4), resolved as decoding a
message received then resolves it (`bindings.md` §4): its binding, or,
without bindings, the stand-in §2.2 describes (`missing_set` where there
can't be one). Its set is checked as routing checks it (`decoding.md`
§3), with the same codes, so a lookup and a message's decoding agree: a
set named for a family whose version locator chooses is
`unexpected_set`, no set for a family without one `missing_set`, a set
the build doesn't have `unknown_set`. It's absent when the native id is
no asset then: no row binds it yet, or its row is retired, or it's empty
(§1), with bindings or without, and then its set isn't checked, as
decoding refuses an alias that reads it before it reaches the set.

`asset` takes a handle, a family, an asset's name and an instant, and
looks the other way: its result is what the native id that is the asset
at the instant is (§3.4), which is where a command to the asset goes,
its set checked as `binding`'s is. It's absent when no native id is the
asset then. A registry may bind one asset to several native ids at once;
then there's no one destination, and the result is `ambiguous_asset`.
Without bindings, the asset is the native id its name spells
(`bindings.md` §3), a stand-in. An empty name is no asset, with bindings
or without, its set not checked: no row names one, and no native id is
empty (§1).

Encoding takes the set and the native id (§2.4), which `asset` gives for a
family without a version locator. A family with one picks each message's
set by its version, so no binding says which set a command to an asset
uses: a ground system takes it from what it knows of the asset, such as
the set of its latest decoded telemetry (§3.1's `set`). MDL has no rule
of its own for it.

### 2.7 decode_command

Inputs: a handle, a family, the set that decodes the payload, by name,
and the message: its payload, in the family's encoding as for decode
(§2.2), and its metadata, which a command packet's identity includes
(`encoding.md` §7). A ground time takes no part, so a binding needn't
ask for one.

Decoding a command is what a simulated asset does with a command it
receives, and what command history records: the payload back into its
catalog command and arguments, by `encoding.md` §7. The set is the
caller's to name, as for encode. The result is a decoded command (§3.5),
or an error (§4).

### 2.8 instant

Inputs: an event time, in §3.1's form, and a boot instant or none. It
takes no handle: it needs nothing from a store.

The result is the instant the event time is, or absent:

- `at`: its `time`;
- `since_boot`, with a boot instant: the boot instant plus `value`, a
  count from 0 to 2^64 − 1, in `unit` (`s`, `ms`, `us` or `ns`), counted
  exactly, if that's within years 0001 to 9999; absent when it isn't, and
  when there's no boot instant;
- `ground`: absent. The message carries no time of its own; its ground
  time says when it was received, which the caller already has.

Correlating is the caller's (§3.1): the boot instant is the latest
`booted` it has seen for the message's family and asset, the message's
own included. `instant` is pure, so the same inputs always give the same
result, and it has no errors: an input that isn't an event time or an
instant is the binding's to refuse, in its own way (Elixir:
`ArgumentError`). A count is exact however large: `value` may be up to
2^64 − 1, whose instant in seconds is past what 64 bits of nanoseconds
hold.

### 2.9 encode_telemetry

Inputs: a handle, a family, and a telemetry request (`encoding.md` §9.1):
the set and one of its packets, by name; the native id of the asset that
sends it; params' values by name, each a list of values in §3.1's sample
forms, one for each sample decoding gives the param from the message, in
record order; and raw values by entry name, in §3.1's record form.

Encoding telemetry is what a simulated asset does to send telemetry: the
values into the payload, and the metadata a link frames it with, that
decoding reads back as those values (`encoding.md` §9). The set is the
caller's to name, as for encode. The result is an encoded telemetry
packet (§3.6), or an error (§4).

## 3. Results

### 3.1 A decoded message

An object with these keys:

| Key | Value |
| --- | --- |
| `family` | text |
| `native_id` | integer or text: the native id the family's alias locator read, read as §1 reads one, so the string `"7"` at a `path` locator's pointer is the integer 7 |
| `asset` | text: the asset the native id is at the ground time |
| `stand_in` | boolean: true when there are no bindings, and the native id stands for an asset of its own (§2.2) |
| `set` | text: the set that decoded it |
| `def_version` | text: the set artifact's digest |
| `packet` | text |
| `event_time` | an event time, below |
| `booted` | an instant: when the asset booted, which the message teaches as its set's correlation packet (`decoding.md` §8.2); absent for any other message, and for a correlation packet that teaches nothing |
| `samples` | a list of samples, below |
| `record` | the raw record, below |
| `engineering` | a list of engineering values, below |

**Event time** (`decoding.md` §8): an object whose `kind` is one of

- `ground`: the set's time source gives no time for the packet; `time` is
  the message's ground time, an instant;
- `at`: `time` is the instant the time source gives;
- `since_boot`: `value`, an integer from 0 to 2^64 − 1, in `unit` (`s`,
  `ms`, `us` or `ns`): time since the asset booted, which isn't an instant
  until correlated (`decoding.md` §8.2).

Correlating takes state across messages, which the runtime doesn't keep:
`booted` is what one message teaches, and a consumer keeps, per family
and asset, the latest `booted` it has seen, a message's own included, in
the order messages arrive. A `since_boot` event time's instant is then
that boot instant plus its `value` in its `unit`, when that's within
years 0001 to 9999, as `instant` gives it (§2.8).

**Sample** (`decoding.md` §7.3): an object with

| Key | Value |
| --- | --- |
| `param` | text: the catalog param |
| `value` | the value, below |
| `unit` | text: the param's unit; absent when it has none |
| `path` | text: the entry it came from, as a record path (`batt.volts`, `samples[3]`); for a composite, its members' paths joined by `,` |
| `index` | integer: its position in its array; absent for a value not from an array |
| `time_offset_ns` | integer: how long after the packet's time it was taken; absent when the block has no sample interval |
| `quality` | text: `good`, `invalid`, `unknown_state`, `out_of_calibration`, `out_of_range` or `non_finite` |

A value whose quality is `good` is in the param's type and unit: a float
(`f64`), an integer (`i64`, `u64`), a boolean, text (`str`), bytes, the
state's name, text (`enum`), and for `vec3` and `quat` an object of the
frame's name (`frame`) and floats (`x`, `y`, `z`; `w`, `x`, `y`, `z` for
a quaternion, scalar first), in the frame of the entries it came from.
Any other quality's value is the raw value, as in the record, but for a
composite, whose value is always its frame and floats: each member is
then its raw value as a float, unconverted (`decoding.md` §7.4). Check
the quality before storing or plotting a value.

**Record** (`decoding.md` §6): an object of each entry that's present, by
name, in the packet's order: a field's raw value (an integer for `uint`
and `int`, a float, a boolean, text for `string`, bytes), an array's
elements as a list (a JSON array's `null` element as absent), and a
struct's or an array record's entries as an object of the same kind. An
entry a condition leaves out isn't there, and padding has no value.

**Engineering value** (`decoding.md` §7.5): an object with `path`, `value`
and `quality`, for each field and scalar element, mapped or not, in the
record's order; a JSON array's `null` element, which is no reading, has
none. A value is the field's raw value through its conversion, in the
field's unit (not the param's, which a sample's is), and spelled as a
sample's: a float, an integer, a boolean, text, bytes or a state's name,
or the raw value when the quality isn't `good`.

### 3.2 An encoded command

An object with these keys:

| Key | Value |
| --- | --- |
| `family` | text |
| `set` | text |
| `def_version` | text: the set artifact's digest, to keep with what was sent |
| `packet` | text |
| `command` | text |
| `payload` | bytes: what the link sends; a JSON family's payload is its UTF-8 text |
| `metadata` | an object of the values the link frames the payload with, by name, each an integer or text (`encoding.md` §7.1) |
| `defaulted` | a list of text: the arguments the request left out, which took their defaults, in the catalog's order |
| `deprecated` | text: the catalog's deprecation note; absent when the command isn't deprecated |
| `record` | the raw record of what was written: each field's and checksum's raw value, as decoding the payload gives them |

### 3.3 A description

An object with these keys:

| Key | Value |
| --- | --- |
| `format` | integer: the result format's version (§1) |
| `catalog` | an object: `digest`, text, the catalog artifact's digest; and `catalog`, the catalog |
| `families` | a list, in name order, of objects: `name`, text; `digest`, text; `family`, the family; and `sets`, a list of its sets' names, text, in name order |
| `sets` | a list, in name order, of objects: `name`, text; `family`, text; `def_version`, text, the set artifact's digest; `pins_catalog`, text, the digest of the older catalog the set pins, absent when it pins the manifest's; `set`, the set; and `layout`, below, absent for a self-describing (JSON) family |

The catalog, a family and a set are the IR's own JSON values for them, as
their artifacts hold them, read by the IR's specifications. They aren't
converted by §1's rules: an integer the IR writes as a decimal string
(past ±(2^53 − 1)) stays that string, in JSON and in a binding. MDL
specifies the IR once; a second rendering of it, value by value, could
come to mean something else. Each value inside them has the IR's type,
not §1's: a range end is a number or a decimal string, a bytes default is
hex text, and a number keeps its field's type, so an `f64` written `10` in
an artifact is the float 10.0. They're the same values as the artifacts
hold, not the same spelling, and a binding returns them as plain maps,
lists and scalars, without §1's integer rule inside them.

**Layout** (`decoding.md` §5.2): where each entry of a binary set can be,
in bits from its container's start, as §1's integers. An object of
`packets` and `commands`, lists in the set's order, and `types`, an object
by struct type name, each of a container: `name`; `entries`, a list with
one placement for each of the container's entries in the IR, in the same
order, padding and checksums included; `size_min_bits`, the fewest bits its
entries take; and `size_max_bits`, the most, absent when unbounded. A
placement is an object of

| Key | Value |
| --- | --- |
| `static_offset_bits` | integer: its static position (`decoding.md` §5.2), absent when a message can move it |
| `start_min_bits` | integer: the earliest bit it can start at |
| `start_max_bits` | integer: the latest, absent when earlier data can move it without bound |
| `size_min_bits` | integer: its smallest size when present |
| `size_max_bits` | integer: its largest, absent when unbounded |
| `conditional` | boolean: a condition can leave it out |

A struct type's positions count from the struct's own start, wherever it's
read.

### 3.4 A binding

An object with these keys:

| Key | Value |
| --- | --- |
| `family` | text |
| `native_id` | integer or text (§1) |
| `asset` | text |
| `set` | text: the set that decodes the asset's messages, absent when the family's version locator chooses |
| `stand_in` | boolean: true when there are no bindings, and the native id stands for an asset of its own (§2.2) |

### 3.5 A decoded command

An object with these keys:

| Key | Value |
| --- | --- |
| `family` | text |
| `set` | text |
| `def_version` | text: the set artifact's digest |
| `packet` | text: the command packet the payload is |
| `command` | text: the catalog command |
| `arguments` | an object of every argument of the command, by name: each value in its argument's type (`encoding.md` §7.4), an integer, a float, a boolean, text (`str`, and an `enum`'s state), bytes, or a list of them for a list argument |
| `defaulted` | a list of text: the arguments the set doesn't encode, which have their defaults, in the catalog's order |
| `target` | integer or text (§1): the target's native id, for a packet that holds it, or a string field's empty text as it is (`encoding.md` §7.4); absent otherwise |
| `record` | the raw record of the payload: each field's and checksum's raw value, as decoding it gives them |

For a payload an encoder wrote (`encoding.md` §1 to §6), the arguments,
encoded again with the same set and target, give the same payload
wherever `encoding.md` §7.4 says they do. A payload written otherwise
(bytes past its last entry, another JSON spelling or other padding, for
example) decodes, and its arguments encode again as an encoder writes
them, unless its target field holds empty text: that names no target to
encode them with (§1), so encoding it is `missing_target`.

### 3.6 An encoded telemetry packet

An object with these keys:

| Key | Value |
| --- | --- |
| `family` | text |
| `set` | text |
| `def_version` | text: the set artifact's digest, which decoding the payload stamps |
| `packet` | text |
| `native_id` | integer or text (§1): the native id that sends it |
| `payload` | bytes: what the link sends; a JSON family's payload is its UTF-8 text |
| `metadata` | an object of the values the link frames the payload with, by name, each an integer or text: the family's locators and the packet's identify keys that read metadata (`encoding.md` §9.3) |
| `record` | the raw record of the payload: each entry's raw value, as decoding it gives them |

## 4. Errors

An error is an object with a `code`, which is stable and names the
problem, a `message`, which is for people and may change, and `problems`,
a list. An error that stands for several problems (a build that doesn't
validate, bindings with problems on several rows) lists each as an object
with its own `code`, `message` and `path`, and its message has them all;
any other error's list is empty. A path is a JSON Pointer, empty for the
whole: into the bindings file for its problems, and otherwise into the
build as one bundle, its catalog, families and sets (each list in name
order). A problem found putting one family together with an older catalog
a set pins (in a store `morphc` didn't build) points into that family's
own bundle: the catalog, the family and its sets. A problem's message also
names where it is, for people. A binding returns errors as values,
never by unwinding across the binding's boundary (Elixir:
`{:error, %MDL.Error{}}`, its code an atom).

The codes are those `decoding.md` §9 and `encoding.md` §8 define for
decoding, encoding and decoding commands, and these:

| Code | Operation | Meaning |
| --- | --- | --- |
| `unknown_family` | decode, encode, decode_command, encode_telemetry, binding, asset | the build has no family of that name |
| `ambiguous_asset` | asset | several native ids are the asset at the instant |
| `invalid_layout` | describe | a set validated, but its layout can't be worked out: a bug in MDL |
| `store_unreadable` | open | a file of the store can't be read, or isn't there |
| `manifest_version` | open | the manifest's format version isn't one this runtime reads |
| `bad_manifest` | open | the manifest isn't one: not JSON, a key that isn't the format's, a value of the wrong kind |
| `wrong_digest` | open | an artifact's identity isn't the digest it's stored under |
| `wrong_artifact` | open | an artifact named as a catalog, a family or a set isn't one, or isn't the one of that name |
| `missing_artifact` | open | a set pins an artifact the store doesn't give |
| `duplicate_artifact` | open | two artifacts of one kind have one name |
| `duplicate_version` | open | two sets of one family have one version |
| `incompatible_catalog` | open | a set pins an older catalog the manifest's catalog isn't compatible with; its problems say how |
| `invalid_build` | open, describe | the build doesn't validate, by this release's rules; its problems are validation's diagnostics. From describe, the build's parts and its artifacts disagree: a bug in MDL |
| `invalid_bindings` | open | the bindings have rows that aren't valid in themselves (`bindings.md` §7); its problems are the rows' |

An artifact that can't be read is the read's own error (`wrong_shape`,
`missing_version`, `invalid_version`, `unsupported_version`,
`not_normal_form` or `feature_version`, as `canonical-json.md` and
[compatibility](compatibility.md) describe), and its message names the
file. Bindings that aren't TOML, or state an unsupported version, are
the bindings file's whole-file errors (`invalid_toml`, and the read codes
above).
