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

# Every test, with the property tests at 10,000 cases each (`check` runs
# 256), optimized but with overflow checks and debug assertions on
props:
    PROPTEST_CASES=10000 cargo test --workspace --profile props

# Format all code
fmt:
    cargo fmt --all

# The compiler, e.g. `just morphc build examples/toml/mission`
[positional-arguments]
morphc *args:
    cargo run -q -p morpheus-morphc -- "$@"
