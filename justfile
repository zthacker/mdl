# mdl tasks. Run `just` to list them.

default:
    @just --list

# fmt check, clippy, tests (the definition of done)
check:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace
    # mdl again with serde_json's arbitrary_precision, which any other crate in a consumer's build can turn on
    cargo test -p morpheus-mdl --features serde_json/arbitrary_precision
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --quiet

# Format all code
fmt:
    cargo fmt --all

# The compiler, e.g. `just morphc build examples/toml/mission`
[positional-arguments]
morphc *args:
    cargo run -q -p morpheus-morphc -- "$@"
