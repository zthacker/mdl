# Versions and compatibility

Status: normative. The key words MUST, MUST NOT, SHOULD and MAY are to be
interpreted as described in RFC 2119.

Consumers keep artifacts (a `def_version` stamped on stored telemetry),
keep stores, and pin a binding. This document says what each version
number covers, and so what an upgrade can and can't break.

## 1. The IR

The IR's version is `major.minor` (`frontends.md` §7).

- A minor version only adds: a key, a kind, an option. A document valid
  at 2.*n* is valid at 2.*n*+1 and means the same there, but for a
  definition §7 says a later release refuses.
- Anything that changes or removes what a document means is a new major
  version. A new major version comes with a way to carry documents across,
  and its readers say whether they read the old one.
- A reader of 2.*n* MUST read every document stating 2.0 to 2.*n*, MUST
  refuse one stating a newer minor version (`unsupported_version`, naming
  the version it needs) or another major one, and MUST refuse a document
  stating less than a feature it uses needs (`feature_version`). Each
  feature's version is listed in `frontends.md` §7 and never changes.

## 2. Artifacts

- An artifact states exactly the lowest version that reads it
  (`frontends.md` §7): what it uses decides, never the version of the
  build that wrote it. Growing the IR never changes an artifact that
  doesn't use what's new, so its identity, and a set's `def_version`, stay
  the same across upgrades.
- Every reader of a major version reads every artifact of that major
  version up to its own minor, for as long as the major version lasts,
  except a definition §7 says a later release refuses. A `def_version`
  kept with telemetry stays readable as long as the store keeps the
  artifact, and stores never remove one (`store.md` §2).
- A frontend's output can change from one release to the next, an
  importer reading more of what a vendor's XTCE says, say, so a set
  rebuilt from unchanged files can get a new digest; the release notes
  say when. An artifact already in a store keeps its identity, and a
  `def_version` kept with telemetry still resolves.
- An artifact never says which optional features it uses beyond its
  version; that would change its identity. A runtime that implements part
  of MDL works the features out from the artifact when it opens it, and
  refuses one it can't decode correctly then, not while decoding.

## 3. Stores

- The manifest's `manifest` key is the store format's version
  (`store.md` §1). A reader MUST refuse a manifest of another version, and
  a writer MUST NOT replace one (`morphc build` refuses, even with
  `--fresh`).
- Changing the manifest's keys or the store's layout is a new store format
  version. Artifact files aren't touched by it: they keep their names and
  bytes.

## 4. The runtime API's result format

The results the runtime API returns ([api](api.md)) have a format version,
1 today, which the runtime states.

- Additive changes keep the version: a new key in a result object, a new
  error code, a new value form for a new IR feature (which appears only
  for sets that use the feature). A consumer MUST ignore keys it doesn't
  know, and SHOULD treat a code it doesn't know as an error it can't
  handle more specifically.
- Removing or renaming a key, changing a value's spelling, or changing
  what a key means is a new format version.
- `morphc`'s JSON reports carry their own version (`"report"`), which
  changes when the results inside them do.

## 5. Diagnostic and error codes

A code names one kind of problem and keeps that meaning: codes may be
added, never reused for something else. Messages are for people and may
change in any release; tools match codes, not messages.

## 6. Releases and bindings

- The crates follow semantic versioning for their Rust API. Before 1.0, a
  minor release MAY change the Rust API; artifacts, stores, the result
  format and codes follow §1 to §5 whatever the crate version.
- Each release states the IR version it reads and writes up to, the store
  format it reads and writes, and the result format version.
- A binding bundles one runtime release, and its version is that
  release's (a binding 0.3.1 calls runtime 0.3.1), so what it reads is
  what that runtime reads. A fix to a binding alone is released as a
  patch release of the runtime too, so the versions stay equal. A binding
  MUST NOT change the result format on its own, nor return values §1 of
  the API doesn't specify.

## 7. What an upgrade can break

A runtime trusts nothing about a store it opens: it validates each build
by its own release's rules, since decoding depends on what validation
guarantees. Validation is corrected as mistakes are found, so a release
can refuse a definition an earlier one accepted. It does so only for a
definition the earlier release would have decoded or encoded wrongly
(values that weren't what the payload held, bytes that weren't what the
command meant), never for one it handled correctly: accuracy comes
first, and a wrong value stopped at open is better than one stored. Such
a refusal is `invalid_build` with the problem's own code among its
problems, and the release notes name the rule and why.

Before 1.0, a release may also refuse to build a definition it doesn't
refuse in a store:

- one that spells something two ways where the IR means one, such as a
  value written out that is already its default, so that one meaning
  keeps one digest. A store an earlier release built with such a
  spelling means what the IR's one spelling does;
- one that can fail to do what it's for in a message it allows, such as
  a checksum that can cover no bytes ([decoding.md](decoding.md) §5.6),
  which checks nothing there. A store an earlier release built with one
  is read as that release read it.

Its release notes name each such rule, and a runtime still opens a store
an earlier release built with what it refuses. From 1.0 on, a build
refuses only what the rule above does.

So upgrading the runtime, or a binding, within a major IR version:

- reads every artifact and store the older one read, but a definition the
  older one handled wrongly, as above;
- leaves every artifact identity and `def_version` as it was;
- may add keys and codes to results, and may change messages;
- may change the Rust API before 1.0 (§6).

Otherwise it refuses something only by its version: an artifact of a
newer minor version, a store of another format, both named in the error.
Rebuilding the definitions with the newer `morphc` shows what a refused
definition needs, at its source.
