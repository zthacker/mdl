# mdl tasks. Run `just` to list them.

default:
    @just --list

# fmt check, clippy, tests (the definition of done)
check:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

# Format all code
fmt:
    cargo fmt --all

# The compiler, e.g. `just morphc build examples/definitions`
[positional-arguments]
morphc *args:
    cargo run -q -p morphc -- "$@"
