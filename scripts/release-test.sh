#!/usr/bin/env bash
# Tests of scripts/release.sh, run by `just check`. Each case runs it in a
# scratch tree with a Cargo.toml and CHANGELOG.md of its own, and a
# stand-in cargo whose `fetch` fails for a manifest named in STALE, as
# cargo's does for a stale lockfile, and for a fetch without --locked.
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

# tree NAME VERSION: a tree whose workspace is at VERSION, with stdin as
# its CHANGELOG.md.
tree() {
    local dir="$scratch/$1"
    mkdir -p "$dir/scripts" "$dir/fuzz"
    cp "$here/release.sh" "$dir/scripts/"
    printf '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "%s"\n' "$2" \
        > "$dir/Cargo.toml"
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
