#!/usr/bin/env bash
# Release checks and notes (RELEASING.md), shared by `just release-check`,
# `just release-notes` and the release workflow, so the checks a maintainer
# runs before releasing are the ones the workflow runs on the release.
#
#   scripts/release.sh version [REV] the workspace's version, or the
#                                    version at commit REV
#   scripts/release.sh plan push BEFORE AFTER
#   scripts/release.sh plan dispatch COMMIT BRANCH RELEASE
#                                    what the release workflow does with a
#                                    run, as lines for its step outputs
#                                    (see plan below)
#   scripts/release.sh check [TAG]   TAG (v and the workspace's version by
#                                    default) names the workspace's version,
#                                    which the Elixir binding's mix.exs states,
#                                    the binding's license copies are current,
#                                    CHANGELOG.md has a dated section for it,
#                                    and the lockfiles (the workspace's, the
#                                    fuzz harness's, the Elixir binding's
#                                    native library's) match the manifests,
#                                    and the release workflow builds the
#                                    native libraries MDL.Native downloads
#   scripts/release.sh notes [TAG]   that section's text: the release notes
#   scripts/release.sh hex [TAG]     publish the Elixir binding to Hex from
#                                    the published release: from a clean
#                                    checkout of TAG, its checksum file made
#                                    by downloading every native library and
#                                    each checked against the release's
#                                    SHA256SUMS, then mix hex.publish
set -euo pipefail
cd "$(dirname "$0")/.."

# The version under [workspace.package] in the Cargo.toml on stdin.
workspace_version() {
    sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"$/\1/p'
}

version=$(workspace_version < Cargo.toml)
if [ -z "$version" ]; then
    echo "Cargo.toml's [workspace.package] has no version" >&2
    exit 1
fi
tag=${2:-v$version}
native_ex=bindings/elixir/lib/mdl/native.ex

usage() {
    echo "usage: scripts/release.sh version [REV] | plan push BEFORE AFTER |" \
        "plan dispatch COMMIT BRANCH RELEASE | check [TAG] | notes [TAG] | hex [TAG]" >&2
    exit 2
}

# The workspace's version at commit $1: an error if git can't read its
# Cargo.toml, or that has no version.
version_at() {
    local manifest found
    if ! manifest=$(git show "$1:Cargo.toml" 2> /dev/null); then
        echo "git can't read Cargo.toml at $1" >&2
        return 1
    fi
    found=$(workspace_version <<< "$manifest")
    if [ -z "$found" ]; then
        echo "Cargo.toml at $1 has no [workspace.package] version" >&2
        return 1
    fi
    echo "$found"
}

# What the release workflow does with a run, as `mode=`, `commit=` and
# `version=` lines for its step outputs. The mode is release, dry-run or
# skip.
#
#   plan push BEFORE AFTER          a push to main from BEFORE to AFTER:
#                                   a release of AFTER's version if the push
#                                   changes it, otherwise skip
#   plan dispatch COMMIT BRANCH RELEASE
#                                   run by hand on BRANCH, whose head is
#                                   COMMIT: a dry run, or with RELEASE true,
#                                   a release of COMMIT's version, which only
#                                   main gets
#
# Only an untagged version is released. A release is always of the commit
# the run is for, so its build provenance names the commit it was built
# from.
plan() {
    local mode commit before
    case "${2:-}" in
        push)
            [ $# -eq 4 ] || usage
            commit=$4
            version=$(version_at "$commit")
            if ! before=$(version_at "$3"); then
                echo "so nothing is released: whether the push changed the version can't be" \
                    "told; if it did, run the release workflow on main with release checked" >&2
                mode=skip
            elif [ "$before" = "$version" ]; then
                mode=skip
            else
                mode=release
            fi
            ;;
        dispatch)
            [ $# -eq 5 ] || usage
            commit=$3
            version=$(version_at "$commit")
            case "$5" in
                true)
                    if [ "$4" != main ]; then
                        echo "only main is released, and this run is on $4; leave release" \
                            "unchecked for a dry run" >&2
                        exit 1
                    fi
                    mode=release
                    ;;
                false) mode=dry-run ;;
                *)
                    echo "release is true or false, not $5" >&2
                    exit 1
                    ;;
            esac
            ;;
        *) usage ;;
    esac
    if [ "$mode" = release ] && git rev-parse -q --verify "refs/tags/v$version" > /dev/null; then
        echo "v$version is already tagged; a release needs a new version" >&2
        exit 1
    fi
    printf 'mode=%s\ncommit=%s\nversion=%s\n' "$mode" "$commit" "$version"
}

# The targets MDL.Native downloads a native library for, one a line.
native_targets() {
    sed -n '/targets: ~w(/,/),/{/targets: ~w(/d;/),/d;p;}' "$native_ex" | tr -d ' '
}

# The targets the release workflow's nif job builds, one a line.
workflow_targets() {
    awk '/^  nif:/ {in_nif = 1} /^  release:/ {in_nif = 0} in_nif && $1 == "-" && $2 == "target:" {print $3}' \
        .github/workflows/release.yml
}

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
    # A Hex package holds only its own directory's files, so the Elixir
    # binding keeps copies of the licenses, which must be the repository's.
    local license
    for license in LICENSE-MIT LICENSE-APACHE; do
        if ! cmp -s "$license" "bindings/elixir/$license"; then
            echo "bindings/elixir/$license isn't the same as $license; copy it again" >&2
            exit 1
        fi
    done
    # The package downloads its libraries from this repository's releases.
    local repository base_url
    repository=$(sed -n '/^\[workspace.package\]/,/^\[/s/^repository = "\(.*\)"$/\1/p' Cargo.toml)
    base_url=$(sed -n 's/^ *base_url: "\(.*\)",$/\1/p' "$native_ex")
    if [ "$base_url" != "$repository/releases/download/v#{version}" ]; then
        echo "$native_ex downloads from ${base_url:-nowhere}, not $repository's releases" >&2
        exit 1
    fi
    # A library the package would download that no release built, or one
    # built that it never would, means its targets and the workflow's differ.
    if [ "$(native_targets | sort)" != "$(workflow_targets | sort)" ]; then
        echo "the release workflow's nif job builds" $(workflow_targets | sort) \
            "but $native_ex downloads" $(native_targets | sort) >&2
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

# The Elixir package on Hex (RELEASING.md steps 9 and 10), from the
# published release of the tag.
hex() {
    local named
    named=$(tagged)
    if [ "$named" != "$version" ]; then
        echo "tag $tag names $named, but the workspace's version (Cargo.toml) is $version" >&2
        exit 1
    fi
    # What's published is the working tree: it must be the tag's, unchanged.
    if [ "$(git rev-parse HEAD)" != "$(git rev-parse "$tag^{commit}")" ]; then
        echo "the checkout isn't $tag; publish from it: git switch --detach $tag" >&2
        exit 1
    fi
    if [ -n "$(git status --porcelain)" ]; then
        echo "the checkout has changes; publish from a clean checkout of $tag" >&2
        exit 1
    fi
    # Hex packages all of lib/, ignored files too.
    if [ -n "$(git status --porcelain --ignored -- bindings/elixir/lib)" ]; then
        echo "bindings/elixir/lib holds files git ignores, which Hex would publish" >&2
        exit 1
    fi
    local checksum=$PWD/bindings/elixir/checksum-Elixir.MDL.Native.exs
    # In a checkout the file makes the binding download its library, so it
    # goes, whatever happens.
    trap "rm -f '$checksum'" EXIT
    rm -f "$checksum"
    (cd bindings/elixir && mix deps.get --check-locked &&
        mix rustler_precompiled.download MDL.Native --all)
    # Exactly the libraries MDL.Native downloads, each the release's.
    local repository nif_version sums expected found
    repository=$(sed -n '/^\[workspace.package\]/,/^\[/s/^repository = "\(.*\)"$/\1/p' Cargo.toml)
    nif_version=$(sed -n 's/.*nif_versions: \["\([0-9.]*\)"\].*/\1/p' "$native_ex")
    sums=$(curl -fsSL "$repository/releases/download/$tag/SHA256SUMS")
    expected=$(native_targets | while read -r target; do
        echo "libmdl_nif-v$version-nif-$nif_version-$target.so.tar.gz"
    done | sort)
    found=$(sed -n 's/^ *"\([^"]*\)" => "sha256:\([0-9a-f]*\)",\{0,1\}$/\1 \2/p' "$checksum" | sort)
    if [ "$(cut -d' ' -f1 <<< "$found")" != "$expected" ]; then
        echo "the checksum file names" $(cut -d' ' -f1 <<< "$found") \
            "but MDL.Native downloads" $expected >&2
        exit 1
    fi
    local file hash released
    while read -r file hash; do
        released=$(awk -v file="$file" '$2 == file {print $1}' <<< "$sums")
        if [ "$hash" != "$released" ]; then
            echo "$file downloaded with SHA-256 $hash, but the release's SHA256SUMS" \
                "gives ${released:-none}" >&2
            exit 1
        fi
    done <<< "$found"
    echo "the checksum file has the release's $(wc -l <<< "$found" | tr -d ' ') native libraries"
    (cd bindings/elixir && mix hex.publish)
}

case "${1:-}" in
    version)
        if [ $# -ge 2 ]; then
            version_at "$2"
        else
            echo "$version"
        fi
        ;;
    plan) plan "$@" ;;
    check) check ;;
    notes) notes ;;
    hex) hex ;;
    *) usage ;;
esac
