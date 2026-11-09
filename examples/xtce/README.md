# XTCE examples

Definitions written in OMG XTCE, imported with `mdl-xtce`
([spec/xtce.md](../../spec/xtce.md)).

| File | What it shows |
| --- | --- |
| [`demosat.xml`](demosat.xml) | A CCSDS satellite as XTCE tools describe one: the primary header as aggregates in the root SpaceSystem, and an EPS and an ADCS packet that inherit it and are told apart by APID. It names the header in every way XTCE and Yamcs allow (absolute, relative, `.` and `/` members), maps two parameters to shared catalog names through MDL aliases, and has a calibration polynomial, a spline, an enumeration, a vector aggregate, a little-endian counter in a big-endian packet, and a field present only in one mode. Its commands inherit a telecommand header and opcode from an abstract meta-command, each subsystem fixing the APID and each command its opcode, with an enumeration, a valid range, defaults, an argument without a range, a calibrated current, a NUL-padded string, a float with an exclusive bound, and a list of waypoint records counted by an argument MDL works out. |
| [`imager.xml`](imager.xml) | A payload whose packets have sizes the data gives: an optional secondary header embedded where a flag says so (not a packet of its own), a row of calibrated pixels counted by the packet, a label in a Pascal string, an array of target records, and three temperatures repeated with a spare byte after each. |

`cargo test -p mdl-xtce --test examples` imports each file, validates and
builds it, decodes sample packets through it, and encodes its commands to
exact bytes and decodes them back. (`morphc build` learns
XTCE in Phase 5.)
