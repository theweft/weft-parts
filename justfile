# Run `just` to list recipes. CI runs `just ci` inside `nix develop`.

default:
    @just --list

# Format all code and TOML.
fmt:
    cargo fmt --all
    taplo fmt

# Check formatting without changing files.
fmt-check:
    cargo fmt --all --check
    taplo fmt --check

# Lints; warnings are errors.
clippy:
    cargo clippy --locked --all-targets -- -D warnings
    cargo clippy --locked --lib --no-default-features -- -D warnings

# Data validation and model tests (with independent drivers).
test:
    cargo test --locked


# Licences, advisories, banned crates, allowed sources.
deny:
    cargo deny check

# Spelling.
typos:
    typos

# Licence and copyright: REUSE compliance, plus an SPDX header in every Rust file.
licenses:
    reuse lint
    @missing=$(git ls-files '*.rs' | while read -r f; do head -2 "$f" | grep -q 'SPDX-''License-Identifier: MIT' || echo "$f"; done); \
    if [ -n "$missing" ]; then echo "missing SPDX header:"; echo "$missing"; exit 1; fi

# API docs; broken links are errors.
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps

# Everything CI checks.
ci: fmt-check clippy test deny licenses typos doc
