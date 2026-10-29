# Every recipe runs cargo from the pinned toolchain in .toolchain (see toolchain.nix), never the host's.
export PATH := justfile_directory() / ".toolchain/bin" + ":" + env("PATH")

# List the recipes.
default:
    @just --list

# Build or refresh the pinned toolchain into the .toolchain GC root.
toolchain:
    nix-build toolchain.nix -o .toolchain

# Debug build of the workspace.
build: _require-toolchain
    cargo build --workspace

# Release build of the workspace.
release: _require-toolchain
    cargo build --workspace --release

# rustfmt check plus clippy with warnings as errors.
check: _require-toolchain
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings

# Format the workspace in place.
fmt: _require-toolchain
    cargo fmt --all

# Run the cubrid-fastmig binary (debug build) with ARGS.
[positional-arguments]
run *ARGS: _require-toolchain
    cargo run -q -p cubrid-fastmig -- "$@"

_require-toolchain:
    @test -x .toolchain/bin/cargo || { echo "error: .toolchain is missing; run 'just toolchain' first" >&2; exit 1; }
