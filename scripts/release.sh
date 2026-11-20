#!/usr/bin/env bash
# Release checks and notes (RELEASING.md), shared by `just release-check`,
# `just release-notes` and the release workflow, so the checks a maintainer
# runs before tagging are the ones the workflow runs on the tag.
#
#   scripts/release.sh version       the workspace's version
#   scripts/release.sh check [TAG]   TAG (v and the workspace's version by
#                                    default) names the workspace's version,
#                                    which the Elixir binding's mix.exs states,
#                                    CHANGELOG.md has a dated section for it,
#                                    and the lockfiles (the workspace's, the
#                                    fuzz harness's, the Elixir binding's
#                                    native library's) match the manifests
#   scripts/release.sh notes [TAG]   that section's text: the release notes
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' Cargo.toml)
if [ -z "$version" ]; then
    echo "Cargo.toml's [workspace.package] has no version" >&2
    exit 1
fi
tag=${2:-v$version}

# The section of CHANGELOG.md for version $1, without its heading or the
# blank lines around it. It ends at the next heading, or at the link
# definitions that end the file.
section() {
    awk -v heading="## [$1] - " '
        index($0, heading) == 1 { found = 1; next }
        found && (/^## / || /^\[[^]]*\]: /) { exit }
        found { lines[++n] = $0 }
        END {
            first = 1
            while (first <= n && lines[first] ~ /^[[:space:]]*$/) first++
            last = n
            while (last >= first && lines[last] ~ /^[[:space:]]*$/) last--
            for (i = first; i <= last; i++) print lines[i]
        }
    ' CHANGELOG.md
}

# The version a tag names: the tag without its v.
tagged() {
    case "$tag" in
        v*) echo "${tag#v}" ;;
        *)
            echo "tag $tag doesn't start with v" >&2
            exit 1
            ;;
    esac
}

check() {
    local named
    named=$(tagged)
    if [ "$named" != "$version" ]; then
        echo "tag $tag names $named, but the workspace's version (Cargo.toml) is $version" >&2
        exit 1
    fi
    # A binding's version is its runtime release's (compatibility.md §6).
    local elixir
    elixir=$(sed -n 's/^  @version "\(.*\)"$/\1/p' bindings/elixir/mix.exs)
    if [ "$elixir" != "$version" ]; then
        echo "bindings/elixir/mix.exs's @version is ${elixir:-missing}, but the" \
            "workspace's version is $version; a binding's version is its runtime's" >&2
        exit 1
    fi
    local escaped=${version//./\\.}
    if ! grep -qE "^## \[$escaped\] - [0-9]{4}-[0-9]{2}-[0-9]{2}$" CHANGELOG.md; then
        echo "CHANGELOG.md has no dated section for $version; add one headed" \
            "## [$version] - YYYY-MM-DD" >&2
        exit 1
    fi
    if [ -z "$(section "$version" | tr -d '[:space:]')" ]; then
        echo "CHANGELOG.md's section for $version says nothing" >&2
        exit 1
    fi
    # Fails if a lockfile doesn't match the manifests: a release builds
    # exactly the committed Cargo.lock, and the fuzz harness's and the
    # Elixir binding's native library's lockfiles name the workspace
    # crates' versions too (CI builds them with --locked).
    local manifest
    for manifest in Cargo.toml fuzz/Cargo.toml bindings/elixir/native/mdl_nif/Cargo.toml; do
        if ! cargo fetch --locked --quiet --manifest-path "$manifest"; then
            echo "if ${manifest%Cargo.toml}Cargo.lock is out of date," \
                "cargo update --workspace --manifest-path $manifest updates it" >&2
            exit 1
        fi
    done
    echo "ready to tag $tag"
}

# The tag's release notes: its version's section, which must say something.
# (An assignment, not an argument, takes `tagged`'s output, so its failure
# stops the script.)
notes() {
    local named text
    named=$(tagged)
    text=$(section "$named")
    if [ -z "$(printf '%s' "$text" | tr -d '[:space:]')" ]; then
        echo "CHANGELOG.md has no section for $named, or it says nothing" >&2
        exit 1
    fi
    printf '%s\n' "$text"
}

case "${1:-}" in
    version) echo "$version" ;;
    check) check ;;
    notes) notes ;;
    *)
        echo "usage: scripts/release.sh version | check [TAG] | notes [TAG]" >&2
        exit 2
        ;;
esac
