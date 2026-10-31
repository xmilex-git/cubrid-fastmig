# The verifier (`cubrid-fastmig verify`)

Checks a database's on-disk consistency in O(n log n), and with `--source` sorts each target defect into a
migration defect, a pre-existing defect or a resolved defect ([GLOSSARY](../GLOSSARY.md)). Design: decision G3
[workspace#483](https://github.com/xmilex-git/workspace/issues/483) (+ G6 #488 for check 5, G5 #486 for deferred
indexes, G2 #482 for header statistics); implementation ticket P1f
[workspace#495](https://github.com/xmilex-git/workspace/issues/495); [ADR 0007](adr/0007-rust-verifier-instead-of-checkdb.md).

```sh
cubrid-fastmig verify <target-db> --source <source-db> [--workers N] [--mem 16G] [--spill-dir DIR] [--temp-disk SIZE]
cubrid-fastmig verify /path/to/first-volume --role source      # one database, source rules (vacuum residue allowed)
```

Exit code: 0 no migration defect, 1 migration defects, 2 usage or tool error. Report: `--report` (default
`<target volume dir>/fastmig/verify.jsonl`), one `{"kind":"defect",...}` line per defect, then one
`{"kind":"summary",...}` line (also printed). `migrate` runs the same check at its end only with `--verify`; `finalize` reads this report
(ADR 0015), so a target migrated without `--verify` needs `verify <target> --source <source> --measured-classes
<dest>/fastmig/measured-classes` before finalize. A defect's identity is (check, class, index, object, code); source and
target defects with the same identity are the same defect, because placement preservation keeps OIDs.

## Checks and defect codes

| Check | Module | Codes |
|---|---|---|
| 1 heap | `heap_pass`, `heap_chain` | `page-header` `slotted-page-header` `no-slot-0` `header-class-oid` `header-stats-decode` `chain-record` `chain-class-oid` `page-vacuum-residue`ᵀ `slot-out-of-area` `slot-overlap` `total-free` `cont-free` `record-home` `record-header` `record-mvcc-residue`ᵀ `record-layout` `record-repid` `record-decode` `chain-foreign-page` `chain-cycle` `chain-prev-link` `chain-unvisited-page` `header-last-vpid` `bestspace-entry` `header-stats`ᴹ `newhome-shared` `newhome-orphan` `relocation-dangling` `row-in-two-heaps` |
| 2 index | `btree_walk`, `index_merge`, `foreign_key` | `root-unreadable` `root-header` `key-domain` `root-mvcc-fields`ᵀ `node-*` `max-key-len` `nonleaf-record` `overflow-key` `leaf-*` `prefix-restore` `separator` `keys-not-increasing` `oids-not-ascending`ᵀ `duplicate-oid` `unique-violation` `class-oid-flag`ᵀ `ovf-oid-*` `ovf-dir-head` `ovf-directory` `object-mvcc-form`ᵀ `unique-stats` `unique-num-nulls` `nonunique-stats` `ovfid` `missing-in-index` `missing-in-heap` `key-value-differs` `null-in-primary-key` `key-extract` `fk-orphan-key` `fk-key-decode` |
| 3 catalog | `catalog_refs`, `object_refs` | `rep-dir` `heap-file` `btree-file` `orphan-heap` `orphan-btree` `owner` `trigger` `fk-ref` `partition-link` `constraints` `db-class-row` `object-dangling` `object-wrong-class` |
| 4 sector | `sector_owner` | `file-unreadable` `file-header` `sector-shared` `sector-leaked` `sector-unmarked` `unallocated-page-header` `page-unreadable` |
| 5 LOB | `lob_files` | `lob-missing` `lob-size` `lob-unsupported` `lob-no-base` |

ᵀ target rules only (`--role target`, the default for `<target-db>`). ᴹ only for classes in `--measured-classes`.

`page-vacuum-residue` follows the patch-target page selection (workspace#499, #501, #484): a data page is a defect
when its `max_mvccid` is not below the target log's `mvcc_next_id`, or — with `--source` — when the source's
vacuum horizon still makes it a patch target (ONCE or UNKNOWN with `max_mvccid` ≥ the source's oldest unvacuumed
MVCCID while vacuum work was pending), or — without `--source` — when its vacuum status is ONCE. A ONCE page below
the horizon is vacuumed: the engine leaves ONCE on a page whose vacuum job had been interrupted and re-ran.
Pages the migration did not need to patch keep their chain record and pass.

## Work and memory

Phases (`phases_s` in the summary, per database): `sectors` → `heap pass` (every heap once, all classes in one
queue on `--workers` threads; tree tops opened first) → `run sort` (every in-memory key run, largest first, on the
workers) → `index merge and foreign keys` (one pool on the workers, most leaf pages first: an index of at least
2 × 1024 leaves is merged in up to `--workers` leaf-range shards, cut at parent separators; each foreign key is
one job) → `root heap` → `live rows` → `deferred indexes` → `catalog`. With `--source` the target is verified
first, then the source. workspace#509 measured the phases on the big develop DB (22.6 M rows, 75 indexes).

Under `--mem`: the heap pass's key runs (spilled under `--spill-dir` when they do not fit) and the **live row map**
(the live rows of every heap, for OBJECT values, catalog references and deferred indexes). The map holds 24 B per
heap page with a live row plus one bit per slot up to the page's last live slot; it is reserved before the heap
pass at 64 B per heap page (5 words of slot bits) and settled to its real size after. A `--mem` that cannot hold it
stops `verify` with exit code 2 and the `--mem` that can. Measured: 6.1 MB for 104,923 pages and 22.6 M rows on the
big develop DB (58 B per page; 16 B per row before workspace#509 was 361 MB); `--mem 2M` there stops with "rerun
with --mem 7M or more". Scale: a 1 TiB database has 64 Mi pages of 16 KiB, so its map is 4 GiB reserved
(64 B × 64 Mi) and about 3.6 GiB at the measured 58 B per page — when every page is a heap page. Outside `--mem`:
per-worker page buffers and the 64 KiB read buffer of each spill file a merge has open.

## What is not compared

- Deferred indexes (function and filter): structure and the OIDs of their entries only; listed under
  `not_checked` in the summary. 11.4 DEDUPLICATE indexes (hidden key column) the same way.
- The builder's fill rules and the engine's separator bytes (G3 Q3).
- Bestspace hint counts and B-tree statistics in the catalog (hints, G2).
- A synonym's target (stored by name).
- Values themselves: the verifier decodes with the same codec as the migration; the independent oracle is the
  set comparison of `unloaddb` outputs.
