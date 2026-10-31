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

# Build one baseline DB variant (basedb/README.md); needs BASEDB_INSTALL and a claimed BASEDB_PORT.
basedb VERSION VARIANT:
    bash basedb/build.sh {{VERSION}} "${BASEDB_INSTALL:?BASEDB_INSTALL required}" "${BASEDB_PORT:?BASEDB_PORT required}" {{VARIANT}}

_require-toolchain:
    @test -x .toolchain/bin/cargo || { echo "error: .toolchain is missing; run 'just toolchain' first" >&2; exit 1; }

# Run ported testcases (tc/README.md) against the release build; needs FASTMIG_TC_CTP and FASTMIG_TC_SERVER_CTL.
tc TC_ROOT TC_LIST INSTALL OUT: release
    bash tc/run.sh {{TC_ROOT}} {{TC_LIST}} {{INSTALL}} target/release/cubrid-fastmig "${FASTMIG_TC_CTP:?FASTMIG_TC_CTP required}" "${FASTMIG_TC_SERVER_CTL:?FASTMIG_TC_SERVER_CTL required}" {{OUT}}

# One end-to-end run of a baseline DB (e2e/README.md); needs FASTMIG_E2E_INSTALL, FASTMIG_E2E_PORT, FASTMIG_E2E_SERVER_CTL.
e2e PRISTINE TARGET_VERSION OUT: release
    bash e2e/run.sh target/release/cubrid-fastmig {{PRISTINE}} {{TARGET_VERSION}} {{OUT}}

# Operations scenario on both sides with parity (e2e/ops/README.md); needs FASTMIG_OPS_INSTALL, FASTMIG_OPS_PORT, FASTMIG_OPS_SERVER_CTL.
ops PRISTINE TARGET_VERSION OUT *ROUNDS: release
    bash e2e/ops/run.sh target/release/cubrid-fastmig {{PRISTINE}} {{TARGET_VERSION}} {{OUT}} {{ROUNDS}}

# One lane of the T37 version matrix (e2e/ops-matrix/README.md); needs FASTMIG_OPS_INSTALL, FASTMIG_OPS_PORT, FASTMIG_OPS_SERVER_CTL.
ops-matrix LANE OUT_ROOT *IDS: release
    bash e2e/ops-matrix/run_lane.sh target/release/cubrid-fastmig {{LANE}} {{OUT_ROOT}} {{IDS}}

# The pass table of the version matrix from its runs, on stdout (docs/ops-verification.md).
ops-matrix-summary OUT_ROOT:
    bash e2e/ops-matrix/summarize.sh {{OUT_ROOT}}
