# Conformance

Status: normative. The key words MUST, MUST NOT, SHOULD and MAY are to be
interpreted as described in RFC 2119.

The other specifications say what MDL does in prose. The conformance
suites say some of it again as examples a program checks: language-neutral
JSON files under `conformance/`, each worked out from the specifications.
An implementation conforms when it passes every file of each suite that
covers what it implements. A binding of the runtime API
([api.md](api.md)) runs them through its own API, so they check its
inputs and its language's values as well as the runtime beneath it.

## 1. Suites

| Directory | What it checks | Specified in |
| --- | --- | --- |
| `conformance/canonical/` | canonical JSON: number text, canonical form, the input rules | [canonical-json.md](canonical-json.md) §4 |
| `conformance/decoding/` | decoding messages through the runtime API | §2 |
| `conformance/encoding/` | encoding commands through the runtime API | §3 |
| `conformance/decoding_commands/` | decoding command payloads through the runtime API | §4 |

## 2. Decoding cases

Each file in `conformance/decoding/`, or in a directory under it, is a
JSON object with these keys:

| Key | Value |
| --- | --- |
| `about` | text: what the file covers, for people |
| `definitions` | one IR bundle, as the JSON frontend reads it ([frontends.md](frontends.md) §6) |
| `bindings` | optional: a bindings file ([bindings.md](bindings.md)), as a list of its lines; the file is each line followed by a line feed |
| `warnings` | optional: the warnings opening the store with the bindings gives (api.md §2.1), a list of objects with a `code` and a `path` |
| `cases` | a list of cases |

A case is an object with these keys:

| Key | Value |
| --- | --- |
| `name` | text, unique in its file |
| `spec` | a list of the sections its expected result follows from, each a file and a section number (`"decoding.md §5.6"`) |
| `note` | text: how the expected result follows from them, for people |
| `family` | text: the family that sent the message |
| `message` | an object: the payload, as `hex` (lowercase hex digits, two a byte, with spaces between bytes allowed), for any family, or as `json` (a JSON family's document, as its text, when it is UTF-8 text); `metadata`, an object of values, each an integer or text read as [api.md](api.md) §1 reads them (a string in an integer's one decimal spelling within −2^63 to 2^64 − 1 is that integer); and `ground_time`, an instant (api.md §1) |
| `decoded` | the decoded message (api.md §3.1), in its JSON, without `def_version` |
| `error` | text: the error's code |

Every key is required but `bindings` and `warnings`; a case has one of
`decoded` and `error`, and a message one of `hex` and `json`. A file,
case, message or warning with a key this section doesn't list, or
without one it requires, is malformed, so a misspelt key fails rather
than being skipped.

### 2.1 Running a file

A runner:

1. builds the definitions into a store, as `morphc build` builds one
   from a JSON input that holds them ([frontends.md](frontends.md) §6,
   [store.md](store.md)): a runner that can't call MDL's library writes
   them to a `.json` file and runs `morphc build` on it. A file whose
   definitions don't build fails as a whole;
2. opens the store (api.md §2.1) with the file's bindings, if it has any,
   and compares the open's warnings with the file's: the same codes and
   paths, in any order, each as many times. A file without `warnings`
   opens with none;
3. decodes each case's message (api.md §2.2) from its family, with the
   payload's bytes, its metadata and its ground time, and compares the
   result with the case's (§2.2);
4. decodes each family's messages again, in the file's order, with one
   `decode_many` (api.md §2.3), whose results MUST compare equal to the
   cases' in the same way.

A decoded message's `def_version` MUST be the digest that `describe`
(api.md §2.5) gives for its set. No person works out a SHA-256, so a case
leaves it out; `conformance/canonical/` checks the text it's computed
from. An error compares equal when its code is the case's and its list of
problems is empty. Its message is for people, and isn't compared.

### 2.2 Comparing results

A result and a case's compare as JSON values, in api.md §1's spellings:

- objects have the same keys, in any order, with values that compare
  equal under each;
- arrays have the same length, and elements that compare equal in order;
- strings, booleans and `null` compare equal when they're identical;
- a number with a fraction or an exponent is a float, and any other is an
  integer. An integer compares equal only to an integer of the same value,
  and a float only to a float that reads as the same binary64, bit for bit,
  so `-0.0` isn't `0.0`.

A binding compares its language's values with the ones the case's JSON
stands for, under api.md §1 and the forms it documents, as strictly: an
integer never equals a float, and its NaN equals NaN.

### 2.3 Expected results

No expected result is a decoder's output. They come from one of two
places, and when a case and an implementation disagree, the
specifications settle it: the case is wrong, or the implementation has a
bug.

- The files directly in `conformance/decoding/` are worked out from the
  specifications by hand. Each case's `note` shows how, and it cites the
  narrowest sections its result depends on.
- The files in `conformance/decoding/oracle/` are generated from
  random layouts by the testkit's oracle (`crates/mdl-testkit`). The
  oracle doesn't decode: it builds each message from values it chose and
  works forward from the specifications to the result, or to the error a
  damaged message gives, in code separate from MDL's decoder, and the
  testkit spells the result as [api.md](api.md) §1 and §3.1 do. It has
  the same authors as MDL, though, and the property tests run MDL against
  it over many more layouts, so the two agree by construction: a
  misreading common to both would be in these cases too. The hand-written
  cases, and other implementations, are the check on that. Each case's
  `note` names the layout and message it came from, and what was planted
  in a damaged one. `just conformance` writes them from fixed seeds.
  They're a snapshot: they're regenerated on purpose, the diff reviewed
  and the runners run again, not checked against the generator.

## 3. Encoding cases

Each file in `conformance/encoding/`, or in a directory under it, is a
JSON object with these keys:

| Key | Value |
| --- | --- |
| `about` | text: what the file covers, for people |
| `definitions` | one IR bundle, as the JSON frontend reads it ([frontends.md](frontends.md) §6) |
| `cases` | a list of cases |

A case is an object with these keys:

| Key | Value |
| --- | --- |
| `name` | text, unique in its file |
| `spec` | a list of the sections its expected result follows from, as in §2 (`"encoding.md §3.1"`) |
| `note` | text: how the expected result follows from them, for people |
| `family` | text: the family whose set encodes the command |
| `request` | an object: `set` and `command`, text; `arguments`, an object of the arguments given, by name, each value tagged (below); and `native_id`, the target, an integer or text read as [api.md](api.md) §1 reads a native id |
| `encoded` | the encoded command (api.md §3.2), in its JSON, without `def_version` |
| `error` | text: the error's code |

Every key is required but a request's `native_id`; a case has one of
`encoded` and `error`. A file, case, request or value with a key this
section doesn't list, or without one it requires, is malformed.

A value is an object with one key, which names its kind (api.md §1,
[encoding.md](encoding.md) §1), so a case gives exactly the value it
means, one of a kind its argument refuses included: `{"text": "3"}` for
an integer argument is `wrong_type`, not 3.

| Key | Value |
| --- | --- |
| `integer` | a number within ±(2^53 − 1), or past it a string in its one decimal spelling (api.md §1: no sign but a leading `-`, no leading zeros), within −2^63 to 2^64 − 1 |
| `float` | a number with a fraction or an exponent, or a `non_finite` object (api.md §1) |
| `boolean` | `true` or `false` |
| `text` | a string |
| `bytes` | lowercase hex digits, two a byte, with no spaces |
| `list` | a list of values, none of them a list: a list argument's |

A request's arguments are an object, as a binding that takes them as a
map takes them, so no case gives a name twice (`duplicate_argument`).

### 3.1 Running a file

A runner builds the definitions into a store as §2.1 says, opens it
without bindings, and encodes each case's request (api.md §2.4) with its
family. It compares the result with the case's as §2.2 does: an encoded
command's `def_version` is the digest `describe` gives for its set, and an
error compares by its code, with an empty list of problems.

### 3.2 Expected results

No expected result is an encoder's output. They come from the same two
places as decoding's (§2.3), and a disagreement is settled the same way:

- The files directly in `conformance/encoding/` are worked out from the
  specifications by hand. Each case's `note` shows how, and it cites the
  narrowest sections its result depends on.
- The files in `conformance/encoding/oracle/` are generated from random
  command layouts by the testkit's oracle, which builds each request from
  values it chose and works forward from the specifications to the
  command it encodes to, or to the error a refused one gives, in code
  separate from MDL's encoder, and the testkit spells the result as
  [api.md](api.md) §3.2 does. What §2.3 says of the generated decoding
  cases holds for these: the oracle has the same authors as MDL, the
  property tests run MDL against it, so a misreading common to both would
  be in these cases too, and the files are a snapshot `just conformance`
  writes. A refused request that gives a name twice isn't among them, as
  no case can give one.

## 4. Command decoding cases

Each file in `conformance/decoding_commands/`, or in a directory under
it, is a JSON object with these keys:

| Key | Value |
| --- | --- |
| `about` | text: what the file covers, for people |
| `definitions` | one IR bundle, as the JSON frontend reads it ([frontends.md](frontends.md) §6) |
| `cases` | a list of cases |

A case is an object with these keys:

| Key | Value |
| --- | --- |
| `name` | text, unique in its file |
| `spec` | a list of the sections its expected result follows from, as in §2 (`"encoding.md §7.4"`) |
| `note` | text: how the expected result follows from them, for people |
| `family` | text: the family whose set decodes the payload |
| `set` | text: the set that decodes it |
| `message` | an object: the payload, as `hex` or `json` as in §2, and `metadata`, as in §2; no ground time, which decoding a command doesn't take |
| `decoded` | the decoded command (api.md §3.5), in its JSON, without `def_version` |
| `error` | text: the error's code |

Every key is required; a case has one of `decoded` and `error`, and a
message one of `hex` and `json`. A file, case or message with a key this
section doesn't list, or without one it requires, is malformed.

A runner builds the definitions into a store as §2.1 says, opens it
without bindings, and decodes each case's payload and metadata as a
command (api.md §2.7) with its family and set. It compares the result
with the case's as §2.2 does: a decoded command's `def_version` is the
digest `describe` gives for its set, and an error compares by its code,
with an empty list of problems.

Expected results come from the places §3.2 names: the files directly in
`conformance/decoding_commands/` are worked out by hand, and those in
`conformance/decoding_commands/oracle/` are generated by the testkit's
command oracle, from the same layouts as `conformance/encoding/oracle/`:
each request's payload with what decoding it gives back, and each
payload with a fault planted in it with the error it gives. What §2.3
says of generated cases holds for these.

## 5. Runners

This repository runs every suite in two places: the runtime's runner,
`crates/mdl/tests/conformance.rs`, which also checks that each section a
case cites is a heading of its file, and the Elixir binding's,
`bindings/elixir/test/mdl/conformance_test.exs`, which builds each file's
definitions with `morphc`.
