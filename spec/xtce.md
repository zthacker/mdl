# XTCE

Status: normative from IR v2 (`mdl_version` 2) on. The key words MUST,
MUST NOT, SHOULD and MAY are to be interpreted as described in RFC 2119.

The XTCE importer reads OMG XTCE documents (versions 1.0 to 1.3)
and produces a bundle (`frontends.md`). This document says which XTCE it
reads, what each construct becomes, and what it reports. The reference
implementation is the `mdl-xtce` crate.

XTCE describes a space system's packets whole, headers included. An
imported packet is one concrete sequence container with the entries of
every container it inherits from; the restriction criteria along the way
become its identify keys. An imported command is one concrete
meta-command with the arguments and entries of every meta-command it
inherits from, encoded as Yamcs encodes it. What XTCE says that MDL can't
represent is reported as `frontends.md` §2 requires: an error when leaving
it out would change a decoded or encoded value, a warning otherwise.

## 1. Documents

An import reads one or more documents. Each MUST be well-formed XML, its
namespaces too (no prefix bound to none, `xmlns:p=""`), without a DTD
[`xtce.syntax`], whose root is a `SpaceSystem` in
`http://www.omg.org/space/xtce` (XTCE 1.0 and 1.1),
`http://www.omg.org/spec/XTCE/20180204` (XTCE 1.2) or
`http://www.omg.org/spec/XTCE/20250214` (XTCE 1.3) [`xtce.document`]. A
root SpaceSystem in no namespace, or in a name documents written for Yamcs
give XTCE 1.2's or 1.3's namespace (its `https` spelling, and
`SpaceSystem.xsd` below it over `http` or `https`), is read as XTCE, as
that version's where it's one, with a warning [`xtce.namespace`]: Yamcs
reads elements by their local names, so such documents load there. Either way, every element is in its root's
namespace. Each root is a top-level SpaceSystem; two with one name are an
error at the later one, which isn't imported [`xtce.duplicate`].
References may cross documents.

Every element the importer reads is checked. An attribute or child it
doesn't read is an error [`xtce.unsupported`], since it can't tell whether
leaving it out would change a value, unless this document lists it as
left out (§7), which is a warning [`xtce.ignored`]. Attributes in the XML
Schema instance and XML namespaces (`xsi:schemaLocation`, `xml:lang`)
describe the document and are skipped; attributes in other namespaces are
warnings, and elements in a namespace other than the root's, XInclude
among them, are errors.

SpaceSystems nest at most 64 deep, and aggregates (an aggregate whose
member is an aggregate, and so on) at most 32, MDL's bound on struct
nesting; deeper ones are unsupported. Real documents come nowhere near
either, and the bounds keep a crafted document from exhausting the
importer.

## 2. Names and references

Each SpaceSystem names its parameter types, parameters, containers,
argument types, meta-commands, meta-commands' command containers and
SpaceSystems, each kind on its own; a name used twice for one kind is an
error at the second, with the first as its first location, and the second
isn't imported [`xtce.duplicate`].

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
XTCE writes them (`Packet_ID.APID`) or as Yamcs does (`Packet_ID/APID`).
XTCE 1.3 also lets a reference name an array's element (`Cells[2]`) and
an argument reference go into an argument's members or elements
(`Target.Id`); both are unsupported [`xtce.unsupported`], since MDL refers
to arrays and arguments only whole. A reference that names nothing is an
error at the reference [`xtce.reference`].

## 3. The bundle

An import produces one set, and the family it's in unless it joins
another input's:

- **Family.** Its name is the importer's option, or the first root
  SpaceSystem's name lowercased with `_` for each run of characters other
  than letters and digits; a name that doesn't start with a letter needs
  the option [`xtce.name`]. Its encoding is `binary`. Its byte order is
  little-endian when every number wider than a byte that a packet reads
  or an argument writes is, and big-endian otherwise, each field in the
  other order naming its own (`decoding.md` §2.1), so a big-endian header
  ahead of little-endian data is described as it is. A command's
  constants are bit patterns, and don't count. Its alias is the option,
  or a fixed value: the first root SpaceSystem's name. An alias read from
  metadata makes that name the family's one metadata name, since the
  family has to declare it and XTCE has nowhere to.
- **Joining a family.** With the importer's `join` option, naming a
  family another input defines (which `morphc` gives the import, [build
  files](build-files.md) §2), the import defines no family. Its set is in
  that one, as another layout version of it, and its fields take that
  family's byte order, each in the other naming its own. The import names
  its set, and gives it a version (the option `set_version`) exactly when
  the family has a version locator. A family no other input defines (the
  closest name suggested), a JSON one, an alias option other than the
  family's alias, a missing set name or version, a version for a family
  without a version locator, a `family` option beside `join`, or a
  `set_version` without `join` is an error [`xtce.options`].
- **Set.** Its name is the option, or the family's. It has one packet for
  each concrete sequence container, in document order, a struct type for
  each embedded container and each array element that isn't a scalar
  (§5), and a command packet for each concrete meta-command, in document
  order (§6).
- **Catalog.** Each parameter a packet holds is a param; an aggregate
  parameter is one param per member, or one vector param when it's a
  composite (§4.1). A param's name is the parameter's
  alias in the `MDL` namespace (`<Alias nameSpace="MDL"
  alias="eps.bus_voltage"/>`), taken as it is, or else its SpaceSystem
  path and name, each part lowercased with `_` for each run of other
  characters and none at either end, joined by dots:
  `/BogusSAT/SC001/Bus_Voltage` is `bogussat.sc001.bus_voltage`. A
  member's name adds its own part (`demosat.adcs.rate.x`). Two parameters
  with one catalog name are an error at the later, with the earlier as its
  first location [`xtce.name`]. A param's description is the parameter's
  `shortDescription` and `LongDescription`, or a member's. Each concrete
  meta-command is a catalog command (§6).
- **Referring to another catalog.** With the importer's catalog role
  `refer` (which `morphc` gives a build's inputs that don't own its
  catalog, [build files](build-files.md) §3), the import adds nothing to
  the catalog. A parameter with an alias in the `MDL` namespace is a field
  mapped to the param of that name, a member's name adding its part as
  above, which validation checks the catalog has (`unknown_canonical`);
  any number of parameters may map to one name. A parameter without one
  is a field with no catalog name. Meta-commands are as §6.1 says. The
  import is given that catalog [`xtce.options` if it isn't], which its
  command packets read (§6.1). What
  validation compares with the catalog stays checked: a field's type,
  unit and states against its param, a command packet's arguments
  against its command's. Descriptions aren't carried, the catalog's
  being the owner's.
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
| `IntegerParameterType` | `uint` (`unsigned`) or `int` (`twosComplement`, or XTCE 1.0's `twosCompliment`; `signMagnitude` and `onesComplement` as `signed`, IR 2.1), 1 to 64 bits | `i64`, or `u64` when `signed="false"` |
| `FloatParameterType` | `float` (IEEE 754, 32 or 64 bits), or an integer encoding with or without a calibrator | `f64` |
| `EnumeratedParameterType` | an integer encoding with a `states` conversion from its `EnumerationList` | `enum`, its labels in order, each once |
| `BooleanParameterType` | `bool` for one bit, else `uint` | `bool`: nonzero is true |
| `StringParameterType` | `string`, `utf8` (the default), `ascii` (`US-ASCII`) or `latin1` (`ISO-8859-1`); a `Fixed` size in whole bytes, up to a one-byte `TerminationChar`, or both: a fixed-size buffer whose value ends at the terminator | `str` |
| `BinaryParameterType` | `bytes`, a fixed size in whole bytes, or a size another parameter gives | `bytes` |
| `AggregateParameterType` | one field per member, depth first | one per member |
| `ArrayParameterType` | an array of one dimension: scalar elements, or a struct type of an aggregate's members | the elements' param, or one per member, each element a sample with its index |
| `RelativeTimeParameterType` | its `Encoding`'s integer or float encoding; a `scale` or `offset` other than 1 and 0 is a `linear` conversion | `f64`, in `s` (`seconds`, the default), `ms` (`milliseconds`), `us` (`microseconds`), `ns` (`nanoseconds`), `ps` (`picoseconds`, or XTCE 1.2's `picoSeconds`), `min` (`minutes`), `h` (`hours`) or `d` (`days`); a duration in `months` or `years` has no unit, since their lengths vary [`xtce.ignored`], and any other `units` is an error [`xtce.invalid`] |

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
  §2.1 reads it. Any other byte order, and
  `bitOrder="leastSignificantBitFirst"`, are unsupported. So is an
  `ErrorDetectCorrect` in a value's encoding: XTCE doesn't say whether the
  value holds the check or is covered by it, and 1.3 deprecates it for a
  container's (§5).
- A `DefaultCalibrator` is the field's conversion. A
  `PolynomialCalibrator` is `linear` up to degree 1, else `poly` (degree
  at most 32); terms with one exponent add up. A `SplineCalibrator` of
  `order` 0 is a `step` table, of order 1 a `linear` one, and
  `extrapolate` carries over. This follows XTCE: Yamcs interpolates and
  extrapolates linearly whatever the order and `extrapolate` say.
- Unsupported, each an error: `ContextCalibratorList`,
  `MathOperationCalibrator`, splines of order 2 or more or whose points
  name different orders, a calibrator on an integer, enumerated or boolean
  type (MDL's calibrations give floats), BCD and packed BCD integers,
  floats other than IEEE 754 binary32 and binary64, text
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
alarms of other types, relative time types included. A `DefaultAlarm`
XTCE 1.3 marks `disabled` gives no limits, since no alarm applies
[`xtce.ignored`].

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

### 4.1 Composites

XTCE has no vectors, so a vector or a quaternion is an aggregate of
numbers, or an array of them. Ancillary data in the MDL namespace makes
one a composite (`decoding.md` §7.4): one `vec3` or `quat` sample from its
members, rather than a sample of each. Yamcs leaves that data out, so a
document reads the same there.

- **`MDL.frame`**, `<AncillaryData name="MDL.frame">ECI_J2000</AncillaryData>`,
  on an aggregate or array parameter or its parameter type, makes it a
  composite in that frame: `ECEF`, `ECI_J2000`, `BODY`, `NED`, `ENU` or
  `GEODETIC` (anything else is `xtce.invalid`). A parameter's says it for
  that parameter, over its type's, so one type can serve vectors in
  different frames. A member whose type is marked is a composite of its
  own, so an aggregate of a position and a velocity gives two.
- **`MDL.members`** names the members in MDL's order, x, y, z for a vec3
  and w, x, y, z for a quat, separated by spaces: an aggregate's members by
  name, `q4 q1 q2 q3` for a quaternion sent scalar last, and an array's
  elements by the indices XTCE numbers them with, from its
  `StartingIndex`, `4 1 2 3` for one numbered 1 to 4. Without it, the
  order is the members'. It MUST name each member once [`xtce.invalid`],
  and comes with `MDL.frame`, from the parameter or its type, each on its
  own [`xtce.invalid` otherwise].
- A composite has three members (a vec3) or four (a quat), each a number:
  an integer or a float, not an enumeration, a boolean, a string or an
  aggregate. An array's count is fixed, an XTCE 1.1 array's given by its
  entry's `DimensionList`: a count another parameter gives can't be a
  composite's [`xtce.invalid`].
- Each name is given once in an `AncillaryDataSet`, and the MDL namespace
  has only these two; a scalar parameter, one of a single value, takes
  neither [`xtce.invalid`].

An aggregate's members are fields, as any aggregate's are (`Rate_x`), and
an array is an array entry, its elements' paths (`Att[0]`) counting from
0 whatever XTCE numbers them from; either way they're in the document's
order and map to no param of their own. The packet lists the composite
with its param's name and its members' paths in MDL's order. The param's
name is the parameter's (§3), a marked member's adding its part
(`sat.state.pos`). An import that owns its catalog defines the param: a
vec3 or quat in the composite's frame and its members' unit. It can't
define one for `GEODETIC` positions, whose param is in `ECEF` or
`ECI_J2000`, nor for members in different units, which a param doesn't
have [`xtce.unsupported`]: such a composite is imported referring to a
catalog that defines its param. A referring import maps the composite onto
the catalog's param named by the parameter's MDL alias [`xtce.invalid`
without one], and validation checks its frame and its members' units
against it, as for any composite.

A composite's members are one of each in the packet, so a marked parameter
in a repeated entry, in an embedded container or in an array's elements
is unsupported. Commands have no vectors: in a command container, a marked
parameter's members are fields as any aggregate's are. Other ancillary
data, and ancillary data anywhere else, is left out [`xtce.ignored`].

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
  entries, and a `TimeAssociation` on an entry or a parameter (it times
  the value by another parameter, and an MDL sample takes its packet's
  time), are unsupported.
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
- **Sizes.** A container's `BinaryEncoding` with a `SizeInBits` of a
  `FixedValue` is padding after the container's entries, up to that many
  bits from where they're placed from: the start of the packet, as for
  `containerStart`, so a header's size says where the next container's
  entries start and a packet's is the whole packet; for an embedded
  container, its own start, as Yamcs reads it. Yamcs reads a container's
  size only there, so padding a container that isn't embedded is a
  warning [`xtce.differs`]: what follows it, or the packet's length, isn't
  what Yamcs reads or writes. Entries that take more than the size are an
  error [`xtce.invalid`], and so is a size that isn't a positive number of
  bits. A size after an entry a condition can leave out, or one whose size
  another value gives, is unsupported: the padding would have no fixed
  size. So is a size after entries placed out of order (the last one
  listed not reaching furthest), since MDL places the padding after the
  last one listed, and so are a size another value gives or a lookup
  list chooses, and transform algorithms. Before XTCE 1.3 a
  container's `BinaryEncoding` is a value's, so it has a `SizeInBits`
  [`xtce.invalid`], and a `byteOrder` or `bitOrder` other than its
  default is unsupported (each value's encoding gives its own); 1.3's
  has neither.
- **Checks.** An XTCE 1.3 `ErrorDetectCorrect` in a container's
  `BinaryEncoding` holds checks over the bytes of each packet built on
  the container. A check naming the parameter that holds its value
  (`parameterRef`) makes that parameter's field a checksum entry
  (`decoding.md` §5.6), which decoding verifies and encoding works out;
  the parameter gives no sample, and isn't a catalog param unless a field
  in another packet maps it. One naming none, as before 1.3 none can,
  has nothing to be checked against, and is left out [`xtce.ignored`].
  - A `CRC` is MDL's `crc`: `width` (whole bytes, 8 to 64 bits, else
    unsupported), `Polynomial` as `poly`, `InitRemainder` as `init` and
    `FinalXOR` as `xor_out` (`hexBinary`, hexadecimal digits without
    `0x`, each fitting the width, the polynomial without its top bit
    [`xtce.invalid`]; 0 when left out),
    `reflectData` as `reflect_in` and `reflectRemainder` as
    `reflect_out`. These are the names of Michael Barr's CRC code, whose
    model is the CRC catalogue's that MDL uses. `direction` of
    `leastSignificantBitFirst` is unsupported: XTCE doesn't say how it
    differs from the reflections.
  - A `Checksum` named `sum8` is MDL's 8-bit `sum`, with a
    `hashSizeInBits` of 8 or none. `sum16`, `sum24` and `sum32` are
    unsupported, since XTCE doesn't say whether they add bytes or words,
    as are the others XTCE names (`fletcher16` isn't MDL's `iso8473`,
    whose check bytes differ) and an `InputAlgorithm`.
  - An `XOR` is MDL's `xor`. A `Parity` is unsupported: it's one bit.
  - A check covers the bytes from `bitsFromReference` bits after the
    start of the packet (whole bytes, else unsupported) up to its own
    value, at least one, as every MDL checksum does (`decoding.md` §5.6):
    a check counted from where its value starts, or past it, is
    imported as written and refused when building. XTCE doesn't say where a check's bytes end, so its parameter
    is a field at the packet's top level that only other checks follow
    in bits (each covering those before it, as MDL's do); anything else
    after it is unsupported. Entries follow one another in the order
    they're listed; where some are placed from the start of the packet,
    each has to be at a fixed place to tell, and a check in a packet where
    one isn't is unsupported. A packet built on the container that
    doesn't hold the parameter has nothing to check it against, so it's
    imported without the check [`xtce.ignored`]. `reference="end"`, a
    check on an embedded container, a member of an aggregate, and a
    parameter that isn't an unsigned integer of the check's width whose
    raw value is its value, in the family's byte order, always present,
    are unsupported.
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

## 6. Commands

A command is encoded as Yamcs encodes it, so a document written for Yamcs
sends the same bytes with MDL; §6.3 lists where MDL's encoding rules make
it otherwise.

### 6.1 Meta-commands

- **Commands.** Each `MetaCommand` that isn't `abstract` is a catalog
  command and a command packet. Referring to another catalog (§3), one
  with an alias in the `MDL` namespace is a command packet encoding the
  command of that name, whose arguments the packet names as below, and
  isn't a catalog command; one without is left out [`xtce.ignored`].
  The packet then carries what the vendor says of each argument beyond
  the owner's command, as its own limits (`encoding.md` §2.6), so the
  vendor's limits hold whatever the owner's command allows:
  - the argument's range and default (§6.2, IR 2.2), in its unit where
    that isn't the command argument's; in the command argument's own,
    the unit isn't written, since limits are in it without one;
  - for an enumerated argument whose labels are some of the command
    argument's states and not all, the states it accepts, in the
    catalog's order (IR 2.3). Labels that are all of them write nothing,
    the packet accepting every state as it does without a list, and a
    label the catalog lacks is reported by validation at the label
    (`canonical_type`);
  - for an argument with no initial value whose command argument has a
    default, `required` (IR 2.3): Yamcs requires a value for it, and the
    default is the other vendors'.

  Its chain is the
  meta-commands it inherits from (`BaseMetaCommand`), root first, then
  itself; one that inherits from itself is an error [`xtce.invalid`].
- **Arguments.** A command's arguments are those each meta-command in its
  chain declares (`ArgumentList`), in chain order, less those an
  `ArgumentAssignment` gives a value. An assignment, in a meta-command's
  `BaseMetaCommand`, names an argument of a meta-command above it
  [`xtce.reference`] that isn't assigned already [`xtce.invalid`]. A
  meta-command that declares an argument twice, or one it inherits, is an
  error [`xtce.duplicate`]. An argument no entry writes changes no bytes
  and isn't imported [`xtce.ignored`]; Yamcs requires it, records it and
  writes nothing.
- **Names.** A command's catalog name is the meta-command's alias in the
  `MDL` namespace, else its path and name, as a param's (§3); params and
  commands share the catalog's names [`xtce.name`]. An argument's name is
  its alias in the `MDL` namespace, else its name as a part of a catalog
  name (`Heater-Id` is `heater_id`); two of one command with one name are
  an error [`xtce.name`]. A command packet is named for its meta-command,
  qualified as packets are (§3). The descriptions are the meta-command's
  and each argument's own.
- **Packets.** A command packet holds the entries of each meta-command's
  `CommandContainer`, in chain order, placed from the start of the packet
  as a telemetry packet's entries are (§5). A `CommandContainer`'s
  `BaseContainer` names a command container as references name items
  (§2) [`xtce.reference`], and MUST name the container of the nearest
  meta-command above it that has one, which is what Yamcs encodes ahead of
  it. Another base, a telemetry container, and a base for a meta-command
  without a `BaseMetaCommand` are unsupported. The sent command's
  container's restriction criteria give its parameter entries their
  values; Yamcs reads no other criteria, so those of an abstract
  meta-command's container are left out [`xtce.ignored`]. Yamcs doesn't
  read a `CommandContainerSet`, so it's left out too. A
  `CommandContainer`'s size and checks are a telemetry container's (§5):
  encoding writes the padding as zeros, and works out each check.
- **Arguments in entries.** An `ArgumentRefEntry`, or an
  `ArrayArgumentRefEntry`, names an argument its own meta-command declares,
  as Yamcs reads it [`xtce.reference`], and an argument is in one entry of
  a container. It's a field of the argument's type that encodes the
  argument, named for it; for an aggregate, a field per member, named for
  the argument and the members joined by `_` (`Target_Id`); for an array,
  an array (§6.2), which an `ArrayArgumentRefEntry` names
  [`xtce.invalid`]. Yamcs skips an `ArrayArgumentRefEntry` and writes
  nothing for it; MDL writes the array, as XTCE says [`xtce.differs`]. A
  subset of an array (an entry's own `DimensionList`) is unsupported. For
  an assigned argument it's a constant instead, the assigned value as its
  raw value: an integer read as a default is (§6.2), an enumeration's
  label, a boolean's labels (true being written as 1), the text, for a
  string, and for an aggregate a JSON object of its members, completed as
  §6.2 Defaults says (a member nothing gives a value is an error
  [`xtce.invalid`], as Yamcs refuses it). An integer MUST lie within the
  argument's valid range, which Yamcs checks when it sends the command
  [`xtce.invalid`]. A value for a float, bytes or array argument, however
  it's spelled, is unsupported: MDL constants are single integers and
  text, and Yamcs writes a float through its own arithmetic (§6.3).
- **Parameters in entries.** A `ParameterRefEntry` naming the parameter
  that holds one of the command's checks is that check (§5). Any other is
  a constant field for the parameter, or one for each member of an
  aggregate, named as a telemetry field is (§3). It holds the value the sent command's criteria
  give the parameter or member, as Yamcs fills it: an `==` comparison in a
  `ComparisonList` on the sent command's container's `BaseContainer`, read
  as a comparison's value is (§5). A comparison with another operator, and
  a `Comparison`, `BooleanExpression` or other criterion outside a
  `ComparisonList`, aren't read, as Yamcs reads none of them
  [`xtce.ignored`]; two values for one parameter are an error
  [`xtce.invalid`]. A parameter or member the criteria give no value is
  unsupported: Yamcs sends its latest value, which MDL can't. So is a
  calibrated, float or text parameter's engineering value (MDL constants
  are raw integers and text), a label on several raw values, and an array
  parameter. Yamcs fails on a signed integer or a label here; they're read
  as XTCE says. With `useCalibratedValue="false"` the value is the raw
  value, as XTCE says, where Yamcs reads it as the engineering value
  regardless [`xtce.differs`].
- **Fixed values.** A `FixedValueEntry` is an unsigned integer constant
  named for its `name`, or `fixed1`, `fixed2` and so on, counted in the
  packet. It holds `binaryValue`'s low `sizeInBits` bits (all of them by
  default), most significant first; more than 64 bits are fields of 64
  bits named with `_1`, `_2` and so on, the first taking the bits left
  over. `binaryValue` is hexadecimal, an odd number of digits read with a
  leading 0, with at least `sizeInBits` bits [`xtce.invalid`]; higher bits
  that aren't zero are dropped, as Yamcs drops them [`xtce.ignored`].
- **What a command packet can't hold.** Every entry of an MDL command
  packet is written, so an entry with an `IncludeCondition` is
  unsupported. So are repeated entries and container entries.

### 6.2 Argument types

Argument types, in `ArgumentTypeSet`, are read as parameter types of the
same kind are (§4), with these differences:

- Integer, float, enumerated, boolean, string, binary, relative time,
  aggregate and array types are supported; absolute times are
  unsupported, as for parameters. Every version of XTCE's schema names the
  relative time argument type's element `RelativeTimeAgumentType`; it's
  read, and so is `RelativeTimeArgumentType`.
- **Aggregates.** An aggregate argument is a catalog argument per member,
  named for the argument and each member's name as a catalog name part,
  joined by `_` (`Target` with members `Id` and `Range` is `target_id` and
  `target_range`), each with the member's type and description; a member
  name changed to fit MDL's field names is reported [`xtce.renamed`]. A
  member's default is what the argument's initial value gives it,
  completed as Defaults says. An aggregate argument type's own
  `initialValue` (XTCE 1.3), a JSON object of its members, gives each
  member it names a value beneath the member's own initial value, as
  Defaults says; a document before XTCE 1.3 has none, Yamcs doesn't read
  it there, and it's left out [`xtce.ignored`]. An aggregate holding an
  array is unsupported.
- **Arrays.** An array argument of one dimension is a list argument
  written by an array entry: of its elements, for a scalar element type;
  for an aggregate's, of records, a struct type named for the meta-command
  and the field with `_element` (`Track_Waypoints_element`), qualified as
  containers' struct types are where meta-commands share a name (§3), and
  where another struct type has the name, by its SpaceSystems' names and
  then `__` between the meta-command's name and the field's
  [`xtce.renamed`],
  holding a field per member that takes its own list argument,
  named as an aggregate's members are (`targets_id`), so the lists fill
  the records together. A fixed dimension gives the array a fixed count,
  which is the list's least and most values. An ending index another
  argument gives makes that argument the array's count field (Sizes).
  Several dimensions, elements whose size another value gives, a starting
  index another value gives, and an XTCE 1.1 array type without a
  `DimensionList` are unsupported.
- **Sizes.** An argument another argument's size reads
  (`ArgumentInstanceRef`), an array's ending index or a string's or
  binary's `DynamicValue`, is the count or length field MDL works out from
  the value it measures, so it isn't a catalog argument [`xtce.ignored`].
  Its slope and intercept, and an array's starting index, are the field's
  `multiply` and `add`, as a parameter's size is read (§4); a list's
  bounds are the counts the argument's range (or its field) holds, up to
  the 4294967295 a list's bounds hold. The field MUST come before what it
  measures [`xtce.unsupported`], and be an unsigned integer whose raw
  value is its value, whose range its field holds [`xtce.invalid`]. Each
  of these is unsupported, since an MDL field gives one value one size: an
  argument that's both a count and a length, a length for two values, an
  aggregate or list as a size, and a size of an argument no entry writes.
  A size a parameter gives is unsupported too, as Yamcs refuses it.
  An assigned size argument is a constant, and the array it counts has
  that fixed count, as Yamcs then refuses any other; a string or binary it
  measures, that fixed length (a string NUL-padded, as Yamcs pads it).
- **Calibrators.** A float type's `DefaultCalibrator` runs from
  engineering to raw, as Yamcs applies it. A polynomial of degree 1, raw
  = a + b·v, is the field's inverse as written (`linear`, scale b and
  offset a), which MDL evaluates as Yamcs does (multiply, then add), and
  its algebraic inverse is the field's conversion (`poly`, −a/b and 1/b),
  for decoding a command. A spline of order 1 whose calibrated values only
  rise or only fall is a linear table with its points swapped (a point's
  calibrated value is the raw value, its raw value the engineering one),
  which MDL reads backwards (`encoding.md` §3.1); it extrapolates when the
  spline does. Decoding a command needs the calibrator's exact inverse, so
  a constant polynomial, a higher degree, an order 0 spline, a spline that
  rises and falls, and a math operation are unsupported.
- **Relative times** are read as relative time parameters are (§4): a
  duration in its unit, the encoding's scale and offset a `linear`
  conversion, which MDL inverts exactly. Yamcs doesn't encode relative
  time arguments, so they follow XTCE. A calibrator in the encoding, and a
  scale of 0, are unsupported.
- **Ranges.** A number type's `ValidRange`, alone (XTCE 1.0 and 1.1) or
  the one in a `ValidRangeSet` (1.1 and 1.2), is the argument's `range`.
  An exclusive end is the nearest double inside the range, which keeps
  the same values in it (`minExclusive="0"` is a `min` of 5e-324). With
  both an inclusive and an exclusive end, the inclusive one is taken, as
  Yamcs takes it, and the other left out [`xtce.ignored`]. More than one
  range, which Yamcs doesn't load either, is unsupported. An integer
  type's ends are taken exactly, as XTCE's `xs:long` ends are, and written
  as decimal strings past ±(2^53 − 1). Yamcs
  checks the range against the engineering value whatever
  `validRangeAppliesToCalibrated` says. An uncalibrated type's raw and
  engineering values are one; on a calibrated type, `false` puts the range
  on raw values, as XTCE says, and the argument's range is the engineering
  values whose raw values lie in it [`xtce.differs`].
- **Without a range.** An integer argument with no valid range takes what
  both its encoding holds and its type does (`signed`, and `sizeInBits`,
  32 by default), which Yamcs checks a value against; an end its catalog
  type has anyway (a `u64`'s 0) is left out; the others are exact, so a
  60-bit field's argument reaches 2^60 − 1. An uncalibrated float written
  as an integer takes what MDL's rounding keeps in the encoding, from just
  above `low − 0.5` to just below `high + 0.5`, for each end its valid
  range doesn't give (§6.3). A calibrated argument, and a relative time
  written as an integer, takes the engineering values whose raw values the
  encoding holds, worked out through the calibrator or the scale and
  offset, cut at ±(2^53 − 1) [`xtce.differs`]; a float encoding holds so
  much that the cut is its range. If every such value lies past
  ±(2^53 − 1), the argument is unsupported, and if a given end lies past
  every such value, it's an error [`xtce.invalid`]. A spline that doesn't
  extrapolate keeps a range to its points. An unsigned integer type over a
  two's complement encoding is an `i64` argument, its range keeping it at
  0 and above, since an MDL command writes a `u64` argument only to a
  `uint` field.
- **Defaults.** `initialValue`, the argument's or else its type's, is the
  argument's default, read as Yamcs reads it [`xtce.invalid`]: an integer
  in decimal, or after `0x`, `0o` or `0b` in that base, with `_` allowed
  between digits; a finite number in decimal notation (Java's `1.5f` and
  hexadecimal floats aren't read); a boolean's labels, or `true` and
  `false`; an enumeration's label; binary as hexadecimal digits; text as
  it is; for a relative time, an `xs:duration` without years or months,
  whose lengths vary (`PT1.5S`), in the argument's unit; for an aggregate,
  a JSON object of its members (`{"Id": 3}`); and for an array, a JSON
  array of its values, or of objects for records. The argument's own
  initial value is used over its type's (an array type's is XTCE 1.3's,
  and a Yamcs extension before it). An aggregate's value, given or
  initial, is completed as Yamcs completes it: from the first member it
  leaves out down, each member's own initial value (an aggregate member's
  a JSON object of its members), else what its aggregate type's initial
  value gives it (XTCE 1.3), outermost first, then the member's type's.
  A member's value that should be an object of its members and isn't, and
  a member an object names that the aggregate doesn't have, are errors
  [`xtce.invalid`]; so is a value of the wrong type that a member's own
  initial value overrides. An initial value its encoding can't write
  (text longer than its buffer, a size its size tag can't hold) is the
  document's to fix, and validation reports it (`encoding.md` §2.2), where
  Yamcs refuses only the commands that would send it.
- **Text.** A fixed-size string is padded with NULs, as Yamcs pads it:
  `Fixed` alone, or with a `TerminationChar` of `00`, is a fixed buffer
  with a NUL terminator (`encoding.md` §3.4). A fixed buffer with another
  terminator is unsupported, since Yamcs writes one terminator and then
  NULs where MDL fills the buffer with the terminator. A terminated string
  is written the same by both, and both refuse a value longer than its
  buffer. A leading size is a size tag before the text, a field named for
  the string's with `_size`, which MDL works out; a size another argument
  gives is that argument's field (Sizes).
- **Enumerations.** A label on several values is encoded as the first of
  them, as Yamcs encodes it; the others aren't imported [`xtce.ignored`].
- **Left out** [`xtce.ignored`]: an argument type's alarms (XTCE 1.0
  allows them), `SizeRangeInCharacters` and `restrictionPattern`; a time
  association on a command entry, which means nothing for a value a
  command writes.

### 6.3 Where MDL encodes otherwise

MDL encodes arguments by its own rules (`encoding.md` §3), which differ
from Yamcs's in these cases. The first seven are warnings where they are
[`xtce.differs`]:

- An uncalibrated float written as an integer: Yamcs drops the fraction
  (2.7 is 2), and MDL rounds to the nearest integer, halves away from zero
  (3). So an end of its valid range that Yamcs keeps inside the encoding
  but MDL's rounding takes outside it is narrowed to the last value MDL
  keeps: a byte's `maxExclusive` of 256 is a `max` of 255.49999999999997.
  An end outside the encoding either way is the document's to fix, and
  validation reports it.
- A float type of 32 bits, XTCE's default `sizeInBits`, written in 64:
  Yamcs narrows the value to 32 bits first, and MDL writes it whole.
- An f64 argument without a range whose encoding holds values past
  ±(2^53 − 1): MDL refuses them, since the importer works its range out
  within ±(2^53 − 1).
- An argument's spline that doesn't extrapolate: Yamcs extrapolates it
  whatever it says, and MDL keeps to its points, as XTCE says, refusing a
  value outside them.
- A range on raw values (`validRangeAppliesToCalibrated="false"`): Yamcs
  checks it against the engineering value, and MDL against the raw value,
  as XTCE says.
- A command's criteria value with `useCalibratedValue="false"`: Yamcs
  reads it as the engineering value, and MDL as the raw value, as XTCE
  says.
- An `ArrayArgumentRefEntry`: Yamcs skips it, and MDL writes the array,
  as XTCE says.
- A raw value its field can't hold: Yamcs keeps the low bits (a negative
  value through a calibrator into an unsigned field is 0), and MDL refuses
  it (`raw_range`, or `out_of_range` where a range keeps it in the field).
- A calibrated value exactly half way between two integers: Yamcs rounds
  it toward positive infinity (−2.5 is −2), and MDL away from zero (−3).
- A calibrated 32-bit float type (XTCE's default `sizeInBits`): Yamcs
  narrows the value to 32 bits before calibrating it, and MDL calibrates
  it whole. Rounding to an integer field hides the difference but at the
  edges of a raw step.
- A spline read backwards: MDL's arithmetic (`encoding.md` §3.1) differs
  from Yamcs's interpolation, so the raw value before rounding can differ
  in its last bit.
- A fixed-size binary value of the wrong length: Yamcs pads a shorter one
  with zeros and cuts a longer one short, and MDL refuses both
  (`bad_length`).
- A NUL in a NUL-padded string: Yamcs writes it, and MDL refuses it
  (`contains_terminator`).
- A size argument: Yamcs fills an array's size in when it's left out and
  has no initial value of its own or its type's (else it takes that value
  and refuses a list of any other length), and checks it when it's given;
  it requires a string's or binary's, padding a shorter value to it (and
  cutting a longer binary value short). MDL works every size out from the
  value, so a caller doesn't give it.
- A variable string's `maxSizeInBits`: Yamcs refuses a longer value when
  it sends the command, and MDL bounds it only as its length field does.
- An array of records' initial value (a JSON array of objects): Yamcs
  can't read one, and MDL reads each object as an aggregate's value.

## 7. What is left out

These don't change a decoded or encoded value. Each is a warning where it
is [`xtce.ignored`]:

- descriptions of anything but parameters, aggregate members,
  meta-commands and arguments;
- aliases outside the `MDL` namespace, and ancillary data other than the
  MDL namespace's on a parameter or an aggregate parameter type (§4.1);
- parameters' initial values, change thresholds, what alarms say beyond
  MDL's limits (§4), parameter types' valid ranges, display formats
  (`ToString`), a string type's `characterWidth` of 8 (wider characters
  are unsupported), and parameter properties other than time associations,
  validity conditions included (they set a value's quality, not the
  value);
- expected rates and idle patterns;
- the document `Header`, algorithms, streams, services, operational
  status, and a SpaceSystem's system and asset types (XTCE 1.3);
- a boolean type's labels other than `True` and `False`, and a variable
  string's `maxSizeInBits` (for a command's, §6.3 says what Yamcs does);
- a meta-command's system name, transmission constraints, interlock,
  significance, verifiers, parameters to set and alarms to suspend; block
  meta-commands, meta-command references (the meta-command is imported
  where it's defined), and a `CommandContainerSet` (§6);
- what §6 lists of commands: an argument no entry writes, an argument
  that gives another's size, an argument type's alarms, an aggregate
  argument type's `initialValue` before XTCE 1.3, text size ranges and
  restriction
  patterns, an enumeration label's later values, a command entry's time
  association, and restriction criteria Yamcs doesn't fill parameter
  entries from.

## 8. Diagnostics

| Code | What |
| --- | --- |
| `xtce.syntax` | Text that isn't well-formed XML (with namespaces), or has a DTD. |
| `xtce.document` | A document that isn't XTCE, or no documents. |
| `xtce.invalid` | Something XTCE doesn't allow: a missing attribute, a value that isn't a number, a container that inherits from itself, criteria that can't all hold. |
| `xtce.reference` | A reference that names nothing. |
| `xtce.duplicate` | Two items of one kind with one name in a SpaceSystem (SpaceSystems among them), or two root SpaceSystems with one name. |
| `xtce.name` | A name that can't become an MDL name, or two that become the same. |
| `xtce.unsupported` | Something MDL can't represent whose absence would change a decoded or encoded value. |
| `xtce.ignored` | A warning: something left out that doesn't change decoded or encoded values. |
| `xtce.renamed` | A warning: a name changed to fit MDL's rules. |
| `xtce.differs` | A warning: a command MDL encodes otherwise than Yamcs (§6.3), or a container size MDL pads to and Yamcs doesn't read (§5). |
| `xtce.namespace` | A warning: a document read as XTCE whose root is in no namespace, or in a misspelling of XTCE 1.2's (§1). |
| `xtce.options` | Import options the import can't follow: a joined family the build doesn't define or that's JSON, an alias or version that disagrees with it, a `family` option beside `join`, a `set_version` without `join`, a missing set name, or a referring import given no catalog (§3). Reported at the import's first root SpaceSystem, which says which import it is. |

An element is reported at its tag, an attribute at its value, and text at
the text. A problem in a definition many packets share is reported once.
Each value of the bundle has the span of the XTCE it came from, so the
reader's and validation's diagnostics point into the XTCE: a field's
`size_bits` at its encoding's `sizeInBits`, a packet at its container.
