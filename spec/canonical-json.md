# Canonical JSON and artifact identity

Status: normative from IR v2 (`mdl_version` 2) on. The v1 `def_version`
in `crates/mdl/src/plan.rs` hashes serde's field-order output, not this
canonical form; it is replaced when IR v2 lands. The key words MUST,
MUST NOT, SHOULD and MAY are to be interpreted as described in RFC 2119.

Every MDL artifact (catalog, family, set) is identified by the SHA-256 of
its canonical JSON. Two implementations, in any languages, MUST compute the
same identity for the same artifact. This page defines exactly how.

## 1. Input rules

An implementation that reads an artifact MUST reject it, rather than
repair it, if any of these hold. The error code in brackets is the one the
conformance vectors use.

1. The bytes are not valid UTF-8, including overlong encodings and encoded
   surrogates. [`invalid_json`]
2. The text begins with a byte order mark. [`invalid_json`]
3. It is not valid JSON (RFC 8259). [`invalid_json`]
4. A string, or an object member name, contains an unpaired surrogate,
   whether escaped (`\ud800`) or not. [`invalid_json`]
5. A number's value rounds to positive or negative infinity as an IEEE 754
   double (`1e400`). RFC 8785 already forbids these. [`invalid_json`]
6. An object has two members with the same name, at any depth, comparing
   names after unescaping (`"a"` and `"\u0061"` are the same name).
   [`duplicate_key`]
7. An integer literal changes in canonical form. An *integer literal* is a
   JSON number with no fraction and no exponent. If its value lies in the
   range −2^63 to 2^64 − 1 inclusive, the implementation MUST reject it
   unless the canonical serialization (§2) of its nearest IEEE 754 double
   equals the literal's own digits (with a leading `-` for negatives, and
   `-0` read as `0`). [`inexact_integer`]
   - `946684800000000000` is accepted: its double prints those digits.
   - `9007199254740993` is rejected: its double prints `9007199254740992`.
   - `18446744073709551615` is rejected: its double prints
     `18446744073709552000`.

   Integer literals outside that range are read as doubles, like any other
   number.

Ordinary parsers violate several of these silently: Go's `encoding/json`
replaces a lone surrogate with U+FFFD, and JavaScript's `JSON.parse` keeps
it and reads `1e400` as `Infinity`. An implementation has to check for them
deliberately.

**Deviation from I-JSON.** RFC 7493 also forbids Unicode noncharacters
(U+FDD0 to U+FDEF, and U+xFFFE and U+xFFFF in every plane). MDL accepts
them, and the canonical form writes them as is. Every parser passes them
through unchanged, so they can't make implementations disagree, and
rejecting them would add a check that protects nothing.

Why the other rules exist: RFC 8785 treats every number as a double, so
without rule 7, two different artifacts could share one identity. Without
rule 6, parsers that keep the last duplicate and parsers that keep both
would produce different canonical text. Rules 1 to 5 stop implementations
from "repairing" input in different ways.

## 2. Canonical form

The canonical form is the output of the JSON Canonicalization Scheme,
RFC 8785, applied to the parsed document:

- no insignificant whitespace;
- object members sorted by name, comparing names as sequences of UTF-16
  code units;
- strings serialized as ECMAScript's `JSON.stringify` serializes them;
- numbers serialized as ECMAScript's `Number.prototype.toString`
  serializes the double.

## 3. Identity

An artifact's identity is the SHA-256 digest of the UTF-8 bytes of its
canonical form, written as 64 lowercase hexadecimal characters.

- An implementation SHOULD verify an artifact it receives by parsing it
  (§1), canonicalizing it (§2) and hashing the result. If the received bytes
  are already canonical, hashing them directly gives the same identity.
- An implementation MUST NOT compute identity over any other serialization
  (pretty-printed, re-ordered, or produced by a language's default JSON
  encoder).

## 4. Conformance

`conformance/canonical/` holds the vectors every implementation MUST pass:

| File | Contents |
| --- | --- |
| `numbers.json` | about 2,500 IEEE 754 bit patterns with their ECMAScript text |
| `documents.json` | `accepted`: input text and its canonical output. `rejected`: input as text (`input`) or raw bytes (`input_hex`), and the required error code from §1 |

The vectors are generated from a real ECMAScript engine by
`conformance/canonical/generate.mjs`. The reference implementation's runner
is `crates/mdl/tests/conformance_canonical.rs`.
