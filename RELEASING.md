# Releasing MDL

A release is a GitHub release of a commit on `main`, and the tag `vX.Y.Z`
on that commit. A maintainer pushes a commit that changes the workspace's
version; the release workflow
([`.github/workflows/release.yml`](.github/workflows/release.yml)) checks,
tests, builds and attests that commit and drafts the release; the
maintainer reads the draft over and publishes it, which creates the tag.

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
- **A tag ruleset for `v*`** that lets only the repository's admins
  create, move or delete release tags (Settings, Rules, Rulesets, New
  ruleset, New tag ruleset: target the pattern `v*`, turn on "Restrict
  creations", "Restrict updates" and "Restrict deletions", and add the
  Repository admin role to the bypass list). Publishing a draft creates
  its tag as whoever publishes it, so an admin's Publish goes through; in
  an organization whose maintainers publish too, add the Maintain role to
  the bypass list. It stops anyone else making a `v` tag by hand.

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
   dated the day you'll publish, and leave `## [Unreleased]` above it,
   empty. The section says which IR version, store format and result
   format the release reads and writes (`morphc --version` prints them)
   and what changed since the last release. At the end of the file, add
   the version's link and start `[Unreleased]`'s comparison at the new
   tag.

3. **Check.** Run `just check`. It ends with what the workflow checks
   first, which `just release-check` runs alone: the tag (`v` and the
   workspace's version) is the one the release will have, the changelog
   has a dated section for it that says something, and the lockfiles are
   current. `just release-notes` prints the notes the release will have.

4. **Push the version to `main`**, by merging the branch once CI passes
   or by pushing the commit. A push that changes the version is what
   starts a release; any other push to `main` leaves releases alone.
   From here until you publish, merge nothing else to `main`: if
   something goes wrong, the release is made again from `main`'s head,
   which should still be this commit.

5. **Watch the run.** On the repository's page on GitHub:
   1. Click the **Actions** tab, then **release** in the list of
      workflows on the left.
   2. Click the newest run, named after your commit's message.
   3. Wait until every job has a green check: plan, verify and test,
      morphc for each of three platforms, the Elixir native library for
      each of six, and draft the release. A job with a red cross failed:
      click it, then its red step, for the log, and see "If something goes
      wrong" below.

6. **Review the draft.**
   1. Click the **Code** tab, then **Releases** in the right-hand column.
   2. Open **MDL X.Y.Z**, marked **Draft**. Read the notes, and check it
      holds nineteen files: morphc's three archives and the six native
      libraries, each with its `.sha256`, and `SHA256SUMS`.
   3. Optionally, download the archive for your platform and check it
      (this needs the `gh` CLI):

      ```sh
      shasum -a 256 -c morphc-X.Y.Z-<target>.tar.gz.sha256
      gh attestation verify morphc-X.Y.Z-<target>.tar.gz --repo zthacker/mdl \
        --signer-workflow zthacker/mdl/.github/workflows/release.yml
      ```

      The checksum comes from where the archive does, so it catches a
      damaged download, not a replaced one. The attestation is what shows
      the archive is the one this repository's release workflow built,
      from the release's commit.

7. **Publish.** On the draft, click the pencil (**Edit**), check that
   "Set as the latest release" is ticked (unless it's a pre-release,
   which can't be), and click **Publish release** at the bottom. This creates the tag `vX.Y.Z` on the release's commit and
   makes the release public.

8. **Pull the tag** into your checkout, if you want it there:
   `git fetch --tags`.

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

## A dry run

The release workflow can run by hand, without releasing anything: the same
checks, tests and builds, the archives kept as the run's artifacts.

1. Click the **Actions** tab, then **release** on the left.
2. Click **Run workflow** (on the right, above the list of runs).
3. Under "Use workflow from", pick the branch to try. Leave "Draft a
   release" unticked.
4. Click the green **Run workflow** button, then the run that appears, and
   watch it as in step 5. The archives are under **Artifacts** at the
   bottom of the run's page.

## If something goes wrong

- **Before publishing**, nothing public exists yet, and no tag either:
  1. If a draft was made, delete it: open it under **Releases**, click the
     trash can (**Delete**), and confirm.
  2. Fix what's wrong on `main`. A fix that doesn't change the version
     doesn't start a release by itself.
  3. Release `main`'s head by hand: **Actions**, **release**, **Run
     workflow**, "Use workflow from" **main**, tick **Draft a release**,
     and click **Run workflow**. Then carry on from step 5.

  For a failure outside the repository (a runner, the network), the
  failed run's **Re-run failed jobs** button builds the same commit
  again. The release job refuses to make a second draft of a version:
  delete the first.
- **After publishing**, never move or reuse a tag: people, lockfiles and
  attestations refer to it. Release a patch version instead. With release
  immutability on, GitHub enforces this.
- **After publishing to Hex**, a version can be reverted only for a short
  time (`mix hex.publish --revert X.Y.Z`; Hex's documentation gives the
  window). After that, retire it (`mix hex.retire mdl X.Y.Z invalid
  --message "..."`) and release a patch version.
