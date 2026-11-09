# TOML examples

Definitions written in MDL's TOML format: the IR spelled in TOML
([spec/toml.md](../../spec/toml.md)). Each file holds part of a bundle and
states its `mdl_version`; a build merges every file of a directory.

| Directory | What it shows |
| --- | --- |
| [`mission`](mission) | One catalog shared by three very different assets: a CCSDS satellite (CUC time, an APID per packet, a calibration table, a command with a CRC), a satellite sending JSON lines (in Fahrenheit, converted to Celsius), and a MAVLink drone (little-endian, time since boot). |
| [`mixed_versions`](mixed_versions) | Two firmware versions on one link. A format byte in every message picks the set that decodes it, so old and new nodes can share the link. |

`cargo test -p mdl-toml --test examples` reads each directory, validates
and builds it, and decodes sample messages through it. From the command
line, `just morphc build examples/toml/mission` builds one.
