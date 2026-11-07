# Units

Status: normative from IR v2 (`mdl_version` 2) on. The key words MUST,
MUST NOT, SHOULD and MAY are to be interpreted as described in RFC 2119.

Every unit in an artifact (a param's, an argument's, a field's, a scalar
element's) is a code in the subset of UCUM (the Unified Code for Units of
Measure, case-sensitive form) this document defines. A unit outside it is
`unknown_unit`; a quantity MDL doesn't know can still be written as an
annotation (`{dBm}`). Frontends MAY accept friendlier spellings (`degC`,
`°C`) and MUST write the codes here.

## 1. Syntax

```
unit       := "Cel" | "[degF]" | term
term       := ["/"] component (("." | "/") component)*
component  := annotatable [annotation] | "(" term ")" [annotation] | annotation
annotatable:= symbol [exponent] | "10*" exponent | "10^" exponent | digits
symbol     := atom | prefix metric-atom
exponent   := ["+" | "-"] digits          (at most 99 in magnitude)
annotation := "{" printable ASCII other than braces "}"
```

A symbol is `[` up to the next `]`, or else the longest run of characters
other than digits and `+ - . / ( ) { [`; the atom-first rule below
applies to that whole run. An exponent is never 0, and digits have no
leading zero. An annotation isn't empty.

`.` multiplies and `/` divides by the next component only: `kg.m/s2` is
kilogram metre per second squared, and `m/s/s` is metre per second per
second. A leading `/` divides 1. An exponent binds to its symbol,
prefix included: `km2` is square kilometres. A parenthesized term takes
no exponent. Parentheses nest at most 8 deep. A symbol is matched as an
atom first, then as a prefix and a metric atom, so `Pa` is the pascal and
`cd` the candela, while `ms` is the millisecond. Celsius and Fahrenheit
stand alone, unprefixed: `Cel/s` is not a unit.

## 2. Dimensions and factors

A unit has a dimension, the exponents of UCUM's seven base units: the
metre (length), second (time), gram (mass), radian (plane angle), kelvin
(temperature), coulomb (charge) and candela (luminous intensity). The
gram, not the kilogram, is the base, and the coulomb, not the ampere.

A unit's *factor* is how many base units one of it is. It is computed in
binary64, starting from 1, through the components left to right: `.`
multiplies by the component's factor raised to its exponent, `/` divides
by it. A component's factor is its prefix's factor times its atom's (the
prefix first); raised to a positive exponent `e`, it is 1 multiplied by
it `e` times; to a negative one, 1 divided by that. `10*n` and `10^n` are
the binary64 nearest 10ⁿ; digits are that integer; an annotation is 1;
a parenthesized term is its own factor. Every decimal below means the
binary64 nearest it. A factor that isn't a normal binary64 (zero,
subnormal or infinite) makes the code `unknown_unit`.

### 2.1 Prefixes

Metric atoms take a prefix; the binary ones (`Ki`, `Mi`, `Gi`, `Ti`) are
for data sizes:

| Prefix | Factor | Prefix | Factor |
| --- | --- | --- | --- |
| `Y` | 1e24 | `d` | 1e-1 |
| `Z` | 1e21 | `c` | 1e-2 |
| `E` | 1e18 | `m` | 1e-3 |
| `P` | 1e15 | `u` | 1e-6 |
| `T` | 1e12 | `n` | 1e-9 |
| `G` | 1e9 | `p` | 1e-12 |
| `M` | 1e6 | `f` | 1e-15 |
| `k` | 1e3 | `a` | 1e-18 |
| `h` | 1e2 | `z` | 1e-21 |
| `da` | 1e1 | `y` | 1e-24 |
| `Ki` | 1024 | `Gi` | 1073741824 |
| `Mi` | 1048576 | `Ti` | 1099511627776 |

### 2.2 Atoms

Dimensions are written as products of `L` (length), `T` (time), `M`
(mass), `A` (angle), `K` (temperature), `Q` (charge) and `J` (luminous
intensity); "metric" atoms take prefixes.

| Atom | Name | Dimension | Factor | Metric |
| --- | --- | --- | --- | --- |
| `m` | metre | L | 1 | yes |
| `s` | second | T | 1 | yes |
| `g` | gram | M | 1 | yes |
| `rad` | radian | A | 1 | yes |
| `K` | kelvin | K | 1 | yes |
| `C` | coulomb | Q | 1 | yes |
| `cd` | candela | J | 1 | yes |
| `%` | percent | 1 | 0.01 | no |
| `[ppm]` | part per million | 1 | 1e-6 | no |
| `[ppb]` | part per billion | 1 | 1e-9 | no |
| `mol` | mole | 1 | 6.0221367e23 | yes |
| `sr` | steradian | A² | 1 | yes |
| `Hz` | hertz | T⁻¹ | 1 | yes |
| `N` | newton | M L T⁻² | 1000 | yes |
| `Pa` | pascal | M L⁻¹ T⁻² | 1000 | yes |
| `J` | joule | M L² T⁻² | 1000 | yes |
| `W` | watt | M L² T⁻³ | 1000 | yes |
| `A` | ampere | Q T⁻¹ | 1 | yes |
| `V` | volt | M L² T⁻² Q⁻¹ | 1000 | yes |
| `F` | farad | Q² T² M⁻¹ L⁻² | 0.001 | yes |
| `Ohm` | ohm | M L² T⁻¹ Q⁻² | 1000 | yes |
| `S` | siemens | Q² T M⁻¹ L⁻² | 0.001 | yes |
| `Wb` | weber | M L² T⁻¹ Q⁻¹ | 1000 | yes |
| `T` | tesla | M T⁻¹ Q⁻¹ | 1000 | yes |
| `H` | henry | M L² Q⁻² | 1000 | yes |
| `lm` | lumen | J A² | 1 | yes |
| `lx` | lux | J A² L⁻² | 1 | yes |
| `Bq` | becquerel | T⁻¹ | 1 | yes |
| `Gy` | gray | L² T⁻² | 1 | yes |
| `Sv` | sievert | L² T⁻² | 1 | yes |
| `L`, `l` | litre | L³ | 0.001 | yes |
| `t` | tonne | M | 1e6 | yes |
| `bar` | bar | M L⁻¹ T⁻² | 1e8 | yes |
| `eV` | electronvolt | M L² T⁻² | 1.60217733e-16 | yes |
| `min` | minute | T | 60 | no |
| `h` | hour | T | 3600 | no |
| `d` | day | T | 86400 | no |
| `wk` | week | T | 604800 | no |
| `a` | Julian year | T | 31557600 | no |
| `deg` | degree | A | 0.017453292519943295 | no |
| `'` | minute of arc | A | 0.0002908882086657216 | no |
| `''` | second of arc | A | 4.84813681109536e-6 | no |
| `atm` | standard atmosphere | M L⁻¹ T⁻² | 101325000 | no |
| `[g]` | standard gravity | L T⁻² | 9.80665 | no |
| `[in_i]` | inch | L | 0.0254 | no |
| `[ft_i]` | foot | L | 0.3048 | no |
| `[yd_i]` | yard | L | 0.9144 | no |
| `[mi_i]` | mile | L | 1609.344 | no |
| `[nmi_i]` | nautical mile | L | 1852 | no |
| `[kn_i]` | knot | L T⁻¹ | 0.5144444444444445 | no |
| `[lb_av]` | pound | M | 453.59237 | no |
| `[lbf_av]` | pound-force | M L T⁻² | 4448.2216152605 | no |
| `[psi]` | pound per square inch | M L⁻¹ T⁻² | 6894757.293168361 | no |
| `[gal_us]` | US gallon | L³ | 0.003785411784 | no |
| `bit` | bit | 1 | 1 | yes |
| `By` | byte | 1 | 8 | yes |
| `Bd` | baud | T⁻¹ | 1 | yes |

As in UCUM, the mole is a number (dimensionless, with UCUM's value of the
Avogadro constant), as are the bit and byte: `mmol`, `bit` and `%` are
compatible with `1`.

`Cel` (degree Celsius) and `[degF]` (degree Fahrenheit) have dimension K
and convert with an offset (§4). Logarithmic units (bel, decibel, neper)
are not MDL units; write them as annotations.

## 3. Compatibility

Two units are compatible when they have the same dimension and the same
annotations (as a sorted list of their texts): `{counts}` is compatible
with `{counts}`, but not with `{dBm}` or with `1`, though UCUM itself would
let them all convert. `%` and `1` are compatible. A missing unit is
compatible only with a missing unit.

Validation (`unit_mismatch`):

- a field or scalar element mapped to an `f64` param, a member of a
  composite, and a field that encodes an `f64` argument MAY be in any
  unit compatible with the param's or argument's (a geodetic composite's
  first two members with `deg`);
- one mapped to an `i64` or `u64` param, or encoding an `i64` or `u64`
  argument, MUST be in the same unit, since converting would need
  rounding.

## 4. Conversion

A value `v` converts from a compatible unit `a` to `b` in binary64:

- neither is Celsius or Fahrenheit: `(v × factor(a)) / factor(b)`, so 1500
  mV is `(1500 × 1) / 1000` = 1.5 V;
- otherwise, through kelvin: from `Cel`, `v + 273.15`; from `[degF]`,
  `((v + 459.67) × 5) / 9`; from any other unit, `v × factor(a)`. Then to
  `Cel`, `k − 273.15`; to `[degF]`, `((k × 9) / 5) − 459.67`; to any other
  unit, `k / factor(b)`.

Two units that are the same (the same dimension, factor, temperature scale
and annotations), and in particular the same code, or no unit on either
side, convert nothing: the value is unchanged.

`decoding.md` §7.3 converts a sample's value from its field's unit into
its param's; `encoding.md` §3.1 converts an argument's value from its unit
into its field's, and §7.4 back.
