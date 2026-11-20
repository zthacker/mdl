#!/bin/sh
# Rust in one of the release workflow's Linux images: rustup-init of a
# pinned version, checked against the SHA-256 recorded here, which installs
# the toolchain RUST names (release.yml pins it). POSIX sh, for Alpine's
# busybox too. Then `. "$HOME/.cargo/env"` puts it on PATH.
set -eu

: "${RUST:?RUST names the toolchain to install}"
rustup_version=1.29.3

case "$(uname -m)" in
    x86_64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *)
        echo "no rustup-init is pinned for $(uname -m)" >&2
        exit 1
        ;;
esac
# musl's ldd says so; glibc's names GNU libc.
if ldd --version 2>&1 | grep -qi musl; then
    libc=musl
else
    libc=gnu
fi
triple=$arch-unknown-linux-$libc

# rustup-init's SHA-256 for each image's platform, from
# static.rust-lang.org/rustup/archive/$rustup_version/<triple>/rustup-init.sha256.
case "$triple" in
    x86_64-unknown-linux-gnu) sum=b1c8cb14e30889e7e2999e6b69e62d9e9b9ffc0f4df8180564d306d9680e6c55 ;;
    aarch64-unknown-linux-gnu) sum=9f608c142e0e38a78957d0661ba1dfaf0b6fb37184cfaa4b9a0aa57af6184282 ;;
    x86_64-unknown-linux-musl) sum=1552169aba2836777136ccff7a381e95c17233b5d8250cd5e10ae3cb971bfcfc ;;
    aarch64-unknown-linux-musl) sum=956deacdc27ab82ae7ac821d5d3e9e95e4795d476d17efe99743bb8531ca1031 ;;
esac

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl --proto '=https' --tlsv1.2 -fsSL -o "$work/rustup-init" \
    "https://static.rust-lang.org/rustup/archive/$rustup_version/$triple/rustup-init"
echo "$sum  rustup-init" > "$work/rustup-init.sha256"
(cd "$work" && sha256sum -c rustup-init.sha256)
chmod +x "$work/rustup-init"
"$work/rustup-init" -y -q --profile minimal --default-toolchain "$RUST"
