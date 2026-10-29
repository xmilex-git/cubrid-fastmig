# cubrid-fastmig

Offline migration of a CUBRID database to a newer on-disk format without unload/load.
Written in Rust; it does not link or run any CUBRID engine binary.

## Purpose

Move a cleanly shut down CUBRID 10.2–11.4 database directly to 11.4 or develop (11.5):

- user heap and overflow pages keep their placement (volid/pageid/slot), so class OIDs and
  OBJECT references survive unchanged;
- each volume is cloned (reflink), copied in parallel, patched in place, or transformed,
  whichever the plan picks for that volume;
- the system catalog is regenerated in the target format;
- every index is rebuilt in parallel.

The first target is 11.4 → develop.

## Not in scope

- Downgrades and multi-hop migrations through intermediate versions.
- Online or incremental migration; the source must be shut down cleanly.
- The log and backup chains of the source; the target starts with a new log.
- HA environments.
- Changes to the CUBRID engine.

## The source database is deleted only after verification

Do not delete the source database until the verifier has passed on the migrated database.
The verifier runs on both source and target, so defects already present in the source are
not reported as migration defects.

## Layout

| Crate | Role |
|---|---|
| `crates/ondisk` | On-disk structures: volume and page headers, heap records, file headers, log header |
| `crates/plan` | Per-volume choice of clone, copy, patch or transform |
| `crates/copy` | Parallel copy, patch and transform passes |
| `crates/catalog` | Catalog regeneration in the target format |
| `crates/index` | Parallel index rebuild |
| `crates/verify` | Heap, index and catalog verifier |
| `crates/cli` | The `cubrid-fastmig` command |

## Build

The repository is driven by [just](https://just.systems). The Rust toolchain is pinned through
nix (`toolchain.nix`); every recipe runs cargo from `.toolchain/bin`, not the host's.

```sh
just toolchain          # build or refresh the pinned toolchain (.toolchain GC root)
just build              # debug build; `just release` for an optimized one
just check              # rustfmt --check and clippy -D warnings
just fmt                # format in place
just run --help         # run cubrid-fastmig with its arguments
```

`just` alone lists the recipes.

## License

Apache-2.0. See `LICENSE`.
