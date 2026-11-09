# Artifact stores

A build's artifacts (catalog, families, sets; see
[canonical JSON and artifact identity](canonical-json.md)) are kept in a
store: a directory that any consumer, in any language, can read a build
from. `morphc build --out <dir>` writes one.

## 1. Layout

- Each artifact is a file named `<digest>.json`, where `<digest>` is its
  identity: 64 lowercase hex digits. A writer SHOULD write the artifact's
  canonical bytes, so the file's SHA-256 is its name. A reader MUST check
  the identity of what it reads against the name (canonical-json.md §3)
  and refuse a file that doesn't match.
- `manifest.json` names the latest build. It is a JSON object with exactly
  these keys:

  | Key | Value |
  | --- | --- |
  | `manifest` | `1`, the format's version |
  | `catalog` | the catalog's digest |
  | `families` | an object from each family's name to its digest |
  | `sets` | an object from each set's name to its digest |

  A reader MUST refuse a manifest with another version or another key.
  Each family and set named MUST have that name in its artifact.

```json
{
  "manifest": 1,
  "catalog": "81ad15de19201bb19cba3956546dfa0c47e3b66910b4c11f796de917e892a46b",
  "families": { "ccsds_sat": "74cc510c26ac7222d96e9b55facd66f5a961b36782835ff363a83ac1ac676846" },
  "sets": { "ccsds_sat_v1": "5a0980936f89a197f0d7e49a933b33db7c1d2794ef613f62c9d9b0c161a8b776" }
}
```

## 2. Writing

- Artifacts are never rewritten or removed. A set kept across rebuilds
  still pins the catalog it was first built against, and consumers may
  hold older digests (a `def_version` stamped on stored telemetry).
- Every artifact a named set pins (its family and catalog, which may be
  older than the manifest's catalog) MUST be in the store.
- A writer MUST write each file whole and every artifact before the
  manifest that names it:
  1. write the file under a new temporary name in the store, starting
     with `.` and unique to the writer (created only if no file has that
     name);
  2. sync the file, then rename it to its own name;
  3. once every artifact is renamed, sync the store's directory where
     the platform allows it;
  4. write the manifest the same way, and sync the directory again.

  Then a reader never finds a manifest naming an artifact the store
  lacks, even after a crash or power loss, on platforms that can sync a
  directory. Where one can't (Windows), that holds after a crash of the
  writer, and after power loss only as far as the file system keeps the
  order of renames. A rename over a manifest another process holds open
  can fail; the old manifest is then left as it was.
- An existing artifact file is the store's: a writer MUST NOT replace it,
  and one that doesn't hold the artifact its name says means the store is
  damaged. Its content comes from the definitions, so deleting the file
  and building again rewrites it.
- A store MUST have one writer at a time; `morphc` takes no lock. Two at
  once leave the manifest of whichever renamed it last, which still names
  a complete build.
- Names starting with `.` are temporary files: readers ignore them, and
  ones left by a writer that stopped may be deleted when no writer is
  running.

## 3. Reading

A consumer reads the manifest, then each artifact it needs by digest.
A set decodes against the family and catalog it pins; what a compatible
catalog may change (descriptions, deprecations, limits) is read from the
manifest's catalog.
