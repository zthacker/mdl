// Regenerates the canonical JSON conformance vectors from a real ECMAScript
// engine, which is the reference RFC 8785 is defined against.
//
//   node conformance/canonical/generate.mjs
//
// Deterministic: a seeded PRNG, so the output only changes if this script
// or the engine's number formatting changes (which would be a bug).
import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

// mulberry32: tiny, seeded, good enough for test data.
function rng(seed) {
  return () => {
    seed |= 0;
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
const random = rng(8785);
const u32 = () => Math.floor(random() * 4294967296);

const view = new DataView(new ArrayBuffer(8));
function fromBits(hi, lo) {
  view.setUint32(0, hi);
  view.setUint32(4, lo);
  return view.getFloat64(0);
}
function bitsHex(x) {
  view.setFloat64(0, x);
  return (
    view.getUint32(0).toString(16).padStart(8, "0") +
    view.getUint32(4).toString(16).padStart(8, "0")
  );
}

// ---- numbers.json: IEEE 754 bit pattern -> ECMAScript text ----------------
const numbers = [];
const seen = new Set();
function addNumber(x) {
  if (!Number.isFinite(x)) return;
  const hex = bitsHex(x);
  if (seen.has(hex)) return;
  seen.add(hex);
  numbers.push([hex, String(x)]);
}
// Edges: zeros, subnormals, extremes, powers of ten and two, the 1e21
// switch to exponent form, the 2^53 boundary.
[0, -0, 5e-324, -5e-324, Number.MIN_VALUE, Number.MAX_VALUE, -Number.MAX_VALUE,
 2 ** 53 - 1, 2 ** 53, 2 ** 53 + 2, 1e21, 1e21 - 65536, 999999999999999900000,
 1e-6, 1e-7, 0.1, 0.2, 0.30000000000000004, 1 / 3, 2 / 3, Math.PI, Math.E]
  .forEach(addNumber);
for (let e = -30; e <= 30; e++) { addNumber(10 ** e); addNumber(-(10 ** e)); }
for (let e = -60; e <= 80; e++) addNumber(2 ** e);
// Random bit patterns (every exponent), and random "decimal-looking" values.
while (numbers.length < 2500) {
  addNumber(fromBits(u32(), u32()));
  addNumber(Math.round(random() * 1e6) / 10 ** Math.floor(random() * 8));
}

// ---- documents.json: input text -> canonical text ------------------------
// The reference canonicalizer: RFC 8785 defines its output as what
// ECMAScript's JSON.stringify produces for strings and numbers, with object
// keys sorted by UTF-16 code units (the default Array.prototype.sort order).
function canonicalize(v) {
  if (v === null || typeof v !== "object") return JSON.stringify(v);
  if (Array.isArray(v)) return "[" + v.map(canonicalize).join(",") + "]";
  return "{" + Object.keys(v).sort()
    .map((k) => JSON.stringify(k) + ":" + canonicalize(v[k])).join(",") + "}";
}

const alphabet = [
  "a", "b", "Z", "0", "~", "/", "\\", '"', " ", "\t", "\n", "\r", "\b", "\f",
  "\u0000", "\u0001", "\u001f", "\u007f", "\u0080", "\u00e9", "\u2028",
  "\u2029", "\u20ac", "\ufb33", "\ufeff", "\ud83d\ude00", "\ud800\udc00",
];
function randomString() {
  let s = "";
  const n = Math.floor(random() * 6);
  for (let i = 0; i < n; i++) s += alphabet[Math.floor(random() * alphabet.length)];
  return s;
}
function randomNumber() {
  switch (Math.floor(random() * 4)) {
    case 0: return Math.floor(random() * 2000) - 1000;
    case 1: return (random() - 0.5) * 10 ** (Math.floor(random() * 60) - 30);
    case 2: return fromBits(u32(), u32());
    default: return Math.round(random() * 1e5) / 100;
  }
}
function randomValue(depth) {
  const r = random();
  if (depth > 3 || r < 0.5) {
    const leaf = Math.floor(random() * 5);
    if (leaf === 0) return null;
    if (leaf === 1) return random() < 0.5;
    if (leaf === 2) return randomString();
    const x = randomNumber();
    return Number.isFinite(x) ? x : 0;
  }
  if (r < 0.75) {
    return Array.from({ length: Math.floor(random() * 5) }, () => randomValue(depth + 1));
  }
  const o = {};
  for (let i = Math.floor(random() * 5); i > 0; i--) o[randomString()] = randomValue(depth + 1);
  return o;
}

const documents = [
  {
    name: "rfc8785-3.2.2",
    input: '{"numbers":[333333333.33333329,1E30,4.50,2e-3,0.000000000000000000000000001],' +
      '"string":"\\u20ac$\\u000F\\u000aA\'\\u0042\\u0022\\u005c\\\\\\"\\/","literals":[null,true,false]}',
  },
  {
    name: "rfc8785-3.2.3-sorting",
    input: '{"\\u20ac":"Euro Sign","\\r":"Carriage Return","\\ufb33":"Hebrew Letter Dalet With Dagesh",' +
      '"1":"One","\\ud83d\\ude00":"Emoji: Grinning Face","\\u0080":"Control",' +
      '"\\u00f6":"Latin Small Letter O With Diaeresis"}',
  },
  { name: "whitespace-and-nesting", input: ' { "b" : [ 1 , { "d" : 2.50 , "c" : -0 } ] , "a" : "" } ' },
  { name: "integer-above-2^53-that-is-its-own-canonical-text", input: '{"epoch_ns":946684800000000000}' },
  // Integer literals outside the 64-bit range are doubles (spec rule 7).
  // Python and Elixir decode them as exact big integers by default.
  { name: "integer-beyond-u64-is-a-double", input: "[18446744073709551617]" },
  { name: "integer-below-i64-is-a-double", input: "[-9223372036854775809]" },
  { name: "integer-beyond-u64-in-exponent-range", input: "[100000000000000000000000]" },
  { name: "underflow-is-zero", input: "[1e-400]" },
];
for (const d of documents) d.output = canonicalize(JSON.parse(d.input));
for (let i = 0; i < 400; i++) {
  const v = randomValue(0);
  // Pretty-print with an indent, so inputs exercise whitespace removal too.
  const input = JSON.stringify(v, null, i % 3);
  documents.push({ name: `random-${i}`, input, output: canonicalize(JSON.parse(input)) });
}

// MDL's input rules (spec/canonical-json.md, section 1). Each case is either
// `input` (text) or `input_hex` (raw bytes, for input that isn't valid
// UTF-8 and so can't be a JSON string here). Ordinary parsers accept some of
// these (JSON.parse keeps lone surrogates and reads 1e400 as Infinity), so
// an implementation has to check for them deliberately.
const backslash = String.fromCharCode(92);
const rejected = [
  { name: "duplicate-key", input: '{"a":1,"a":2}', error: "duplicate_key" },
  { name: "nested-duplicate-key", input: '[{"x":{"k":1,"k":1}}]', error: "duplicate_key" },
  { name: "duplicate-key-after-unescaping", input: '{"a":1,"' + backslash + 'u0061":2}', error: "duplicate_key" },
  { name: "integer-2^53+1", input: "[9007199254740993]", error: "inexact_integer" },
  { name: "integer-u64-max", input: "18446744073709551615", error: "inexact_integer" },
  { name: "integer-i64-min", input: "-9223372036854775808", error: "inexact_integer" },
  { name: "escaped-lone-high-surrogate", input: '"' + backslash + 'ud800"', error: "invalid_json" },
  { name: "escaped-lone-low-surrogate", input: '["' + backslash + 'udc00x"]', error: "invalid_json" },
  { name: "number-beyond-a-double", input: "[1e400]", error: "invalid_json" },
  { name: "negative-number-beyond-a-double", input: '{"x":-1e400}', error: "invalid_json" },
  { name: "invalid-utf8", input_hex: "22ff22", error: "invalid_json" },
  { name: "overlong-utf8", input_hex: "5b22c0af225d", error: "invalid_json" },
  { name: "raw-surrogate-bytes", input_hex: "22eda08022", error: "invalid_json" },
  { name: "byte-order-mark", input_hex: "efbbbf7b7d", error: "invalid_json" },
  // Not JSON at all, though some parsers accept them (Python's json.loads
  // takes NaN and Infinity by default).
  { name: "nan", input: "[NaN]", error: "invalid_json" },
  { name: "infinity", input: "[Infinity]", error: "invalid_json" },
  { name: "negative-infinity", input: "[-Infinity]", error: "invalid_json" },
  { name: "trailing-comma-in-array", input: "[1,]", error: "invalid_json" },
  { name: "trailing-comma-in-object", input: '{"a":1,}', error: "invalid_json" },
  { name: "leading-zero", input: "[01]", error: "invalid_json" },
  { name: "raw-control-character-in-string", input_hex: "220122", error: "invalid_json" },
  { name: "two-documents", input: "{} {}", error: "invalid_json" },
  { name: "lone-surrogate-in-key", input: '{"' + backslash + 'ud800":1}', error: "invalid_json" },
];

const header = (what) =>
  `Generated by generate.mjs with ${process.release.name} ${process.version}; ${what}`;
writeFileSync(
  join(here, "numbers.json"),
  JSON.stringify({ about: header("[IEEE 754 bits as hex, ECMAScript text]"), vectors: numbers }, null, 1) + "\n",
);
writeFileSync(
  join(here, "documents.json"),
  JSON.stringify({ about: header("input -> canonical output, or input -> error"), accepted: documents, rejected }, null, 1) + "\n",
);
console.log(`numbers: ${numbers.length}, documents: ${documents.length}, rejected: ${rejected.length}`);
