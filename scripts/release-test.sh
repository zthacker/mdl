#!/usr/bin/env bash
# Tests of scripts/release.sh, run by `just check`. Each case runs it in a
# scratch tree with a Cargo.toml and CHANGELOG.md of its own, and stand-ins:
# a cargo whose `fetch` fails for a manifest named in STALE, as cargo's does
# for a stale lockfile, and for a fetch without --locked; and, for `hex`, a
# git, a mix and a curl that act out a checkout, rustler_precompiled's
# download, Hex and the release's SHA256SUMS.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

mkdir "$scratch/bin"
cat > "$scratch/bin/cargo" <<'EOF'
#!/usr/bin/env bash
[ "$1" = fetch ] || { echo "cargo $1 wasn't expected" >&2; exit 1; }
case " $* " in
    *" --locked "*) ;;
    *) echo "cargo fetch without --locked" >&2; exit 1 ;;
esac
for arg in "$@"; do
    case " ${STALE:-} " in
        *" $arg "*)
            echo "error: cannot update the lock file" >&2
            exit 101
            ;;
    esac
done
EOF
chmod +x "$scratch/bin/cargo"

# HEAD_COMMIT and TAG_COMMIT, the checkout's and the tag's; DIRTY, what
# `git status --porcelain` says, and IGNORED, what it says with --ignored.
cat > "$scratch/bin/git" <<'EOF'
#!/usr/bin/env bash
case "$1 $2" in
    "rev-parse HEAD") echo "${HEAD_COMMIT:-c0ffee}" ;;
    "rev-parse "*) echo "${TAG_COMMIT:-c0ffee}" ;;
    "status --porcelain")
        if [ "${3:-}" = --ignored ]; then printf '%s' "${IGNORED:-}"; else printf '%s' "${DIRTY:-}"; fi
        ;;
    *) echo "git $* wasn't expected" >&2; exit 1 ;;
esac
EOF
# DOWNLOADED, the checksum file rustler_precompiled's download writes;
# PUBLISHED, a file `mix hex.publish` makes.
cat > "$scratch/bin/mix" <<'EOF'
#!/usr/bin/env bash
case "$1" in
    deps.get) ;;
    rustler_precompiled.download) cp "$DOWNLOADED" checksum-Elixir.MDL.Native.exs ;;
    hex.publish) touch "$PUBLISHED" ;;
    *) echo "mix $* wasn't expected" >&2; exit 1 ;;
esac
EOF
# SUMS, the release's SHA256SUMS, served for the release's URL only.
cat > "$scratch/bin/curl" <<'EOF'
#!/usr/bin/env bash
url=${!#}
[ "$url" = "https://example.com/mdl/releases/download/v1.2.3/SHA256SUMS" ] ||
    { echo "curl $url wasn't expected" >&2; exit 22; }
cat "$SUMS"
EOF
chmod +x "$scratch/bin/git" "$scratch/bin/mix" "$scratch/bin/curl"

# tree NAME VERSION: a tree whose workspace is at VERSION, with stdin as
# its CHANGELOG.md.
tree() {
    local dir="$scratch/$1"
    mkdir -p "$dir/scripts" "$dir/fuzz" "$dir/bindings/elixir/native/mdl_nif" \
        "$dir/bindings/elixir/lib/mdl" "$dir/.github/workflows"
    cp "$here/release.sh" "$dir/scripts/"
    printf '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "%s"\nrepository = "%s"\n' \
        "$2" "https://example.com/mdl" > "$dir/Cargo.toml"
    cat > "$dir/bindings/elixir/lib/mdl/native.ex" <<'EOF'
defmodule MDL.Native do
  use RustlerPrecompiled,
    base_url: "https://example.com/mdl/releases/download/v#{version}",
    targets: ~w(
      aarch64-apple-darwin
      x86_64-unknown-linux-gnu
    ),
    nif_versions: ["2.15"]
end
EOF
    cat > "$dir/.github/workflows/release.yml" <<'EOF'
jobs:
  nif:
    strategy:
      matrix:
        include:
          - target: x86_64-unknown-linux-gnu
            runner: ubuntu-24.04
          - target: aarch64-apple-darwin
            runner: macos-15

  release:
    needs: [nif]
EOF
    printf 'defmodule MDL.MixProject do\n  @version "%s"\nend\n' "$2" \
        > "$dir/bindings/elixir/mix.exs"
    for license in LICENSE-MIT LICENSE-APACHE; do
        echo "$license text" | tee "$dir/$license" > "$dir/bindings/elixir/$license"
    done
    cat > "$dir/CHANGELOG.md"
}

# run NAME ARGS...: release.sh ARGS in tree NAME, leaving its exit status,
# stdout and stderr in status, out and err.
run() {
    local dir="$scratch/$1"
    shift
    status=0
    out=$(PATH="$scratch/bin:$PATH" "$dir/scripts/release.sh" "$@" 2> "$scratch/err") || status=$?
    err=$(cat "$scratch/err")
}

cases=0
failures=0
fail() {
    echo "FAIL: $1 (exit $status)"
    echo "  stdout: $out"
    echo "  stderr: $err"
    failures=$((failures + 1))
}

# expect WHAT STATUS TEXT: the last run exited with STATUS, and TEXT is in
# its stdout or stderr.
expect() {
    cases=$((cases + 1))
    if [ "$status" != "$2" ] || [[ "$out$err" != *"$3"* ]]; then
        fail "$1"
    fi
}

# expect_notes WHAT TEXT: the last run succeeded and printed TEXT exactly.
expect_notes() {
    cases=$((cases + 1))
    if [ "$status" != 0 ] || [ "$out" != "$2" ]; then
        fail "$1"
    fi
}

changelog='# Changelog

## [Unreleased]

## [1.2.3] - 2026-01-02

The release.

- A change.

## [1.2.2] - 2025-12-01

Older.

[Unreleased]: https://example.com/compare/v1.2.3...HEAD
[1.2.3]: https://example.com/releases/tag/v1.2.3'

tree good 1.2.3 <<< "$changelog"
run good version
expect "the workspace's version" 0 "1.2.3"
run good check
expect "the default tag" 0 "ready to tag v1.2.3"
run good check v1.2.3
expect "a tag naming the version" 0 "ready to tag v1.2.3"
run good check v1.2.4
expect "a tag naming another version" 1 \
    "tag v1.2.4 names 1.2.4, but the workspace's version (Cargo.toml) is 1.2.3"
run good check 1.2.3
expect "a tag without v" 1 "tag 1.2.3 doesn't start with v"
STALE=Cargo.toml run good check
expect "a stale Cargo.lock" 1 \
    "if Cargo.lock is out of date, cargo update --workspace --manifest-path Cargo.toml updates it"
STALE=fuzz/Cargo.toml run good check
expect "a stale fuzz/Cargo.lock" 1 \
    "if fuzz/Cargo.lock is out of date, cargo update --workspace --manifest-path fuzz/Cargo.toml updates it"
STALE=bindings/elixir/native/mdl_nif/Cargo.toml run good check
expect "the Elixir binding's stale native lockfile" 1 \
    "if bindings/elixir/native/mdl_nif/Cargo.lock is out of date, cargo update --workspace --manifest-path bindings/elixir/native/mdl_nif/Cargo.toml updates it"
run good notes
expect_notes "the notes, without the heading or the blank lines around them" \
    "The release.

- A change."
run good notes v1.2.2
expect_notes "the last section's notes, without the link definitions" "Older."
run good notes 1.2.3
expect "notes for a tag without v" 1 "tag 1.2.3 doesn't start with v"
run good notes v9.9.9
expect "notes for a version with no section" 1 "CHANGELOG.md has no section for 9.9.9"
run good
expect "no command" 2 "usage: scripts/release.sh"

tree binding 1.2.3 <<< "$changelog"
printf 'defmodule MDL.MixProject do\n  @version "1.2.2"\nend\n' \
    > "$scratch/binding/bindings/elixir/mix.exs"
run binding check
expect "an Elixir binding of another version" 1 \
    "bindings/elixir/mix.exs's @version is 1.2.2, but the workspace's version is 1.2.3"

tree licenses 1.2.3 <<< "$changelog"
echo "an older MIT license" > "$scratch/licenses/bindings/elixir/LICENSE-MIT"
run licenses check
expect "the Elixir binding's stale license copy" 1 \
    "bindings/elixir/LICENSE-MIT isn't the same as LICENSE-MIT; copy it again"

tree targets 1.2.3 <<< "$changelog"
native="$scratch/targets/bindings/elixir/lib/mdl/native.ex"
sed 's/aarch64-apple-darwin/x86_64-apple-darwin/' "$native" > "$native.new" && mv "$native.new" "$native"
run targets check
expect "native libraries the workflow doesn't build" 1 \
    "the release workflow's nif job builds aarch64-apple-darwin x86_64-unknown-linux-gnu but"

tree elsewhere 1.2.3 <<< "$changelog"
native="$scratch/elsewhere/bindings/elixir/lib/mdl/native.ex"
sed 's|example.com/mdl|example.org/fork|' "$native" > "$native.new" && mv "$native.new" "$native"
run elsewhere check
expect "downloads from another repository" 1 \
    "downloads from https://example.org/fork/releases/download/v#{version}, not https://example.com/mdl's releases"

# hex: the release's SHA256SUMS, and checksum files rustler_precompiled's
# download might write.
linux=libmdl_nif-v1.2.3-nif-2.15-x86_64-unknown-linux-gnu.so.tar.gz
mac=libmdl_nif-v1.2.3-nif-2.15-aarch64-apple-darwin.so.tar.gz
printf '%s  %s\n' 1111 morphc-1.2.3-x.tar.gz aaaa "$linux" bbbb "$mac" > "$scratch/sums"
printf '%%{\n  "%s" => "sha256:bbbb",\n  "%s" => "sha256:aaaa"\n}\n' "$mac" "$linux" \
    > "$scratch/checksums"
printf '%%{\n  "%s" => "sha256:aaaa",\n  "%s" => "sha256:bbbb"\n}\n' "$mac" "$linux" \
    > "$scratch/checksums-swapped"
printf '%%{\n  "%s" => "sha256:aaaa"\n}\n' "$linux" > "$scratch/checksums-one"
export SUMS=$scratch/sums

# hex CASE CHECKSUMS [VAR=VALUE...]: release.sh hex in a fresh tree, the
# download writing CHECKSUMS and the rest of the environment as given;
# published says whether it published, and whether it left the checksum
# file behind.
hex() {
    local name=$1 downloaded=$2
    shift 2
    tree "hex-$name" 1.2.3 <<< "$changelog"
    local assignment
    for assignment in "$@"; do
        export "${assignment?}"
    done
    DOWNLOADED=$downloaded PUBLISHED=$scratch/published-$name run "hex-$name" hex
    for assignment in "$@"; do
        unset "${assignment%%=*}"
    done
    published=no
    [ -f "$scratch/published-$name" ] && published=yes
    if [ -f "$scratch/hex-$name/bindings/elixir/checksum-Elixir.MDL.Native.exs" ]; then
        published="$published, the checksum file left"
    fi
}

# expect_published WHAT YES_OR_NO
expect_published() {
    cases=$((cases + 1))
    [ "$published" = "$2" ] || fail "$1: published $published"
}

hex ok "$scratch/checksums"
expect "publishing the release's libraries" 0 "the checksum file has the release's 2 native libraries"
expect_published "publishing the release's libraries" yes

hex elsewhere "$scratch/checksums" TAG_COMMIT=beef
expect "publishing from another commit than the tag" 1 "the checkout isn't v1.2.3"
expect_published "publishing from another commit than the tag" no

hex dirty "$scratch/checksums" "DIRTY= M lib/mdl.ex"
expect "publishing with changes" 1 "the checkout has changes"
expect_published "publishing with changes" no

hex ignored "$scratch/checksums" "IGNORED=!! bindings/elixir/lib/scratch.ex"
expect "publishing with an ignored file in lib" 1 "bindings/elixir/lib holds files git ignores"
expect_published "publishing with an ignored file in lib" no

hex missing "$scratch/checksums-one"
expect "a checksum file without every library" 1 "the checksum file names $linux but MDL.Native downloads"
expect_published "a checksum file without every library" no

hex swapped "$scratch/checksums-swapped"
expect "a checksum that isn't the release's" 1 \
    "$mac downloaded with SHA-256 aaaa, but the release's SHA256SUMS gives bbbb"
expect_published "a checksum that isn't the release's" no

tree unlisted 1.2.4 <<< "$changelog"
run unlisted check
expect "a version with no section" 1 \
    "CHANGELOG.md has no dated section for 1.2.4; add one headed ## [1.2.4] - YYYY-MM-DD"

tree undated 1.2.3 <<'EOF'
## [1.2.3]

The release.
EOF
run undated check
expect "an undated section" 1 "CHANGELOG.md has no dated section for 1.2.3"

# The dots in a version are dots, not any character.
tree dots 1.2.3 <<'EOF'
## [1x2x3] - 2026-01-02

The release.
EOF
run dots check
expect "a heading matching the version only as a pattern" 1 \
    "CHANGELOG.md has no dated section for 1.2.3"

tree empty 1.2.3 <<'EOF'
## [1.2.3] - 2026-01-02

## [1.2.2] - 2025-12-01

Older.
EOF
run empty check
expect "an empty section" 1 "CHANGELOG.md's section for 1.2.3 says nothing"
run empty notes
expect "notes for an empty section" 1 "CHANGELOG.md has no section for 1.2.3"

tree prerelease 2.0.0-rc.1 <<'EOF'
## [2.0.0-rc.1] - 2026-03-04

A release candidate.
EOF
run prerelease check v2.0.0-rc.1
expect "a pre-release" 0 "ready to tag v2.0.0-rc.1"
run prerelease notes v2.0.0-rc.1
expect_notes "a pre-release's notes" "A release candidate."

tree unversioned 1.2.3 <<< "$changelog"
printf '[workspace]\nmembers = []\n' > "$scratch/unversioned/Cargo.toml"
run unversioned version
expect "a workspace without a version" 1 "Cargo.toml's [workspace.package] has no version"

if [ "$failures" -ne 0 ]; then
    echo "release.sh: $failures of $cases cases failed"
    exit 1
fi
echo "release.sh: $cases cases passed"
