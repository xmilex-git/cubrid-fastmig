# Resources a run needs: temp disk and memory (workspace#526)

`plan` prints a resource table and `migrate` decides its temp disk limit from it before anything is written. The
user asked for this so that a run never stops halfway on a full disk: the sizes are shown before the start, the
user picks one, and the run keeps to it by scheduling, not by failing.

## What spills

The index rebuild reads every indexed heap once (`fastmig_index::fill`) and turns each live row into a (key, OID)
pair per index. Pairs that do not fit the memory limit (`--mem`) are sorted and written to spill files under
`--spill-dir` (default `<dest>/fastmig/spill`) as records `key_len u32 · OID 8 B · key`. Every index's pairs exist
at once by the end of the scan; the builds that follow read them back and write nothing more to disk.

The verifier (`--verify`) spills in its own pass after the build's spill files are gone, so a run needs
**max(build, verify)** temp disk, not the sum. The verifier scans every index at once under the same memory rule
(its heap side is the same pairs), so its bound is temp max; with `--verify` the smallest workable limit is temp
max.

## The estimate

`pair_count` reads the heaps with the build's own key extractor and adds, per index, `12 + key length` for every
pair the build would emit (deleted rows and all-NULL keys are left out the same way). This is the exact size of
the index's pairs as spill records, `T_i`. The read takes about one heap scan; the table shows its time.

The scan keeps finished runs in memory while the limit has room: each worker holds a share of at most
`share = min(256 MiB, mem/2)` and finished runs get the other half, so at least `mem/2 − share` bytes stay in
memory before anything spills. One scan over a set of indexes therefore spills at most

    spill_bound(T, mem) = max(0, T − (mem/2 − share)),  T = Σ T_i over the set

This is an upper bound: the table never shows less than a run can write.

- **temp min** = max over indexes of `spill_bound(T_i, mem)`: the largest index alone. Any limit at or above it
  lets the run finish.
- **temp max** = `spill_bound(Σ T_i, mem)`: every index in one scan, what a run without a limit writes at most.
- **index batches**: a limit between the two cuts the indexes largest first, first fit, into batches whose bound
  fits it (`temp_disk::index_batches`). Each batch is its own heap scan, so a batch costs one more scan: the
  table's *extra s* is the counting scan's time × (batches − 1).
- **mem min** = one 256 KiB copy buffer per worker (the copy engine refuses less); below the other needs the
  build spills instead of failing.
- **mem recommended** = `2 × (Σ T_i + 256 MiB)`, at most the physical memory: the limit at which nothing spills at
  full parallelism.

## Choosing the limit (`--temp-disk min|max|<size>`)

- With the option: that limit; below temp min, or with the target's growth (`target_space.needed_bytes`) over
  the free space of the target filesystem (the spill directory's own filesystem when it lies elsewhere), the run
  is refused before anything is written.
- Without it on a terminal: the table and the choices are shown and the answer is read (empty answer: max).
- Without it otherwise: max when it fits the free space, else a refusal naming `--temp-disk min` and its cost.
- A limit at or above temp max runs as one batch; below it the run counts the pairs again on the target (after
  page treatment, exact) and batches by those counts.

## The target side

Right after the incomplete marker, before the first byte of the target, `migrate` also takes the target space:

- every volume the plan byte-copies is created and allocated in full (`volume_copy::preallocate_targets`); the
  copy then writes into it and never clones it;
- one reservation file, `<dest>/fastmig/reserve-target`, holds an upper bound of everything else the run may
  write, until the run ends: the pages a cloned volume unshares (`Plan::rewritten_pages_bound` — every heap page
  when the catalog is regenerated, else the treated pages and the root heap), the new B-trees (a leaf record takes
  at most twice a pair's spill record, leaves at most 90 % full, non-leaf pages fewer than leaves:
  `5 · T_i / page + 4` pages per index), 64 pages of catalog slack, the referenced LOB and Java files and the new
  log. Counting it twice while the run writes is accepted.

The free-space check (with the temp limit when the spill directory is on the target filesystem) uses this total;
`plan` prints it.

## The spill arena

With a temp disk limit, `migrate` allocates one file of that size (`<spill-dir>/arena`, `posix_fallocate`) right
after the incomplete marker, before the first byte of the target, and every spill run of the run (the build and
the verifier) is written into it as extents (`fastmig_index::spill_arena`): first fit, several extents when no
single free range holds a run, merged back on free. The space is never given back to the filesystem until the run
ends, so another process filling the disk cannot stop the run halfway. The arena is a scratch line of the marker:
a rerun after a kill removes it, and the end-of-run fsync skips it.

`report.json` records `temp_disk.limit_bytes`, whether it batched, and `peak_bytes`, the most spill bytes on disk
at any moment of the run (the build and, with `--verify`, the verifier).

## Not covered yet

- 11.4 sources have no estimate (their keys come from re-encoded rows): `plan` says so, `--temp-disk min|max` is
  refused, `--temp-disk <size>` is taken and checked against the exact count on the target before the build.
- The verifier does not batch: with `--verify` the limit must be temp max (T1 develop dirty, `--mem 64M`,
  `--temp-disk min --verify` before this rule: build peak 0.85 MB under a 1.48 MB limit, the verifier 7.5 MB).
- The B-tree bound for 11.4 sources is three times the source's B-tree pages until their pair counts exist
  (`target_space` shortcut).
