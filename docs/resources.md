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

An 11.4 source is counted as the target will hold it, from the catalog regeneration's first pass run alone
(`catalog_layout::generated_rows`, nothing written; placeholder OIDs have the length of real ones): each user class
through the class object the run writes and, for a re-encoded class, each row through its re-encoder first; each
system catalog index from the rows the run generates, encoded as the run encodes them (`encode_row`) and read by
the same key extractor (`pair_count::record_pair_bytes`). A develop source is counted in its own heaps without the
deferred indexes' `_db_index` and `_db_index_key` rows, which the run deletes before the build
(`same_version_catalog::deferred_rows`).

Measured (`plan --json` on the source against `plan --json` on the migrated target, per index): T1 11.4 and develop,
clean and dirty, and the big 11.4 and develop DBs: all 75 indexes (11.4: 44 user, 31 system) equal byte for byte
(big: 1,530,676,624 B 11.4, 1,530,676,648 B develop).

A pair is spilled at most once (a run is written once; the build after the scan spills nothing more) and an
arena extent takes exactly the run's bytes, so one scan over a set of indexes spills at most `T = Σ T_i` over the
set, whatever the memory limit. Memory gives no credit: each worker's share (`min(256 MiB, mem / (2 ×
workers))`) is split among the indexes of its class, and a writer spills when its part is full, in in-memory
bytes (keys, radix words, entries, Vec growth) that exceed its spill bytes, however much room the limit has
left. The first formula, `T − (mem/2 − share)`, was not a bound: T1 develop dirty at `--mem` 530 MiB spilled
832 KiB where it gave 0, and the run stopped on the arena.

- **temp min** = the largest `T_i`: the largest index alone. Any limit at or above it lets the run finish.
- **temp max** = `Σ T_i`: every index in one scan, what a run spills at most.
- **index batches**: a limit between the two cuts the indexes largest first, first fit, into batches whose pair
  bytes fit it (`temp_disk::index_batches`). Each batch is its own heap scan, so a batch costs one more scan: the
  table's *extra s* is the counting scan's time × (batches − 1).
- **mem min** = one 256 KiB copy buffer per worker (the copy engine refuses less); below the other needs the
  build spills instead of failing.
- **mem recommended** = `2 × (Σ T_i + 256 MiB)`, at most the physical memory: room for every pair, counted in
  spill bytes, in finished runs. It spills less, not nothing; the temp figures do not depend on `--mem`.

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
- the new log `<target>_lgat` is created at its full size and allocated (`new_log::allocate`); its header is
  written last, so until then page 0 has no log magic and the target does not boot (copydb leaves the other pages
  as holes; here they are zero);
- one reservation file, `<dest>/fastmig/reserve-target`, holds an upper bound of everything else the run may
  write, until the run ends: the pages a cloned volume unshares (`Plan::rewritten_pages_bound` — every heap page
  when the catalog is regenerated, else the treated pages and the root heap), the new B-trees (a leaf record takes
  at most twice a pair's spill record, leaves at most 90 % full, non-leaf pages fewer than leaves:
  `5 · T_i / page + 4` pages per index), 64 pages of catalog slack, the referenced LOB and Java files. The file is
  removed only when the run ends, so while the run writes what it bounds the space is held twice (accepted,
  1번 세션 decision).

The free-space check (with the temp limit when the spill directory is on the target filesystem) therefore needs
the byte-copied volumes, the new log and twice the reservation (`TargetSpace::needed`); `plan` prints it, refuses
when the target filesystem has less, and writes it to `plan.json` as `target_space`.

**Unreserved writes.** What the reservation bounds is written outside the space the run took: a process that fills
the filesystem while the run writes can still take that space, and the run then stops on a full disk. `plan`
prints the bound as `unreserved writes ≤ N MiB` (the reservation's size) so the run can be started with that much
left to other writers. Only the byte-copied volumes, the new log and the spill arena are written inside space the
run already holds.

## The spill arena

With a temp disk limit, `migrate` allocates one file of that size (`<spill-dir>/arena`, `posix_fallocate`) right
after the incomplete marker, before the first byte of the target, and every spill run of the run (the build and
the verifier) is written into it as extents (`fastmig_index::spill_arena`): first fit, several extents when no
single free range holds a run, merged back on free. The space is never given back to the filesystem until the run
ends, so another process filling the disk cannot stop the run halfway. The arena is a scratch line of the marker:
a rerun after a kill removes it, and the end-of-run fsync skips it.

The progress line shows `temp <now>/max <peak> of --temp-disk <limit>` next to the heap reading (Relaxed
counters, `TEMP_DISK`).

`report.json` records `temp_disk.limit_bytes`, whether it batched, and `peak_bytes`, the most spill bytes on disk
at any moment of the run (the build and, with `--verify`, the verifier).

## Not covered yet

- The verifier does not batch: with `--verify` the limit must be temp max (T1 develop dirty, `--mem 64M`,
  `--temp-disk min --verify` before this rule: build peak 0.85 MB under a 1.48 MB limit, the verifier 7.5 MB).
