# XTCE

Status: normative from IR v2 (`mdl_version` 2) on. The key words MUST,
MUST NOT, SHOULD and MAY are to be interpreted as described in RFC 2119.

The XTCE importer reads OMG XTCE documents (versions 1.0, 1.1 and 1.2)
and produces a bundle (`frontends.md`). This document says which XTCE it
reads, what each construct becomes, and what it reports. The reference
implementation is the `mdl-xtce` crate.

XTCE describes a space system's packets whole, headers included. An
imported packet is one concrete sequence container with the entries of
every container it inherits from; the restriction criteria along the way
become its identify keys. What XTCE says that MDL can't represent is
reported as `frontends.md` §2 requires: an error when leaving it out would
change a decoded value, a warning otherwise.

## 1. Documents

An import reads one or more documents. Each MUST be well-formed XML
without a DTD [`xtce.syntax`] whose root is a `SpaceSystem` in
`http://www.omg.org/space/xtce` (XTCE 1.0 and 1.1) or
`http://www.omg.org/spec/XTCE/20180204` (XTCE 1.2) [`xtce.document`]. Each
root is a top-level SpaceSystem; two with one name are an error at the
later one [`xtce.duplicate`]. References may cross documents.

Every element the importer reads is checked. An attribute or child it
doesn't read is an error [`xtce.unsupported`], since it can't tell whether
leaving it out would change a value, unless this document lists it as
left out (§6), which is a warning [`xtce.ignored`]. Attributes in the XML
Schema instance and XML namespaces (`xsi:schemaLocation`, `xml:lang`)
describe the document and are skipped; attributes in other namespaces are
warnings, and elements in other namespaces, XInclude among them, are
errors.

SpaceSystems nest at most 64 deep, and aggregates (an aggregate whose
member is an aggregate, and so on) at most 32, MDL's bound on struct
nesting; deeper ones are unsupported. Real documents come nowhere near
either, and the bounds keep a crafted document from exhausting the
importer.

## 2. Names and references

Each SpaceSystem names its parameter types, parameters and containers,
each kind on its own; a name used twice for one kind is an error at the
second, with the first as its first location [`xtce.duplicate`].

References resolve as Yamcs resolves them, so a document written for it
imports as it loads there:

- one starting with `/` from the root SpaceSystem it names
  (`/BogusSAT/SC001/Bus_Voltage`);
- one starting with `./` or `..` from the referring SpaceSystem;
- any other in the referring SpaceSystem, then in each SpaceSystem above
  it in turn, the nearest first.

XTCE 1.2 looks for an unqualified name in the referring SpaceSystem only;
a reference that resolves under that rule resolves the same way here. A
parameter reference may go on into an aggregate's members, written as
XTCE writes them (`Packet_ID.APID`) or as Yamcs does (`Packet_ID/APID`). A
reference that names nothing is an error at the reference
[`xtce.reference`].

## 3. The bundle

An import produces one family and one set:

- **Family.** Its name is the importer's option, or the first root
  SpaceSystem's name lowercased with `_` for each run of characters other
  than letters and digits; a name that doesn't start with a letter needs
  the option [`xtce.name`]. Its encoding is `binary`. Its byte order is
  little-endian when every number wider than a byte is, and big-endian
  otherwise, each little-endian field naming its own (`decoding.md`
  §2.1), so a big-endian header ahead of little-endian data is described
  as it is. Its alias is the option, or a fixed value: the first root
  SpaceSystem's name.
- **Set.** Its name is the option, or the family's. It has one packet for
  each concrete sequence container, in document order, and a struct type
  for each embedded container and each array element that isn't a scalar
  (§5).
- **Catalog.** Each parameter a packet holds is a param; an aggregate
  parameter is one param per member. A param's name is the parameter's
  alias in the `MDL` namespace (`<Alias nameSpace="MDL"
  alias="eps.bus_voltage"/>`), taken as it is, or else its SpaceSystem
  path and name, each part lowercased with `_` for each run of other
  characters and none at either end, joined by dots:
  `/BogusSAT/SC001/Bus_Voltage` is `bogussat.sc001.bus_voltage`. A
  member's name adds its own part (`demosat.adcs.rate.x`). Two parameters
  with one catalog name are an error at the later, with the earlier as its
  first location [`xtce.name`]. A param's description is the parameter's
  `shortDescription` and `LongDescription`, or a member's.
- **Names in a set.** A field is named for its parameter, members joined
  by `_` (`CCSDS_Packet_ID_APID`), and a packet for its container. A name
  that isn't an MDL identifier has `_` for each other character, and `_`
  before a leading digit. Parameters in different SpaceSystems that share
  a name (as MDL identifiers) are each qualified by as many of their
  SpaceSystems' names, nearest first, as it takes to tell them apart
  (`/Sat/EPS/Status` and `/Sat/ADCS/Status` are fields `EPS_Status` and
  `ADCS_Status`), whether or not they share a packet, so a field has one
  name in every packet; containers that share a name are qualified the
  same way (`EPS_HK`, `ADCS_HK`), and so are embedded containers' struct
  types. Each change is reported [`xtce.renamed`]. With the importer's
  qualified naming, every field, packet and struct type name carries its
  SpaceSystems below the root (`EPS_HK` even when it's the only `HK`, and
  the root too where two roots would give one name), so names don't
  change as documents grow; those changes aren't reported. Two entries
  of one packet or struct type, two packets, or two struct types that
  still share a name are an error at the later, with the earlier as its
  first location [`xtce.name`].

## 4. Parameter types

| XTCE type | Field | Param |
| --- | --- | --- |
| `IntegerParameterType` | `uint` (`unsigned`) or `int` (`twosComplement`, or XTCE 1.0's `twosCompliment`), 1 to 64 bits | `i64`, or `u64` when `signed="false"` |
| `FloatParameterType` | `float` (IEEE 754, 32 or 64 bits), or an integer encoding with or without a calibrator | `f64` |
| `EnumeratedParameterType` | an integer encoding with a `states` conversion from its `EnumerationList` | `enum`, its labels in order, each once |
| `BooleanParameterType` | `bool` for one bit, else `uint` | `bool`: nonzero is true |
| `StringParameterType` | `string`, `utf8` (the default), `ascii` (`US-ASCII`) or `latin1` (`ISO-8859-1`); a `Fixed` size in whole bytes, up to a one-byte `TerminationChar`, or both: a fixed-size buffer whose value ends at the terminator | `str` |
| `BinaryParameterType` | `bytes`, a fixed size in whole bytes, or a size another parameter gives | `bytes` |
| `AggregateParameterType` | one field per member, depth first | one per member |
| `ArrayParameterType` | an array of one dimension: scalar elements, or a struct type of an aggregate's members | the elements' param, or one per member, each element a sample with its index |
| `RelativeTimeParameterType` | its `Encoding`'s integer or float encoding; a `scale` or `offset` other than 1 and 0 is a `linear` conversion | `f64`, in `s` (`seconds`, the default), `ps` (`picoSeconds`) or `d` (`days`); a duration in `months` or `years` has no unit, since their lengths vary [`xtce.ignored`] |

The unit, for integer and float types, is the type's one `Unit` without a
`power`, a `factor` or a `form` other than `calibrated`. Text that is a
UCUM code MDL knows (`units.md`) is that code, as UCUM reads it (so `C` is
the coulomb). `C` and `F` are left out with a warning saying so: they
are the coulomb and the farad in UCUM, but XTCE documents mostly mean
degrees by them. Otherwise these spellings, compared without case, are
their codes, and any other unit is left out [`xtce.ignored`]:

| Spelling | Code |
| --- | --- |
| `degC`, `deg C`, `°C`, `celsius`, `degrees C` | `Cel` |
| `degF`, `°F`, `fahrenheit` | `[degF]` |
| `kelvin` | `K` |
| `degree`, `degrees`, `°` | `deg` |
| `radian`, `radians` | `rad` |
| `volt`, `volts` | `V` |
| `amp`, `amps`, `ampere`, `amperes` | `A` |
| `watt`, `watts` | `W` |
| `hertz` | `Hz` |
| `second`, `seconds`, `sec`, `secs` | `s` |
| `millisecond`, `milliseconds`, `msec` | `ms` |
| `meter`, `meters`, `metre`, `metres` | `m` |
| `percent` | `%` |
| `byte`, `bytes`, `octet`, `octets` | `By` |
| `bits` | `bit` |

Encodings:

- `byteOrder` (`mostSignificantByteFirst`, `leastSignificantByteFirst`),
  or a `ByteOrderList` that is one of them, is the byte order of a number
  wider than eight bits; within a byte it doesn't apply. A
  `ByteOrderList` names every byte of the value [`xtce.invalid`]. A
  little-endian value wider than a byte is whole bytes, as `decoding.md`
  §2.1 reads it. Any other byte order, `bitOrder="leastSignificantBitFirst"`,
  and `ErrorDetectCorrect` are unsupported.
- A `DefaultCalibrator` is the field's conversion. A
  `PolynomialCalibrator` is `linear` up to degree 1, else `poly` (degree
  at most 32); terms with one exponent add up. A `SplineCalibrator` of
  `order` 0 is a `step` table, of order 1 a `linear` one, and
  `extrapolate` carries over. This follows XTCE: Yamcs interpolates and
  extrapolates linearly whatever the order and `extrapolate` say.
- Unsupported, each an error: `ContextCalibratorList`,
  `MathOperationCalibrator`, splines of order 2 or more or whose points
  name different orders, a calibrator on an integer, enumerated or boolean
  type (MDL's calibrations give floats), sign-magnitude, ones' complement
  and BCD integers, floats other than IEEE 754 binary32 and binary64, text
  other than the three encodings above, binary transform algorithms,
  enumerations over a range of values (`maxValue`), type inheritance
  (`baseType`), absolute time parameters (an MDL field holds a number, not
  an instant), a relative time with a `ReferenceTime`, and a relative time
  with both a calibrator and a scale or offset.

Limits: an integer or float type's `DefaultAlarm` gives its params'
limits. Its `StaticAlarmRanges` hold the normal ranges: the
`WarningRange`'s ends are `yellow_low` and `yellow_high`, and the
`CriticalRange`'s `red_low` and `red_high`. What MDL's limits don't
hold, which doesn't change a decoded value, is left out, each a warning
[`xtce.ignored`]: an exclusive end (it's imported as the limit, since MDL
limits don't say whether a value at the limit is in alarm), the watch,
distress and severe levels (MDL has two), ranges in alarm inside them
(`rangeForm="inside"`), change alarms, alarm conditions, custom alarms, a
`minViolations` other than 1, `minConformance`, context alarms, and the
alarms of other types, relative time types included.

Sizes:

- **From another parameter.** A `DynamicValue` gives a size in bits: the
  parameter's value times `slope` (1 by default) plus `intercept` (0).
  MDL lengths are bytes, so slope and intercept MUST be multiples of 8
  (`slope="8"` for a parameter counting bytes); a parameter counting bits
  is unsupported. The parameter MUST be an unsigned integer field decoded
  earlier in the same MDL container (the packet, or the embedded
  container), whose raw value is its value (no calibrator, or
  `useCalibratedValue="false"`); it becomes MDL's `{kind: field}` length
  or count. Yamcs's `_yamcs_ignore` reference means no size, so a
  `Variable` string with it ends at its terminator or leading size.
- **Strings.** A `Variable` string is a `DynamicValue` size, up to a
  `TerminationChar`, or a `LeadingSize`; `maxSizeInBits` is left out
  [`xtce.ignored`]. A `LeadingSize` (a Pascal string) is a size tag of
  `sizeInBitsOfSizeTag` bits, in the encoding's byte order, then that many
  bytes: the importer adds the tag as a field named for the string with
  `_size` (a field already named that way is `xtce.name`). A string ended
  by a terminator or a leading size inside a
  buffer whose size another parameter gives, a leading size inside a
  `Fixed` buffer, and a size chosen from a `DiscreteLookupList` are
  unsupported.
- **Arrays.** A `DimensionList` has one `Dimension`, whose fixed
  `StartingIndex` and `EndingIndex` (fixed or from a parameter) give the
  count: `end − start + 1`. Several dimensions, arrays of arrays, a
  `StartingIndex` from a parameter, and elements whose size another value
  gives are unsupported. An XTCE 1.1 array type has no `DimensionList`
  and takes its size from its `ArrayParameterRefEntry`.

Aggregates and arrays nest at most 32 deep, MDL's bound on struct
nesting.

## 5. Containers

Each sequence container that isn't `abstract`, and that no
`ContainerRefEntry` embeds, is a packet; an embedded container is a
structure, as the CCSDS green book's secondary header is, whether or not
it's abstract. One that isn't abstract is reported, naming a container
that embeds it [`xtce.ignored`]. A packet's entries are those of every container it
inherits from (`BaseContainer`), root first, then its own. A container
that inherits from itself is an error [`xtce.invalid`].

- **Entries.** A `ParameterRefEntry` is a field, one per member of an
  aggregate, or an array. An `ArrayParameterRefEntry` is an array; its
  `DimensionList` gives an XTCE 1.1 array its size, and MUST otherwise be
  its type's (a subset of an array is unsupported, as is an array split
  across entries). A parameter, or member, is mapped at most once in a
  packet, counting the struct types of its struct entries, since MDL maps
  a param from one entry per packet. Segment, stream and indirect
  entries, a container's own `BinaryEncoding`, and a `TimeAssociation` on
  an entry or a parameter (it times the value by another parameter, and
  an MDL sample takes its packet's time) are unsupported.
- **Embedded containers.** A `ContainerRefEntry` is a struct entry, named
  for the container, of a struct type holding the container's entries.
  Positions in it are from its own start, and its conditions, sizes and
  counts compare only its own fields, as MDL's struct types do. An
  embedded container that inherits from another, or has no entries, is
  unsupported; one that contains itself is an error [`xtce.invalid`]; and
  containers embed at most 32 deep.
- **Repeated entries.** A `RepeatEntry` makes the entry an array of its
  parameter's values, each a sample with its index, or of an embedded
  container's struct type. Its `Count` is fixed or from a parameter (as
  sizes are, but a count). An `Offset` is padding after each repetition,
  the last one included, as Yamcs reads it (XTCE's wording puts it only
  between repetitions), so an entry placed after the repeat is where
  Yamcs reads it. The elements are then a struct type of the value (a
  field named `value`, or the aggregate's members) and the padding. An
  `Offset` on a repeated container, and a repeated array, are
  unsupported.
- **Element struct types.** An array's elements that aren't one scalar
  are a struct type named for the field with `_element` (and
  `_gap` and the padding's bits, for a repeat with an `Offset`:
  `V_element_gap8`), whose fields are
  named for the members below the element (`id`, `pos_x`); they map to
  the parameter's catalog names, so each element's values are samples
  with its index.
- **Positions.** Entries follow one another from the start of the packet.
  `LocationInContainerInBits` with `containerStart` places an entry that
  many bits from the start of the packet, for an inherited container's
  entries too, as Yamcs reads them. With `previousEntry` and a value
  above zero, a padding entry of that many bits goes before it, present
  only when the entry is. A negative `previousEntry`, `containerEnd`,
  `nextEntry` and a location worked out from other values are
  unsupported.
- **Conditions.** An `IncludeCondition` holding a `Comparison` or a
  `ComparisonList` is the entry's `when`: a `compare`, or `all` of them.
  A `BooleanExpression` is the same nested: `ANDedConditions` is `all`,
  `ORedConditions` is `any`, and a `Condition` (its ParameterInstanceRef,
  ComparisonOperator and Value written as elements) is a `compare`; a
  `Condition` comparing two parameters is unsupported, as are expressions
  nested more than 32 deep, MDL's bound on condition nesting. Each
  comparison compares a field decoded earlier in the packet. A condition
  holds one `Comparison`, `ComparisonList` or `BooleanExpression`; a list
  holds at least one comparison, a `BooleanExpression` one member, and an
  `ANDedConditions` or `ORedConditions` at least one. Anything else is an
  error [`xtce.invalid`]: read in part, it would compare something other
  than what the document says. XTCE leaves an absent entry unplaced, so the next one follows the entry before it,
  where MDL starts it at the absent entry's place; the two agree unless
  an entry with a condition and a `containerStart` location is followed
  by one placed after it, which is unsupported.
- **Identification.** The restriction criteria of a container's
  `BaseContainer`, a `Comparison`, a `ComparisonList`, or a
  `BooleanExpression` that ANDs its conditions, are identify keys of that
  container and every packet built on it. Each compares, for equality, a
  field of the containers above, as XTCE evaluates criteria before the
  container's own entries are read. Criteria that require one field to
  have two values are an error [`xtce.invalid`]. Other operators, an
  `ORedConditions` of two or more conditions (one is that condition),
  `NextContainer` and algorithms are unsupported. What criteria hold is
  checked as a condition's is.
- **Values compared.** MDL compares raw values. With `useCalibratedValue`
  (the default), the value is an enumeration's label, a boolean's labels
  or `true` and `false`, or the integer of an integer field without a
  calibrator; any other engineering value, and an ordering comparison of
  labels, is unsupported. Without it, the value is the raw integer.
  Integers are decimal or `0x` hexadecimal. A comparison with an
  `instance` other than 0 is unsupported.
- **Values that are more than one raw value.** A boolean wider than a bit
  is true for any raw value but 0, so in a condition `== true` is `ne 0`
  and `!= true` is `eq 0`. An enumeration label on several raw values is
  `any` of them for `==`, and `all` of not them for `!=`. An identify key
  is one value, so either one there is unsupported.

A parameter no packet holds isn't imported [`xtce.ignored`]; this is
reported when every packet imported, since after an error which packets
hold what isn't known.

## 6. What is left out

These don't change a decoded value. Each is a warning where it is
[`xtce.ignored`]:

- descriptions of anything but parameters and aggregate members;
- aliases outside the `MDL` namespace, and ancillary data;
- initial values, change thresholds, what alarms say beyond MDL's limits
  (§4), valid ranges, display formats (`ToString`), and parameter
  properties other than time associations, validity conditions included
  (they set a value's quality, not the value);
- expected rates and idle patterns;
- the document `Header`, algorithms, streams, services and operational
  status;
- a boolean type's labels other than `True` and `False`, and a terminated
  string's `maxSizeInBits`;
- in this version, commands: argument types, meta-commands and command
  containers (the parameter types and parameters in `CommandMetaData` are
  read, since telemetry may use them).

## 7. Diagnostics

| Code | What |
| --- | --- |
| `xtce.syntax` | Text that isn't well-formed XML, or has a DTD. |
| `xtce.document` | A document that isn't XTCE, or no documents. |
| `xtce.invalid` | Something XTCE doesn't allow: a missing attribute, a value that isn't a number, a container that inherits from itself, criteria that can't all hold. |
| `xtce.reference` | A reference that names nothing. |
| `xtce.duplicate` | Two items of one kind with one name in a SpaceSystem, or two root SpaceSystems with one name. |
| `xtce.name` | A name that can't become an MDL name, or two that become the same. |
| `xtce.unsupported` | Something MDL can't represent whose absence would change a decoded value. |
| `xtce.ignored` | A warning: something left out that doesn't change decoded values. |
| `xtce.renamed` | A warning: a name changed to fit MDL's rules. |

An element is reported at its tag, an attribute at its value, and text at
the text. A problem in a definition many packets share is reported once.
Each value of the bundle has the span of the XTCE it came from, so the
reader's and validation's diagnostics point into the XTCE: a field's
`size_bits` at its encoding's `sizeInBits`, a packet at its container.
