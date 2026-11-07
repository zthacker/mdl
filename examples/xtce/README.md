# XTCE examples

Definitions written in OMG XTCE, imported with `mdl-xtce`
([spec/xtce.md](../../spec/xtce.md)).

| File | What it shows |
| --- | --- |
| [`demosat.xml`](demosat.xml) | A CCSDS satellite as XTCE tools describe one: the primary header as aggregates in the root SpaceSystem, and an EPS and an ADCS packet that inherit it and are told apart by APID. It names the header in every way XTCE and Yamcs allow (absolute, relative, `.` and `/` members), maps two parameters to shared catalog names through MDL aliases, and has a calibration polynomial, a spline, an enumeration, a vector aggregate, a little-endian counter in a big-endian packet, and a field present only in one mode. |

`cargo test -p mdl-xtce --test examples` imports each file, validates and
builds it, and decodes sample packets through it. (`morphc build` learns
XTCE in Phase 5.)
