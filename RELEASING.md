# Releasing MDL

A release is a tag `vX.Y.Z` on `main` and the GitHub release that the
release workflow ([`.github/workflows/release.yml`](.github/workflows/release.yml))
drafts for it. A maintainer prepares the release commit, tags it and
publishes the draft; the workflow checks, tests, builds and attests in
between.

## What a release holds

- **A version**, the workspace's (`version` under `[workspace.package]` in
  `Cargo.toml`), which every crate shares, as each language binding will
  ([compatibility](spec/compatibility.md) §6). The crates follow semantic
  versioning for their Rust API, and before 1.0 a minor release may
  change it. What a release reads and writes (the IR, the store format,
  the result format) follows the compatibility rules whatever the
  version, and the changelog states it.
- **Notes**: the version's section of [CHANGELOG.md](CHANGELOG.md).
- **morphc for each platform**, `morphc-X.Y.Z-<target>.tar.gz`, holding
  `morphc`, both licenses and the README, with its SHA-256 beside it in
  `morphc-X.Y.Z-<target>.tar.gz.sha256`. The targets are
  `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, statically
  linked so they run on any Linux distribution, and `aarch64-apple-darwin`,
  macOS on Apple silicon. They're built with the Rust the workflow pins
  (`RUST`), from the committed `Cargo.lock`, with nothing cached, and each
  runs the README's worked example before it's packaged. The macOS binary
  isn't signed with an Apple Developer ID or notarized.
- **The Elixir binding's native library for each platform**,
  `libmdl_nif-vX.Y.Z-nif-2.15-<target>.so.tar.gz` (rustler_precompiled's
  name), with its `.sha256`, for `x86_64-unknown-linux-gnu`,
  `aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`,
  `aarch64-unknown-linux-musl`, `aarch64-apple-darwin` and
  `x86_64-apple-darwin` (`scripts/nif.sh`). The glibc ones are built on
  AlmaLinux 8 and checked to need no glibc newer than 2.28; the musl ones
  link musl dynamically and need `libgcc` (Alpine's package of that name).
  On each platform the binding's tests run on the packaged library,
  loaded as a Hex install would load it, before it's released.
- **`SHA256SUMS`**, every archive's SHA-256 in one file.
- **A build provenance attestation for each archive**: a signed statement,
  kept by GitHub, of the repository, commit and workflow run that built
  it, which `gh attestation verify` checks.

The Elixir binding isn't on Hex yet, nor are the crates on crates.io
(`publish = false`): Elixir users depend on this repository at the
release's tag, which builds the native library from source. The binding
goes to Hex, as the package `mdl` at the same version, with the crates on
crates.io, so that its package can build from source too; step 9
publishes it then. As `mix.exs` defines the package today, it holds no
native source and downloads its native library from the release.

## Once, in the repository's settings

- **Release immutability** (Settings, General, Releases, "Enable release
  immutability"). A published release's assets can't then be changed or
  deleted, and its tag can't be moved or deleted while the release
  exists; a deleted release's tag name can't be used again. Drafts stay
  editable, which is why the workflow makes one and you publish it. It
  applies to releases published after it's turned on.
- **A tag ruleset for `v*`** that lets only maintainers create, move or
  delete release tags. The workflow refuses a tag whose commit isn't on
  `main` either way, but anyone who can push a tag can start a run.

## The toolchain

`RUST` in the release workflow is the Rust every release is built and
tested with. Bump it on purpose, in a commit of its own, once `just check`
passes on that Rust. The minimum supported Rust, 1.88 (`rust-version` in
`Cargo.toml`), doesn't move with it; raising that is a decision of its own,
and the changelog says so.

## Making a release

1. **Set the version.** On a branch, set `version` under
   `[workspace.package]` in `Cargo.toml`, and the Elixir binding's
   `@version` in `bindings/elixir/mix.exs` to the same, then update the
   three lockfiles that record the workspace crates' versions. Each
   command changes only those version lines:

   ```sh
   cargo update --workspace
   cargo update --workspace --manifest-path fuzz/Cargo.toml
   cargo update --workspace --manifest-path bindings/elixir/native/mdl_nif/Cargo.toml
   ```

2. **Write the changelog.** In `CHANGELOG.md`, move what's under
   `## [Unreleased]` into a new section headed `## [X.Y.Z] - YYYY-MM-DD`,
   dated the day you'll tag, and leave `## [Unreleased]` above it, empty.
   The section says which IR version, store format and result format the
   release reads and writes (`morphc --version` prints them) and what
   changed since the last release. At the end of the file, add the
   version's link and start `[Unreleased]`'s comparison at the new tag.

3. **Check.** Run `just check`. It ends with what the workflow checks
   first, which `just release-check` runs alone: the tag (by default `v`
   and the workspace's version) names the workspace's version, the
   changelog has a dated section for it that says something, and the
   lockfiles are current. `just release-notes` prints the notes the
   release will have.

4. **Merge** the branch to `main` once CI passes.

5. **Dry run.** Start the release workflow by hand on `main` (Actions,
   release, Run workflow, or `gh workflow run release.yml --ref main`). It
   runs the checks, tests and builds a tag would, the binding's tests on
   each native library included, keeps the archives as the run's
   artifacts and releases nothing. Download the archive for a platform you
   have from the run's page and run `morphc --version`.

6. **Tag** `main` and push the tag:

   ```sh
   git switch main
   git pull
   git tag -a vX.Y.Z -m "MDL X.Y.Z"
   git push origin vX.Y.Z
   ```

   The workflow checks that the tagged commit is on `main`, then tests and
   builds it, attests the archives and makes a **draft** release. A tag
   with a hyphen, such as
   `v0.2.0-rc.1`, makes a pre-release; the workspace's version and the
   changelog's heading are spelled the same way (`0.2.0-rc.1`).

7. **Review the draft** under the repository's Releases: the notes, and
   nineteen files (morphc's three archives and the six native libraries,
   each with its `.sha256`, and `SHA256SUMS`). Download the archive for
   your platform from the draft and check it:

   ```sh
   shasum -a 256 -c morphc-X.Y.Z-<target>.tar.gz.sha256
   gh attestation verify morphc-X.Y.Z-<target>.tar.gz --repo zthacker/mdl \
     --signer-workflow zthacker/mdl/.github/workflows/release.yml
   ```

   The checksum comes from where the archive does, so it catches a damaged
   download, not a replaced one. The attestation is what shows the archive
   is the one this repository's release workflow built.

8. **Publish** the draft. This is the step that makes the release public.

9. **Publish the Elixir package to Hex**, once the binding goes there
   (not yet; until then, skip this step). From a clean checkout of the
   tag, with your Hex account:

   ```sh
   git switch --detach vX.Y.Z
   scripts/release.sh hex vX.Y.Z
   ```

   It refuses unless the checkout is the tag with no changes, since Hex
   publishes the working tree. It then runs `mix
   rustler_precompiled.download MDL.Native --all`, which downloads every
   native library from the published release and writes their SHA-256s
   to `checksum-Elixir.MDL.Native.exs`: a Hex install loads only a library
   with one of those checksums. It checks the file names exactly the
   libraries `MDL.Native` downloads, each with the SHA-256 the release's
   `SHA256SUMS` gives, then runs `mix hex.publish`, which lists the
   package's files (the checksum file among them) and asks before
   publishing. The checksum file is removed afterwards, whatever happens:
   in a checkout, its presence makes the binding download its library
   instead of building it.

   The download needs the release published and the repository public,
   and it compiles the binding first, from source, so it needs Rust 1.91
   or later and Elixir 1.18 or later.

## If something goes wrong

- **Before publishing**, fix what's wrong on `main`, then delete the draft
  release (if one was made) and the tag, and tag again:

  ```sh
  git push --delete origin vX.Y.Z
  git tag -d vX.Y.Z
  ```

  Re-running a failed workflow run builds the same commit again, so it
  helps only for a failure outside the repository (a runner, the
  network). The release job can't make a draft where one exists for the
  tag already.
- **After publishing**, never move or reuse a tag: people, lockfiles and
  attestations refer to it. Release a patch version instead. With release
  immutability on, GitHub enforces this.
- **After publishing to Hex**, a version can be reverted only for a short
  time (`mix hex.publish --revert X.Y.Z`; Hex's documentation gives the
  window). After that, retire it (`mix hex.retire mdl X.Y.Z invalid
  --message "..."`) and release a patch version.
