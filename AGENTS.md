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

  A later research doc gets a row here in the commit that first cites it.
- **Engine source**: `~/dev/cubrid-worktree/develop` on the dev host, release lines at
  `origin/release/<ver>`. Look symbols up with the workspace's
  `tools/code-index/code-index <checkout> definition|body|callers <name>`.

## Crate map

Roles: [README § Layout](README.md#layout). Dependencies point toward `ondisk` and never form a cycle:

```
ondisk   ← every other crate; the only crate that knows byte layouts
plan     → ondisk
copy     → ondisk, plan
catalog  → ondisk
index    → ondisk, catalog
verify   → ondisk, catalog
cli      → all of the above; the only binary
```

These are the allowed edges, not yet all wired. An edge outside this list, or a new crate,
updates this map (and README § Layout for a crate) in the same commit.

External crates, each with the reason it is not a few lines of our own (add a row with a new one):

| Crate | Used by | Why |
|---|---|---|
| `lz4_flex` | `ondisk::value` | LZ4 block format of compressed strings (engine `cubcompress::LZ4`) |
| `md-5` | `catalog::serial_name` | develop names long auto_increment serials with an MD5 of the 11.4 name |

## Files and modules

- **One module, one job.** Name the file after that job (`heap_hdr_stats.rs`, `patch_page.rs`),
  never `utils.rs` or `common.rs`. A file that grows a second concern is split into two.
  `lib.rs` and `mod.rs` only declare modules and re-export.
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
