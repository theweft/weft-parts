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

# Checks the KiCad importer against a real export: the official CM5 IO Board (Raspberry Pi
# design files), exported with kicad-cli. Not part of `ci`: it downloads about 1 GB of KiCad.
kicad-real:
    #!/bin/sh
    set -eu
    dir=$(mktemp -d)
    trap 'rm -rf "$dir"' EXIT
    curl -sSfL -o "$dir/cm5io.zip" "https://pip-assets.raspberrypi.com/categories/1098-design-files/documents/RP-008099-DD-1-CM5%20IO%20Board,%20revision%202,%20KiCAD%20files..zip"
    mkdir "$dir/src" && (cd "$dir/src" && unzip -q ../cm5io.zip)
    HOME="$dir" nix shell nixpkgs#kicad-small -c kicad-cli sch export netlist --format kicadsexpr -o "$dir/cm5io.net" "$dir/src/CM5IO.kicad_sch" >/dev/null
    WEFT_KICAD_NETLIST="$dir/cm5io.net" cargo test --locked --test kicad -- --nocapture

# Validates requirements/*.sdoc and renders requirements, traceability and ReqIF to target/requirements.
# The coverage gate itself runs as a test (tests/requirements.rs).
requirements:
    strictdoc export . --formats=html,reqif-sdoc --output-dir target/requirements

# Everything CI checks.
ci: fmt-check clippy test deny licenses typos doc requirements
