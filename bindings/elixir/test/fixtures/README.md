# Test fixtures

The tests build their stores with `morphc` from the repository's own
definitions, so nothing here is a generated artifact:

- the kitchen sink, `crates/mdl/tests/fixtures/full_bundle.json`, with its
  bindings, `crates/mdl/tests/fixtures/full_bindings.toml` (which the fuzz
  targets use too);
- the README's worked example, `examples/beacon`.

`kitchen/messages.json` holds messages for the kitchen sink: the decode
fuzz target's seeds (`fuzz/seeds/decode`), read from that target's input
layout (`crates/mdl-fuzz/src/messages.rs`), less `vendor_j-as-binary`,
which that target delivers in the other encoding than its family's, as a
binding can't; then `sink-non-finite`
(`sink-mixed` with NaN and infinities in its floats and a temperature of
-128, which means no reading), `vendor_j-far-future` (an event time in the
year 3000, past 64-bit nanoseconds), `vendor_j-no-time` (a thermal
message with no time, which decoding refuses) and `legacy-time-invalid`
(a time packet whose `unix_us` is 2^64 - 1, which means no reading: an
integer past 2^63 in the record). Each has its family, its
ground time, its metadata, and its payload in hex or, for a JSON family,
as the document's text.

The conformance test (`test/mdl/conformance_test.exs`) builds a store of
its own from each file of `conformance/decoding`, writing the file's
definitions where `morphc` reads them (spec/conformance.md).
