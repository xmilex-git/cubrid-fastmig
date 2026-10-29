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

`migrate` registers the target when every write succeeded; with `--verify` it first runs the verifier and only a
target with no migration defect is registered and loses its `FASTMIG_INCOMPLETE` marker (ADR 0015).
`finalize <target-dir>` is the only command that deletes the source, and it refuses unless a verification passed
(`migrate --verify` or a later `verify` run) and the source is unchanged since the migration (ADR 0008).
The verifier runs on both source and target, so defects already present in the source are
not reported as migration defects.

## Disk and memory are chosen before the start

`plan` shows the temp disk the index rebuild needs at least (one index at a time) and at most
(every index at once), and what the batches in between cost in time; `migrate --temp-disk min|max|<size>` picks
one before anything is written and keeps to it by building the indexes in batches. How the bounds are computed:
[docs/resources.md](docs/resources.md) (workspace#526).

## Layout

| Crate | Role |
|---|---|
| `crates/ondisk` | On-disk structures: volume and page headers, heap records, file headers, log header |
| `crates/plan` | The migration plan: version pair, copy method per volume, page treatment grade per class, deferred indexes, LOB manifest, pre-check |
| `crates/progress` | Phases of a run, their per-worker counters, ETA, and the progress display (screen or lines) |
| `crates/copy` | Parallel copy, patch and transform passes |
| `crates/catalog` | Catalog regeneration in the target format |
| `crates/index` | Parallel index rebuild |
| `crates/verify` | Heap, index and catalog verifier |
| `crates/cli` | The `cubrid-fastmig` command |
| `tc/` | Ported testcases: CUBRID shell testcases run through the switch point (`just tc`, [tc/README.md](tc/README.md)) |

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
