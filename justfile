# mdl tasks. Run `just` to list them.

# The toolchain the fuzz recipes build with: the newest nightly, unless
# MDL_FUZZ_TOOLCHAIN names another (CI pins one, so its failures reproduce).
nightly := env("MDL_FUZZ_TOOLCHAIN", "nightly")

default:
    @just --list

# fmt check, clippy, tests (the definition of done)
check:
    cargo fmt --all --check
    cargo fmt --manifest-path fuzz/Cargo.toml --check
    cargo clippy --workspace --all-targets -- -D warnings
    # the fuzz harness builds on stable too; only running it needs nightly
    cargo clippy --manifest-path fuzz/Cargo.toml --locked --all-targets -- -D warnings
    cargo test --workspace
    # mdl again with serde_json's arbitrary_precision, which any other crate in a consumer's build can turn on
    cargo test -p morpheus-mdl --features serde_json/arbitrary_precision
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --quiet

# Every test, with the property tests at 10,000 cases each (`check` runs
# 256), optimized but with overflow checks and debug assertions on
props:
    PROPTEST_CASES=10000 cargo test --workspace --profile props

# Fuzz one target of fuzz/ for a while, 60 s by default (nightly and
# cargo-fuzz): release build with debug assertions and overflow checks,
# 10 s at most an input, 512 MB at most an allocation. Starts from
# fuzz/seeds/<target> and the committed fuzz/corpus/<target>, neither of
# which it writes; new inputs land in fuzz/grown/<target>, or
# fuzz/<into>/<target> (CI's cache), and crashes in fuzz/artifacts/<target>.
fuzz target seconds="60" into="grown":
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{target}}" in
        artifact|canonical|decode|decode_command|layouts|read) dict=json ;;
        bindings|toml) dict=toml ;;
        xtce) dict=xml ;;
        *) dict= ;;
    esac
    # layouts' input is a seed then damage to a payload of any length:
    # without these, libFuzzer keeps to its seeds' 37 bytes.
    length=
    if [ "{{target}}" = layouts ]; then
        length="-max_len=4096 -len_control=0"
    fi
    cd fuzz
    mkdir -p "{{into}}/{{target}}" "corpus/{{target}}"
    cargo +{{nightly}} fuzz run --debug-assertions "{{target}}" "{{into}}/{{target}}" \
        "seeds/{{target}}" "corpus/{{target}}" -- \
        -max_total_time="{{seconds}}" -timeout=10 -rss_limit_mb=2048 -malloc_limit_mb=512 \
        ${dict:+-dict=dicts/$dict.dict} $length

# Add to a target's committed corpus what fuzzing grew that adds coverage
# beyond the seeds and the corpus, up to 4 KB each, removing nothing, so
# history grows only by new coverage; larger inputs stay out of the
# repository (CI's cache keeps its own). The generated targets keep seeds
# only.
fuzz-cmin target: (_corpus-kept target)
    #!/usr/bin/env bash
    set -euo pipefail
    just fuzz-merge "{{target}}" "grown/{{target}}" 4096 "seeds/{{target}}" "corpus/{{target}}"
    mkdir -p "fuzz/corpus/{{target}}"
    find "fuzz/grown/{{target}}" -type f -exec mv {} "fuzz/corpus/{{target}}/" \;

# Minimize a target's committed corpus again from its seeds, up to 4 KB an
# input. It replaces most inputs with smaller ones that cover the same, so
# it's for now and then, when the corpus has drifted (a target's checks or
# the code changed a lot), not for each refresh.
fuzz-reset target: (_corpus-kept target)
    @just fuzz-merge "{{target}}" "corpus/{{target}}" 4096 "seeds/{{target}}"

# The generated targets (layouts, shapes) commit seeds only: libFuzzer
# keeps nearly every seed they're given, the testkit's own code being
# instrumented, so their corpus records the generators' branches, not
# MDL's.
_corpus-kept target:
    #!/usr/bin/env bash
    case "{{target}}" in
        layouts|shapes) echo "{{target}} commits seeds only; nothing to minimize"; exit 1 ;;
    esac

# Shrink fuzz/<dir> to the inputs that add coverage beyond the base
# directories under fuzz/, up to `cap` bytes each (0 for any size). An
# input that crashes the target while merging is an error, not dropped.
fuzz-merge target dir cap +bases:
    #!/usr/bin/env bash
    set -euo pipefail
    cd fuzz
    merged=$(mktemp -d)
    log=$(mktemp)
    before=$(mktemp)
    trap 'rm -rf "$merged" "$log" "$before"' EXIT
    for base in {{bases}}; do
        if [ -d "$base" ]; then
            cp -R "$base"/. "$merged"/
        fi
    done
    ls "$merged" | sort > "$before"
    mkdir -p "{{dir}}"
    # libFuzzer adds to the first directory only what adds coverage to it,
    # each named by its SHA-1; what's there keeps its name. A child that
    # crashes on an input is restarted and the input skipped, so the log is
    # checked for one.
    cargo +{{nightly}} fuzz run --debug-assertions "{{target}}" "$merged" "{{dir}}" -- \
        -merge=1 -rss_limit_mb=2048 > "$log" 2>&1 || { cat "$log"; exit 1; }
    if grep -qE "panicked at|deadly signal|ERROR: libFuzzer" "$log"; then
        cat "$log"
        echo "{{target}} crashed on an input while merging; the log above names it"
        exit 1
    fi
    rm -rf "{{dir}}"
    mkdir -p "{{dir}}"
    # What the merge added: new names, not the bases' own.
    ls "$merged" | sort | comm -13 "$before" - | while read -r name; do
        input="$merged/$name"
        if [[ $name =~ ^[0-9a-f]{40}$ ]] && { [ "{{cap}}" -eq 0 ] || [ "$(wc -c < "$input")" -le "{{cap}}" ]; }; then
            mv "$input" "{{dir}}/"
        fi
    done

# Give fuzz/Cargo.lock the workspace's versions of what they share, after
# Cargo.lock changes, so a crash the harness finds replays in `just check`
fuzz-lock:
    cp Cargo.lock fuzz/Cargo.lock
    cargo update --manifest-path fuzz/Cargo.toml --workspace

# Format all code
fmt:
    cargo fmt --all
    cargo fmt --manifest-path fuzz/Cargo.toml

# The compiler, e.g. `just morphc build examples/toml/mission`
[positional-arguments]
morphc *args:
    cargo run -q -p morpheus-morphc -- "$@"
