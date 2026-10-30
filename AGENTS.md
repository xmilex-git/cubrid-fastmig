| `aes`, `ctr` | `ondisk::tde_keys` | AES-256-CTR of the data keys in the TDE keyinfo record (engine `EVP_aes_256_ctr`); reproduces the engine key format, hand-written crypto is a risk (workspace#515) |
| `sha2` | `ondisk::tde_keys` | SHA-256 of the master key in the keyinfo record (engine `tde_make_mk_hash`, workspace#515) |
# AGENTS.md — cubrid-fastmig

Offline CUBRID database migration in Rust. Purpose, scope and the crate roles are in
[README.md](README.md); this file holds the rules for writing code here. The goal of every rule:
an agent opening any one file can tell what it does, why, and where the engine fact behind it
comes from, without reading the rest of the tree.

## Build and check

- Build, run and check only through the `just` recipes (`just` lists them). They run cargo from
  the pinned toolchain in `.toolchain`; `just toolchain` creates it.
- `just check` (rustfmt + clippy `-D warnings`) is green before every commit.
- Correctness is shown at runtime: the `verify` crate and migrations of real source databases.
  Write no unit tests — no `#[test]`, no `tests/` directory, no `cargo test` recipe.
- Build output stays in `target/`; scratch goes under the workspace repo's `.git_ignored_dir/scratch/`.
  `/tmp` is tmpfs on this host and is never used.

## Where decisions and engine facts live

- **Decisions**: the map [xmilex-git/workspace#472](https://github.com/xmilex-git/workspace/issues/472)
  indexes them; each decision lives in one ticket, linked from the map's *Decisions so far*.
- Vocabulary is in [GLOSSARY.md](GLOSSARY.md) and hard-to-reverse decisions in [docs/adr/](docs/adr/); a ticket that settles a term or such a decision adds it there in the same change.
- **Engine format research** (R1–R6), one document per branch of the workspace repo:

  | Doc | Topic |
  |---|---|
  | [R1](https://github.com/xmilex-git/workspace/blob/research/fastmig-r1-page-format/docs/research/fastmig/r1-page-format.md) | volume and page headers |
  | [R2](https://github.com/xmilex-git/workspace/blob/research/fastmig-r2-heap-record/docs/research/fastmig/r2-heap-record.md) | heap records and value encoding |
  | [R3](https://github.com/xmilex-git/workspace/blob/research/fastmig-r3-class-repr/docs/research/fastmig/r3-class-repr.md) | class objects and representations |
  | [R4](https://github.com/xmilex-git/workspace/blob/research/fastmig-r4-catalog-tables/docs/research/fastmig/r4-catalog-tables.md) | catalog tables |
  | [R5](https://github.com/xmilex-git/workspace/blob/research/fastmig-r5-min-files/docs/research/fastmig/r5-min-files.md) | minimum files of a new database |
  | [R6](https://github.com/xmilex-git/workspace/blob/research/fastmig-r6-index-build/docs/research/fastmig/r6-index-build.md) | index build port |
  | [R7](https://github.com/xmilex-git/workspace/blob/research/fastmig-r7-xasl-streams/docs/research/fastmig/r7-xasl-streams.md) | serialized XASL streams and stored opcodes |
  | [R8](https://github.com/xmilex-git/workspace/blob/research/fastmig-r8-older-pairs/docs/research/fastmig/r8-older-pairs.md) | 10.2..11.3 sources: on-disk differences from 11.4 |
  | [R11](https://github.com/xmilex-git/workspace/blob/research/fastmig-r11-old-system-class-text/docs/research/fastmig/r11-old-system-class-text.md) | system class names in user text before develop: the system name mapping table |
  | [R14](https://github.com/xmilex-git/workspace/blob/research/fastmig-r14-trailing-space-compare/docs/research/fastmig/r14-trailing-space-compare.md) | trailing-space compare and hash rules before 11.0: unique rebuild, HASH/LIST/RANGE partitions, foreign keys, `ignore_trailing_space` |
  | [R15](https://github.com/xmilex-git/workspace/blob/research/fastmig-r15-target-114/docs/research/fastmig/r15-target-114.md) | an 11.4 target: what differs from develop, the 11.4 B-tree layout (overflow OID header, root @70, packed key domain, key images) |
  | [R16](https://github.com/xmilex-git/workspace/blob/research/fastmig-r16-deduplicate-index/docs/research/fastmig/r16-deduplicate-index.md) | 11.3/11.4 DEDUPLICATE indexes: detection and the 11.4 rebuild rule of the hidden key |

  A later research doc gets a row here in the commit that first cites it.
- **Engine source**: `~/dev/cubrid-worktree/develop` on the dev host, release lines at
  `origin/release/<ver>`. Look symbols up with the workspace's
  `tools/code-index/code-index <checkout> definition|body|callers <name>`.

## Crate map

Roles: [README § Layout](README.md#layout). Dependencies point toward `ondisk` (and the leaf `progress`) and never form a cycle:

```
ondisk   ← every other crate but progress; the only crate that knows byte layouts
progress   depends on nothing; run phases, their counters and the progress display
plan     → ondisk, catalog
copy     → ondisk, plan, progress
catalog  → ondisk, progress
index    → ondisk, catalog, progress
verify   → ondisk, catalog, index, progress   (index: the heap-scan driver, sorted runs with spill and k-way merge)
cli      → all of the above; the only binary
```

These are the allowed edges, not yet all wired. An edge outside this list, or a new crate,
updates this map (and README § Layout for a crate) in the same commit.

External crates, each with the reason it is not a few lines of our own (add a row with a new one):

| Crate | Used by | Why |
|---|---|---|
| `lz4_flex` | `ondisk::value` | LZ4 block format of compressed strings (engine `cubcompress::LZ4`) |
| `md-5` | `catalog::serial_name` | develop names long auto_increment serials with an MD5 of the 11.4 name |
| `zerocopy` | `ondisk` (headers, fixed records) | safe typed views of fixed on-disk structs (`#[repr(C)]`, little-endian fields) without `unsafe` or offset constants (workspace#525) |
| `aes`, `ctr` | `ondisk::tde_keys` | AES-256-CTR that encrypts the data keys of the TDE keyinfo record (engine `EVP_aes_256_ctr`): the engine key format has to be reproduced exactly, and hand-written crypto is a risk (workspace#515) |
| `sha2` | `ondisk::tde_keys` | SHA-256 of the master key in the keyinfo record (engine `tde_make_mk_hash`, workspace#515) |

## Files and modules

- **One module, one job.** Name the file after that job (`heap_hdr_stats.rs`, `patch_page.rs`),
  never `utils.rs` or `common.rs`. A file that grows a second concern is split into two.
  `lib.rs` and `mod.rs` only declare modules and re-export.
- **Names are GLOSSARY terms.** Types, functions, fields, variables, files and modules use the English term of
  [GLOSSARY.md](GLOSSARY.md) (`re_encoded`, `MemoryLimit`, `DeferredIndex`), never a word on its _Avoid_ line; a
  new concept goes into GLOSSARY in the same commit.
- **Module header.** Every crate root and module opens with a `//!` doc comment in this shape:

  ```rust
  //! Rewrites a heap file header from the 11.4 to the develop layout.
  //!
  //! Input: the source header page (11.4 `HEAP_HDR_STATS`).
  //! Output: the target header page plus the new bestspace page.
  //! Source: engine `heap_file.c:heap_create_internal` (release/11.4, develop); R1 § heap header.
  ```

  Source names engine `file:function` with the release line, an R-doc section, or both.
- **On-disk structs** (`ondisk`). The doc comment names the engine struct, its size and field
  offsets per version, and the engine commit they were read from:

  ```rust
  /// Engine `HEAP_HDR_STATS` (src/storage/heap_file.c).
  /// Size: 296 B in 11.4 (release/11.4 @ <sha>), 1152 B in develop (develop @ <sha>).
  /// Offsets: per field below.
  ```

  Each field doc carries its offset for every version it exists in. A layout that is computed
  rather than read off a real page says so (`offset computed, not yet checked on a page`).
- **Decision comments.** Where code follows a map decision, leave one line of why plus the ticket:
  `// clone whenever the filesystem allows it (workspace#481)`. The reasoning stays in the ticket.

## Rust shape

- **Volume bytes are untrusted input.** Every length, offset or count read from a page is checked
  before it indexes a slice; a decoder returns `DecodeResult` with the offset, never panics. A
  source database with a damaged record must yield a verify finding or a clean error, not an
  aborted process. A fixed-size engine struct (a page, file or log header, a fixed record) is a
  `#[repr(C)]` zerocopy struct of `little_endian` fields named `<Decoded>Le` (or the engine name when
  nothing decodes it), one per version where the layout differs, with a `size_of` assert; read it
  with `ref_from_prefix`, never with literal offsets. Big-endian variable records go through
  `value::OrReader`, single little-endian values at a computed offset through `le_field`; do not add
  a fourth way or slice `b[at..at + 4]` on a computed `at`.
  `panic!`/`unwrap` stay only for invariants of our own code (a match the type already rules out).
- **Do not port engine C idioms.** Take the byte format from the engine, not its code shape:
  no out-parameters, no error codes, no setjmp-style escapes, no manual buffer bookkeeping a slice
  or iterator already gives. Fixed-size reads use `split_first_chunk::<N>()` /
  `first_chunk` instead of `try_into().unwrap()`.
- **Make wrong states unrepresentable.** A value checked once (a version pair, a page kind, a
  class's page treatment grade) becomes an enum or newtype that later code matches on; it is not
  re-derived from bytes or flags downstream.
- **`unsafe` only for a syscall or the counting allocator**, each block with a `// SAFETY:` line
  saying why the call is sound. Threads are `std::thread::scope`; shared state is passed by
  reference, not `Arc`, unless it must outlive the scope.

## Performance

The rule IDs below are from the workspace skill `cpp-perf-rules` (written for C/C++; the ones
cited here carry over to Rust). Apply them to the hot path only: per-page and per-row loops of
copy, heap rewrite, key extraction, sort and leaf build. Setup, catalog and error paths favour
readability.

- **Measure before optimizing** (MEAS-01/04): a performance change cites a before/after run of the
  same command on the same CPU set, median of 3. No guessed optimizations.
- **No allocation per row** (ALLOC-01): buffers live per worker and are reused (`clear()`, not
  a new `Vec`); keys are borrowed slices until they enter a run.
- **Decide outside the loop** (BR-04): what is fixed per class or per index (domain, page treatment
  grade, key layout) is resolved before the row loop, as a chosen function or a generic
  parameter, not tested again on every row.
- **Per-worker state, merged once** (PAR-17, COH-12): counters and partial results are
  worker-local and combined at the end; shared counters are `Relaxed` atomics on their own cache
  line (`#[repr(align(64))]`, MEM-03). No lock taken per row.
- **Large sequential I/O** (SYS): read and write in batches of contiguous pages; one fsync pass at
  the end (ADR 0014).
- **Bounds checks**: let the compiler drop them with `chunks_exact`, iterators and
  checked-once slices; `get_unchecked` is not used.
