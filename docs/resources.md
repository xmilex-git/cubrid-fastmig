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
**max(build, verify)** temp disk, not the sum. The verifier scans every index at once under the same memory rule,
first on the target (the build's pairs: temp max) and then on the source, whose pairs are not the target's: a
develop source still holds the deferred indexes' catalog rows, an 11.4 source its own catalog rows (T1: develop
+504 B; 11.4 dirty 10,094,800 B against temp max 9,186,674 B). Its need, **verify min** = max(temp max, source
side), is counted only with `--verify` (`plan --verify`, `migrate --verify`): the verifier's own reader over the
source heaps, nothing kept (`fastmig_verify::run::heap_pair_bytes`, one more heap read). With `--verify` it is both
the least and the most the run takes. Before this rule, T1 11.4 dirty at `--mem 2M --verify` stopped on the arena
(limit temp max).

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
- **mem min** = per worker the larger of the copy engine's 256 KiB I/O buffer and what the index build needs,
  449 KiB (`intermediate_merge::WORKER_MIN`: the merge floor, and a scan share holding the 64 KiB spill write buffer
  and a run as large); `migrate` refuses less (`limits::mem_min`). Above it the keys spill instead of failing.
- **verify mem min** (`plan --verify`, `migrate --verify`): the verifier also reserves its live row map before its
  scan (`ESTIMATE_PAGE_BYTES` per source heap page), so it needs that, the merge floors and one scan share
  (`SCAN_MIN`, 128 KiB) per worker (`fastmig_verify::run::memory_min`); `migrate --verify` refuses less before
  anything is written. Big develop DB, 16 workers: 11.4 MiB (mem min 7.0 MiB).
- **mem recommended** = `2 × (Σ T_i + 256 MiB)`, at most the physical memory: room for every pair, counted in
  spill bytes, in finished runs. It spills less, not nothing; the temp figures do not depend on `--mem`.

## Memory the index build and the verifier take

Inside `--mem` (one `MemoryLimit`, grants returned on drop):

- each scan worker's share, `min(256 MiB, mem / (2 × workers))`: its writers' runs (keys, radix words, entries and
  the sort's spare entries, `Run::bytes`) and the 64 KiB write buffer of the one spill it makes at a time;
  the shares are taken together and only as many workers scan as get `SCAN_MIN` each (none: refused). The
  verifier at `--mem 16M` on the big develop DB, its live row map taken first, left its 16 workers shares too small for
  more than a few pairs per run: heap peak 1.1 GiB, 994 s in its run sort (target and source);
- a finished run kept in memory: only with a grant of its size taken when the limit has room, else spilled;
- the merges: a floor of three open runs per worker (`MERGE_FLOOR`, 193 KiB) taken before the scan, and after the
  scan everything the limit has free, split per worker (*merge memory*). One open spill run costs its 64 KiB read
  buffer, 256 B of cursor and its longest key (`open_cost`). An index with more spilled runs than one worker's merge
  memory holds is first reduced by **intermediate merges**: its smallest runs (a heap by size), as many as fit,
  merged into one until the rest fit (`intermediate_merge::bound_open_runs`). Each shard of the final merge holds a worker permit
  and opens at most that many runs, so the merges running at once stay within the merge memory. The verifier does
  the same before its check-2 merges, and gives the merge memory back before its root heap scan.

An intermediate merge frees each range of its inputs as soon as it is in a read buffer and takes its output's
space only as it writes (`SpillCursor::consume`, `SpillOut`): the bytes on disk never exceed the runs before the
merge, so the temp disk bounds above hold. It costs one more read and write of the merged runs; at a memory limit
that keeps few runs open, several rounds.

### Memory outside `--mem`

Outside `--mem`, and why:

- per worker, fixed: the copy engine's I/O buffer, page buffers of the scan and the build (a few pages), thread
  stacks; a key longer than a third of the merge memory still merges two runs at a time;
- per spilled run (more runs at a smaller `--mem`): its descriptor, extents and one sample key per 1024 pairs
  (`spill_run`), its shard bounds;
- proportional to the data, not to `--mem`: the leaf summaries a build keeps for its upper levels (one key per
  leaf), page treatment's per-page counts (`HeapState`, about 100 B a page);
- proportional to the catalog: an 11.4 source's regenerated catalog rows (`catalog_layout::generate`; T1: 14 MB of
  the 21.7 MB peak at `--mem 2M`, valgrind massif);
- the allocator's own: memory freed by one thread kept in its arena (RSS above the live heap; large blocks are
  mapped, glibc `M_MMAP_THRESHOLD` fixed at 1 MiB).

So the excess is no constant: it is largest at a small `--mem` (many runs) and below zero from 256 MiB, where the
limit's own grants are not all touched. Measured max RSS (MiB), big develop and big 11.4 DBs (pair bytes 1.43 GiB
each), `--workers 16`, CPU 0-15, default temp disk, binary c9def7d:

| `--mem` | develop | develop `--verify` | 11.4 | 11.4 `--verify` |
|---|---|---|---|---|
| 16M | 54.6 (+38.6) | 86.1 (+70.1) | 66.9 (+50.9) | 89.2 (+73.2) |
| 32M | 53.7 (+21.7) | 68.5 (+36.5) | 71.2 (+39.2) | 74.5 (+42.5) |
| 64M | 76.9 (+12.9) | 87.6 (+23.6) | 89.0 (+25.0) | 91.6 (+27.6) |
| 128M | 122.7 (−5.3) | 134.8 (+6.8) | 138.1 (+10.1) | 140.4 (+12.4) |
| 256M | 220.1 (−35.9) | 219.3 (−36.7) | 251.9 (−4.1) | 249.5 (−6.5) |
| 1G | 680.4 (−343.6) | 714.5 (−309.5) | 677.6 (−346.4) | 695.7 (−328.3) |

`plan` shows the **expected max RSS** at the chosen `--mem` (`ResourceEstimate::rss_expected_max`): `--mem` plus
12 MiB, plus 18 MiB when the catalog is regenerated, plus 32 MiB per GiB of pair bytes, plus with `--verify` 24 MiB
per GiB of the verifier's source side — an upper envelope of every run measured (the table above, and T1 at
`--mem 2M` and 8M: develop dirty 4 workers `--verify` 12.4 MB against 14.5 MiB, 11.4 dirty 29.3 MiB against
33 MiB, 11.4 clean 16 workers `--mem 8M --verify` 37.0 MiB against 38.5 MiB), fitted, not proven. The default
`--mem` (no option) is a quarter of the physical memory and at least 256 MiB where half the memory allows: from
256 MiB every measured run stayed within `--mem`.

T1 develop dirty, `--workers 4 --mem 2M` (below the old mem min): no `--verify` heap peak 5.3 MiB (3.4 MiB before
the build starts); `--verify` max RSS 11.3 MiB. Before the merge bound, the build opened every spilled run in every
shard with a buffer of at least 4 KiB outside the limit: the big develop DB at `--mem 16M` peaked at 257 MiB of heap.
The verifier read the whole active log to parse its header (64 MiB on T1); it reads 512 B now.

## Choosing the limit (`--temp-disk min|max|<size>`)

- With the option: that limit; below temp min, or with the target's growth (`target_space.needed_bytes`) over
  the free space of the target filesystem (the spill directory's own filesystem when it lies elsewhere), the run
  is refused before anything is written.
- Without it on a terminal: the table and the choices are shown and the answer is read (empty answer: max).
- Without it otherwise: max when it fits the free space, else a refusal naming `--temp-disk min` and its cost.
- A limit at or above temp max runs as one batch; below it the run counts the pairs again on the target (after
  page treatment, exact) and batches by those counts.
- With `--verify`, min and max are both verify min.
- Over a target a killed run left (`FASTMIG_INCOMPLETE`), the free space counts the files that run listed on the
  target filesystem as free: the rerun removes them before it writes (`incomplete_marker::reclaimable_bytes`).
  Without it a rerun on a filesystem the first run filled was refused. A spill arena left in a spill directory
  on another filesystem is not counted.

Measured on T1 11.4 dirty (`--no-clone --mem 64M --verify`, target on a tmpfs capped at plan's need): 16 MiB
short → refused, nothing written; need + 4 MiB → rc 0, migration 0; need + 64 MiB with another process taking all
free space but the unreserved writes once the run took its space → rc 0; kill -9 with the arena present, a second
migrate meanwhile refused by the marker lock, rerun on the same filesystem → rc 0, nothing left over.

## The target side

Right after the incomplete marker, before the first byte of the target, `migrate` also takes the target space:

- every volume the plan byte-copies is created and allocated in full (`volume_copy::preallocate_targets`); the
  copy then writes into it and never clones it;
- the new log `<target>_lgat` is created at its full size and allocated (`new_log::allocate`); its header is
  written last, so until then page 0 has no log magic and the target does not boot (copydb leaves the other pages
  as holes; here they are zero);
- every volume the plan clones is cloned now (`volume_copy::clone_targets`; a filesystem that refuses stops the
  run before anything else is written), and on XFS the pages the run rewrites in it — the allocated pages of every
  file it frees and, on the same-version path, of the root heap and the treated heaps, on the 11.x path of every
  heap (`Plan::unshare_ranges`) — get blocks of their own (`preallocate::unshare`, `FALLOC_FL_UNSHARE_RANGE`): taken
  once, not held twice in the reservation. Another filesystem keeps them in the reservation, and `plan` says so;
- one reservation file, `<dest>/fastmig/reserve-target`, holds an upper bound of everything else the run may
  write, until the run ends: where the target cannot unshare, the pages a cloned volume unshares
  (`Plan::rewritten_pages_bound` — every heap page when the catalog is regenerated, else the treated pages and the
  root heap, and the freed files' pages), the new B-trees (`target_space::btree_pages`,
  below), 64 pages of catalog slack, the referenced LOB and Java files. The file is
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

**The new B-trees' bound** (`target_space::btree_pages`, per index, from the builder's own rules): a leaf record
takes its first object (8 B; the fixed object with a chain), the key (or a 6 B overflow link), 3 B of padding, 8 B
(unique 16 B) per further object, an 8 B chain link, alignment to 4 and a 4 B slot; a leaf takes no record that
would leave less than `leaf_reserve` free, so it holds at least its user area less that reserve less the largest
record, and each shard (one per 4096 entries, at most the workers) ends on one partial leaf. Objects past a record
fill overflow OID pages of `objects_per_page`, each chain ending on a partial page, plus directory pages. Above
the leaves, one separator per child, levels filled with `nonleaf_reserve` up to the root; an overflow key is
written again for every separator that holds it. Plus header pages of the B-tree and overflow key files.

Its inputs: the pairs, key bytes, longest key and overflow key pages from `pair_count` (exact: estimate against
the migrated target, 0 differing lines), and the keys and overflow OID chains of the source's own tree
(`verify::source_key_groups`: its leaf records and the records holding more objects than a rebuilt record takes,
deleted objects included — every live row's key is in the source's index, so these bound the rebuild's). The
system catalog indexes of an 11.4 source, whose rows the run generates, take the worst case: every pair its own
key, every 86th pair opening a chain. The former `5 · T_i / page + 4` was no bound: a key of 86 rows takes a
16 KiB overflow OID page for its 86th object, against 1,032 B of spill records.

| DB (`--workers 16`) | pages the run allocated | B-tree bound, pages | bound / actual |
|---|---|---|---|
| T1 develop clean | 648 | 1,381 | 2.13 |
| T1 develop dirty | 605 | 1,302 | 2.15 |
| T1 11.4 clean | 769 | 1,509 | 1.96 |
| T1 11.4 dirty | 721 | 1,430 | 1.98 |

The allocated pages also hold catalog pages and appended heap pages; on T1 most of the gap is the fixed pages of
75 small indexes (file header, root, one partial leaf).

Freeing a file writes too: `destroy_file` retypes every allocated page of every B-tree and overflow key file (and,
with a regenerated catalog, of the catalog and the freed system heaps), and on a cloned volume each such write
unshares a block. The rewritten-pages bound counts them (`n_page_user + n_page_ftab` of each file the run frees);
5cbe96d had left them out. What the run really writes outside the space it took at the start, measured as the
blocks of the cloned volumes no longer shared with the source (`filefrag -v`) plus added volumes, against the
reservation (`--workers 16 --mem 64M`):

| DB | written outside, B | reservation, B | reservation / written |
|---|---|---|---|
| T1 develop clean | 23,478,272 | 41,964,660 | 1.78 |
| T1 develop dirty | 30,310,400 | 57,087,092 | 1.88 |
| T1 11.4 clean | 37,617,664 | 61,396,084 | 1.63 |
| T1 11.4 dirty | 40,468,480 | 60,183,668 | 1.48 |
| T4 11.3 dirty | 40,796,160 | 59,802,052 | 1.46 |

That table is before the unshare at the start (2ac7dc5). With it, on XFS, the blocks the run unshares beyond the
ranges it took at the start, against the reservation (`--workers 16 --mem 64M`):

| T1 DB | unshared at the start, B | written beyond it, B | reservation, B |
|---|---|---|---|
| develop dirty | 35,586,048 | 4,800,512 | 22,451,316 |
| develop clean | 18,235,392 | 5,259,264 | 23,745,652 |
| 11.4 dirty | 35,635,200 | 6,635,520 | 24,581,236 |
| 11.4 clean | 35,553,280 | 7,061,504 | 25,875,572 |

The ranges hold whole files (a treated heap's pages that need no patch too), so more is unshared than written
before (develop dirty 40.4 MB against 30.3 MB), but taken once: the free space needed went from 2 × 57.1 MB to
35.6 + 2 × 22.5 MB. Cloning and unsharing took 0.08 to 0.16 s on T1 (report.json `volumes.clone_unshare`).

Heap pages the page treatment appends are bounded too (`page_treatment::appended_pages_bound`): only a re-encoded
record can grow, so every record of a re-encoded class is re-encoded as the treatment will (the same encoder, LOB
directories included) and one that grows is counted whole on new pages. report.json: `catalog.appended_pages`
against `temp_disk.target_appended_bound_pages`; T1 11.4 clean and dirty 0 against 2, T1 develop, T4 11.3 dirty
and 11.2 clean 0 against 0. A source index with a defect (a live row's key missing)
can make the source's counts too small: not covered yet (1번 세션 decision: a check at build time before the
first page is written).

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

- The verifier does not batch: with `--verify` the limit must be verify min (T1 develop dirty, `--mem 64M`,
  `--temp-disk min --verify` before this rule: build peak 0.85 MB under a 1.48 MB limit, the verifier 7.5 MB).
