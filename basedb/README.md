# basedb — baseline database builder

Scripts that build the **baseline databases** (기준 DB, see [GLOSSARY.md](../GLOSSARY.md)): small
databases made by the engine itself, one set per version, used to check on-disk structures
against real pages and to judge migrations. Decision and recorded results:
[T1 기준 DB 세트 (workspace#479)](https://github.com/xmilex-git/workspace/issues/479#issuecomment-6496835580).

The DBs already built on the dev host are the reference; **do not rebuild them to use them**.
A rebuild produces different log LSAs (append_lsa, page LSAs), so every number recorded in T1
and in later tickets stops matching. P1 and every later step start from the pristine copy,
taken with `cp -a --reflink=always <BASEDB_ROOT>/<label>/pristine/<variant> <your dir>`.

A pristine copy still names the **registered** DB: its `_vinf` and every volume header hold absolute paths under
`<BASEDB_ROOT>/<label>/<variant>/`. An engine run on a plain copy follows those paths and writes into the registered
DB (workspace#494 did this once with `addvoldb -S`), and `cubrid deletedb` on it deletes the registered volumes and
log (workspace#515; both restored from pristine). So:

- **When the engine opens a copy, the copy comes only from this helper** (`csql`, a server, any utility):

  ```sh
  bash basedb/safe_copy.sh <BASEDB_ROOT>/<label>/pristine/<variant> <new dir>   # then CUBRID_DATABASES=<new dir>
  ```

  It reflink-copies, rewrites `_vinf` to the copy's directory, writes the copy's `databases.txt` and prints the
  rewritten `_vinf`. The volume headers keep the old paths (the engine reads `_vinf` first).
- cubrid-fastmig reads a copy with `--volumes-in-first-dir`; `plan` and `migrate` refuse a source that reads a
  volume from another directory while a file of that name lies next to volume 0.
- Remove a copy by its literal path with `rm -rf`, never with `cubrid deletedb`.

## Run

```sh
bash basedb/build.sh <label: 10.2|11.0|11.2|11.3|11.4|develop> <install> <port> <variant: clean|dirty>
# or
BASEDB_INSTALL=<install> BASEDB_PORT=<port> just basedb <label> <variant>
```

- `<install>`: the CUBRID install to build with. T1 used `/home/cubrid/release/CUBRID-fastmig-11.4.6`
  (11.4.6.1963) and `/home/cubrid/release/CUBRID-fastmig-dev` (develop 682b080b2).
  The script overwrites `<install>/conf/cubrid.conf` on every run.
- Older releases (T4, workspace#512) build with reflink copies of the release installs, so no shared
  conf is rewritten: `/home/cubrid/release/CUBRID-fastmig-{10.2.18,11.0.16,11.2.9,11.3.6}`
  (10.2.18.9024, 11.0.16.0419, 11.2.9.0866, 11.3.6.1321). 10.2, 11.0 and 11.2 need `libncurses.so.5`,
  which the script takes from the workspace's `.git_ignored_dir/legacy-libs/ncurses5/lib64`. For these labels
  the script stops the `cub_master` it started (matched by `CUBRID=<install>`), so the next label does not
  register with a master of another version.
- `<port>`: a `cubrid_port_id` claimed beforehand; the server is started and stopped through
  the workspace's `cubrid-server-control` wrapper.
- Host paths come from the environment; the defaults are the dev host's:

  | Variable | Default | Used for |
  |---|---|---|
  | `BASEDB_ROOT` | `/home/cubrid/fastmig-basedb` | output root and size check |
  | `TOOLING` | `/home/cubrid/dev/workspace` | the `cubrid-server-control` wrapper |
  | `BASEDB_JAVA_HOME` | `/usr/lib/jvm/java-1.8.0-openjdk` | `javac`/`loadjava` for the Java SP |
  | `BASEDB_ROWS_MULTIPLIER` | `1` | rows multiplier k (GLOSSARY 행 배수); k > 1 runs `25_rows_multiplier.sql` and sets `log_max_archives=0` |
  | `BASEDB_CAP_MB` | `5120` | the size cap below; raise it with k |

- The script refuses to run when any output directory already exists; remove it by literal path first.
- Size cap: `du -sm $BASEDB_ROOT/<label>` must stay ≤ 5120 MB (du counts the reflinked pristine copy
  twice). One variant takes about 513 MB registered, 465–473 MB pristine, 17–23 MB unload.
- A multiplied DB (k > 1) is not a T1 DB: build it under its own `BASEDB_ROOT`, never into the T1 root.
  It is the large input of the speed-comparison baseline (workspace#484).

## Output

Under `$BASEDB_ROOT/<label>/` (`CUBRID_DATABASES` points here), DB name `fm<tag>_<variant>`, tag `102`, `110`, `112`, `113`, `114` or `dev`:

| Path | Content |
|---|---|
| `<variant>/` | the registered DB; lob-base-path `file:<this dir>/lob` |
| `pristine/<variant>/` | reflink copy taken right after the final shutdown — the starting point |
| `unload/<variant>/` | `unloaddb` output, the set-comparison reference |
| `evidence/<variant>/` | csql output, `counts.sql` result, `loghdr.txt` (append/chkpt LSA), `vacuum_pending.txt`, `pristine.sha256`, `unload.sha256`, `sizes.txt` |

After the diagnostic SA runs, the registered DB is rsynced back to the pristine bytes.

## Variants

- **clean**: load, full SA vacuum, clean shutdown. No vacuum work is left.
- **dirty**: clean, then `50_dirty.sql` on a server started with `vacuum_disable=yes` (10.2 and 11.0:
  `vacuum_master_interval_in_msecs=20000`, because they die with SIGFPE in `vacuum_get_log_blockid` on the
  first write when vacuum is disabled), then a
  clean shutdown. Vacuum work is left; any SA boot runs it, so work from the pristine copy.

## Scripts

Run in this order by `build.sh` (user, mode in parentheses):

| File | Makes | From |
|---|---|---|
| `00_users.sql` (dba) | users `app_owner`, `app_group`, `app_reader` (group member, password `fm_reader`), user comments | T1 |
| `10_schema.sql` (app_owner) | tables of every common type, collations, PK/FK/UNIQUE/composite/DESC indexes, AUTO_INCREMENT, RANGE/HASH/LIST partitions, overflow table `doc_big`; function and filter indexes on `g5_expr`, `g5_fx`, `region_list` (`i_f1`, `i_f2`, `i_f4`, `i_f5`) | T1; G5 (workspace#486) |
| `20_data.sql` (app_owner) | rows, all functions of `seq_n.n` so both versions load the same data; LOBs on 20 `product` rows | T1 |
| `25_rows_multiplier.sql` (app_owner, k > 1 only) | (k − 1) × the T1 rows of `customer`, `orders`, `order_item`, `event_log`, ids from 1000001; runs before the triggers exist | speed-comparison baseline (workspace#484) |
| `30_objects.sql` (app_owner) | serial, views (one WITH CHECK OPTION), triggers, synonym, PL/CSQL function and procedure, GRANTs | T1 |
| `31_dba_objects.sql` (dba) | dba tables (one `utf8_ko_cs`), synonym, Java SP `fm_hello` | T1; G6 (workspace#488) for `java/` |
| `40_post.sql` (app_owner) | record relocations, AUTO_INCREMENT/trigger/serial/SP use | T1 |
| `45_orphan_lob.sql` (app_owner, all but develop) | 10 orphan LOB files: DROP TABLE before develop leaves them behind | G6 (workspace#488) |
| `46_partition_fx.sql` (app_owner) | partition expressions with operators renumbered after 11.4, CHAR/NUMERIC constants, a HASH table with a NUMERIC column | R7 (workspace#487), T3 (workspace#489) |
| `50_dirty.sql` (app_owner, dirty only) | deletes and updates left for vacuum | T1 |
| `counts.sql` (dba, SA) | one row count per object and catalog counts | T1 |
| `FmHello.java` | class behind the Java SP, compiled to Java 8 and `loadjava`ed | T1 |

Fixtures the engine refuses are absent on purpose: 11.4.6 rejects a UNIQUE function index and
a NUMERIC partition key, so neither exists in an 11.4 source.

### Per-label overrides (T4, workspace#512)

`<label>/<file>.sql` replaces `<file>.sql` for that label. Each override is the common file minus what
that release rejects, and its header says what it drops:

| Label | Files | Drops or rewrites |
|---|---|---|
| 10.2 | `10_schema`, `30_objects`, `31_dba_objects`, `40_post`, `counts` | as 11.0, plus filter index `i_f5` rewritten on `(id, label)`: 10.2 ignores the filter when it compares keys on a partitioned table and rejects the duplicate key |
| 11.0 | `30_objects`, `31_dba_objects`, `40_post`, `counts` | as 11.2/11.3, plus no synonyms (11.2+) and no `db_synonym` count; the Java SP needs `cubrid javasp start`, which the script runs for 11.0 |
| 11.2, 11.3 | `30_objects`, `31_dba_objects`, `40_post` | no PL/CSQL function/procedure and their calls (11.4+), no `GRANT EXECUTE ON PROCEDURE` (11.4+), Java SP called unqualified (`fm_hello`, no owner prefix before 11.4) |

Catalog counts differ from 11.4 accordingly (auths 33/34/36/36 for 10.2/11.0/11.2/11.3 vs 38); data counts equal T1.

### Engine path for 10.2 references across owners (T6, workspace#516)

[`owner-qualified-names-102/`](owner-qualified-names-102/README.md) holds small 10.2 fixtures with views and triggers
that name other owners' tables, serials and SPs, run through 10.2 `unloaddb` → develop `loaddb --no-user-specified-name`.
It is not a baseline DB. Its `expected/` dumps are the engine's result: the stored texts, and how the views and
triggers behave. G8 (workspace#517) compares its text qualification with them. The T4 10.2 DBs fail on this path at
their first partitioned table owned by APP_OWNER.

### 10.2 partition expressions in every opcode range (T17, workspace#540)

[`partition-streams-102/`](partition-streams-102/README.md) builds the same partitioned tables on 10.2 and develop
(the three 10.2 → develop opcode ranges, CHAR results, a TO_CHAR language flag, `BENCHMARK` and `CURRENT_DATE`), so
`cubrid-fastmig partition-streams --from 10.2` compares each transcoded stream with develop's. It is not a baseline DB.

### Remaining edge cases of a 10.2..11.3 source (T26, workspace#554)

[`old-source-edge-cases/`](old-source-edge-cases/README.md) builds small 10.2, 11.2 and 11.3 fixtures for seven edge cases:
system class domains, system names inside literals, system class grants, stored procedure names with a dot, OBJECT
values on stored procedure rows, NCHAR HASH partitions and trailing 0x00. Each is taken to develop through
cubrid-fastmig and through `unloaddb` → `loaddb`, and the develop checks are run on both targets. It is not a
baseline DB.
