#!/usr/bin/env bash
# The Elixir binding's precompiled native library (RELEASING.md), built,
# checked and tested as the release workflow does, one target at a time:
#
#   scripts/nif.sh build TARGET   build bindings/elixir/native/mdl_nif for
#                                 TARGET and package it as rustler_precompiled
#                                 downloads it, in dist/
#   scripts/nif.sh check TARGET   check what the packaged library needs of the
#                                 system: for glibc, no glibc newer than 2.28
#                                 (AlmaLinux 8's, which builds it) and no
#                                 private glibc symbol; for every target, only
#                                 the system libraries listed here
#   scripts/nif.sh test TARGET    run the binding's tests on the packaged
#                                 library, loaded as a Hex install would load
#                                 it: from rustler_precompiled's cache, its
#                                 checksum checked, nothing built
set -euo pipefail
cd "$(dirname "$0")/.."

usage="usage: scripts/nif.sh build TARGET | check TARGET | test TARGET"
command=${1:-}
target=${2:-}
if [ -z "$target" ]; then
    echo "$usage" >&2
    exit 2
fi
native_ex=bindings/elixir/lib/mdl/native.ex
# The NIF version the library is built for, as MDL.Native states it.
nif_version=$(sed -n 's/.*nif_versions: \["\([0-9.]*\)"\].*/\1/p' "$native_ex")
if [ -z "$nif_version" ]; then
    echo "$native_ex states no nif_versions" >&2
    exit 1
fi
version=$(scripts/release.sh version)
# rustler_precompiled's name for the library, the file inside the archive.
name="libmdl_nif-v$version-nif-$nif_version-$target.so"
dist=$PWD/dist
archive=$dist/$name.tar.gz
crate=bindings/elixir/native/mdl_nif

sha256() {
    if command -v sha256sum > /dev/null; then
        sha256sum "$@"
    else
        shasum -a 256 "$@"
    fi
}

build() {
    # The crate's own target directory, whatever the environment says, so
    # what's packaged is what this build made.
    export CARGO_TARGET_DIR=$PWD/$crate/target
    # In the crate's directory: cargo reads .cargo/config.toml from where it
    # runs, and that file links musl dynamically.
    (cd "$crate" && cargo build --release --locked --target "$target")
    local built=$CARGO_TARGET_DIR/$target/release
    local library
    case "$target" in
        *-apple-darwin) library=$built/libmdl_nif.dylib ;;
        *) library=$built/libmdl_nif.so ;;
    esac
    local work
    work=$(mktemp -d)
    cp "$library" "$work/$name"
    mkdir -p "$dist"
    # The library alone: without macOS's AppleDouble files, which
    # COPYFILE_DISABLE stops (other tars ignore it), or its extended
    # attributes, which --no-xattrs leaves out (busybox's tar takes no such
    # flag, and writes none).
    local no_xattrs=()
    case "$target" in
        *-apple-darwin) no_xattrs=(--no-xattrs) ;;
    esac
    COPYFILE_DISABLE=1 tar ${no_xattrs[@]+"${no_xattrs[@]}"} -czf "$archive" -C "$work" "$name"
    rm -rf "$work"
    (cd "$dist" && sha256 "$name.tar.gz" > "$name.tar.gz.sha256")
    cat "$archive.sha256"
}

# The oldest glibc the libraries load on: AlmaLinux 8's, which builds them.
glibc_floor=2.28

# The system libraries a library may need, which every system of its kind
# has; anything else would fail at a user's load, not here.
allowed_needed() {
    case "$target" in
        x86_64-unknown-linux-gnu)
            echo "libc.so.6 libm.so.6 libgcc_s.so.1 libpthread.so.0 libdl.so.2 librt.so.1 ld-linux-x86-64.so.2"
            ;;
        aarch64-unknown-linux-gnu)
            echo "libc.so.6 libm.so.6 libgcc_s.so.1 libpthread.so.0 libdl.so.2 librt.so.1 ld-linux-aarch64.so.1"
            ;;
        x86_64-unknown-linux-musl) echo "libc.musl-x86_64.so.1 libgcc_s.so.1" ;;
        aarch64-unknown-linux-musl) echo "libc.musl-aarch64.so.1 libgcc_s.so.1" ;;
        *-apple-darwin)
            echo "/usr/lib/libSystem.B.dylib /usr/lib/libiconv.2.dylib" \
                "/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation"
            ;;
        *)
            echo "no system libraries are listed for $target" >&2
            exit 1
            ;;
    esac
}

check() {
    local work library needed allowed
    work=$(mktemp -d)
    trap "rm -rf '$work'" EXIT
    tar -xzf "$archive" -C "$work"
    library=$work/$name
    allowed=" $(allowed_needed) "
    case "$target" in
        *-apple-darwin)
            # The first entry is the library's own install name.
            needed=$(otool -L "$library" | tail -n +3 | awk '{print $1}')
            ;;
        *) needed=$(objdump -p "$library" | awk '$1 == "NEEDED" {print $2}') ;;
    esac
    local library_needed
    for library_needed in $needed; do
        if [[ $allowed != *" $library_needed "* ]]; then
            echo "$name needs $library_needed, which isn't among the system libraries it may" >&2
            exit 1
        fi
    done
    echo "$name needs only:" $needed
    if [[ $target == *-linux-gnu ]]; then
        local symbols newest
        symbols=$(objdump -T "$library")
        if grep -q 'GLIBC_PRIVATE' <<< "$symbols"; then
            echo "$name uses a private glibc symbol, which ties it to one glibc" >&2
            exit 1
        fi
        newest=$(grep -oE 'GLIBC_[0-9]+(\.[0-9]+)+' <<< "$symbols" | sed 's/GLIBC_//' | sort -uV | tail -1)
        echo "$name needs glibc $newest"
        if [ "$(printf '%s\n%s\n' "$newest" "$glibc_floor" | sort -V | tail -1)" != "$glibc_floor" ]; then
            echo "that's newer than $glibc_floor, the oldest it's meant to load on" >&2
            exit 1
        fi
    fi
}

test_packaged() {
    if [ ! -f "$archive" ]; then
        echo "$archive isn't there; scripts/nif.sh build $target makes it" >&2
        exit 1
    fi
    local cache checksum
    cache=$(mktemp -d)
    checksum=$PWD/bindings/elixir/checksum-Elixir.MDL.Native.exs
    # The checksum file is what makes the binding load a precompiled library
    # (lib/mdl/native.ex), so it exists only for this run. The paths are
    # expanded now: the trap runs after this function's locals are gone.
    trap "rm -rf '$cache' '$checksum'" EXIT
    cp "$archive" "$cache/"
    printf '%%{\n  "%s.tar.gz" => "sha256:%s"\n}\n' "$name" "$(sha256 "$archive" | cut -d' ' -f1)" \
        > "$checksum"
    cd bindings/elixir
    export MIX_ENV=test RUSTLER_PRECOMPILED_GLOBAL_CACHE_PATH=$cache
    unset MDL_BUILD
    mix deps.get --check-locked
    # MDL.Native again, now with the checksum file, so it downloads (here,
    # takes from the cache) instead of building. Every test environment has
    # Rust, so a source build would pass the tests too: the native
    # directory starts empty, as does the project's, which Mix copies into
    # it and where a source build leaves its library, and must then hold
    # exactly the packaged library, hidden files counted.
    local native=_build/test/lib/mdl/priv/native
    rm -rf priv/native "$native"
    mix compile --force --warnings-as-errors
    if [ "$(ls -A "$native" 2> /dev/null)" != "$name" ]; then
        echo "$native holds $(ls -A "$native" 2> /dev/null | tr '\n' ' ');" \
            "it should hold only $name: the binding built its own library, or" \
            "the archive holds more than the library" >&2
        exit 1
    fi
    tar -xzOf "$archive" "$name" | cmp - "$native/$name"
    mix test --warnings-as-errors
}

case "$command" in
    build) build ;;
    check) check ;;
    test) test_packaged ;;
    *)
        echo "$usage" >&2
        exit 2
        ;;
esac
