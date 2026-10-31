# e2e/ops — operations scenario runner
| `unload-prepared` | down | `unloaddb -S` of the side as prepared, by the target install, into `<side>/unload` (§ Official-unload-comparison) |

Takes one baseline DB (basedb/README.md) to the target version two ways — through cubrid-fastmig (the **migrated** side)
and through the engine's official path, source `unloaddb` → target `loaddb` (the **official** side) — then runs the
same **operations scenario** (운영 시나리오, [GLOSSARY.md](../../GLOSSARY.md)) on each side and compares what each step
leaves behind (**parity**). Decision: G12 [workspace#587](https://github.com/xmilex-git/workspace/issues/587); runner:
T36 [workspace#588](https://github.com/xmilex-git/workspace/issues/588). Users of the runner: the version matrix T37
[workspace#589](https://github.com/xmilex-git/workspace/issues/589) and HA T38
[workspace#590](https://github.com/xmilex-git/workspace/issues/590). This is a runtime check, not a unit test
(AGENTS.md § Build and check).

Pass: every step exits as expected, the official-unload-comparison finds the same entries, every parity diff is empty, the server error logs of both sides hold no assert
and no new core appeared, and the migrated side created at least `FASTMIG_OPS_MIN_ARCHIVES` log archives. Server
statistics and the wording of error logs are not compared (G12 1).

## Run

```sh
FASTMIG_OPS_INSTALL=<own copy of the optdebug target install> FASTMIG_OPS_PORT=<claimed port> \
FASTMIG_OPS_SERVER_CTL=<workspace>/.agents/skills/cubrid-server-control/scripts/cubrid-server-ctl.sh \
  taskset -c 0-15 just ops <BASEDB_ROOT>/<label>/pristine/<variant> <develop|11.4> <out dir> [rounds]
# same as: bash e2e/ops/run.sh target/release/cubrid-fastmig <pristine> <target version> <out dir> [rounds]
```

Arguments:

| Argument | Meaning |
|---|---|
| `<pristine>` | `<BASEDB_ROOT>/<label>/pristine/<variant>`; the label (`10.2`, `11.2`, `11.3`, `11.4`, `develop`) is the source version and picks the scenario SQL |
| `<target version>` | `develop` or `11.4`, as `plan`/`migrate --target-version` (the pairs of e2e/README.md) |
| `<out dir>` | a new directory; holds both sides' DBs while the run lasts, then only text |
| `[rounds]` | how many times the DML round runs in the `dml` step; default `FASTMIG_OPS_ROUNDS` or the measured default below. The 4.2 GB DB takes `1` |

Environment:

| Variable | Default | Meaning |
|---|---|---|
| `FASTMIG_OPS_INSTALL` | required | the target-line install, **optdebug** (G12 6), the caller's own reflink copy: `conf/cubrid.conf` is overwritten. Both sides run on it |
| `FASTMIG_OPS_PORT` | required | claimed `cubrid_port_id` |
| `FASTMIG_OPS_SERVER_CTL` | required | the workspace `cubrid-server-control` wrapper |
| `FASTMIG_OPS_ROUNDS` | `6` (§ Rounds) | as `[rounds]` |
| `FASTMIG_OPS_MIN_ARCHIVES` | `2` | log archives the migrated side must create during the scenario; `0` turns the check off (the 4.2 GB DB, 1 round) |
| `FASTMIG_OPS_WORKERS` | `4` | migrate worker limit |
| `FASTMIG_OPS_SOURCE_UNLOAD` | `<BASEDB_ROOT>/<label>/unload/<variant>/<db>` | the unloaddb prefix the official side loads (a 10.2 source: see e2e/README.md) |
| `FASTMIG_OPS_LOB_BASE` | none | required for a 10.2..11.3 source: the base its LOB locators name (as `FASTMIG_E2E_LOB_BASE`). The official side rewrites that prefix to its own copy of the files, since a LOB UPDATE deletes the old file |
| `FASTMIG_OPS_BREAK` | none | fail-before probe: `official-extra-row` inserts one `region_list` row on the official side only, right after its server starts; the run must then exit 1 with a parity diff from `baseline` on |

Exit: `0` pass, `1` a step, a parity diff, an assert/core or the archive count failed, `2` bad arguments or an
existing `<out dir>`.

## Sides and steps

Both sides keep the source DB name, each in its own `CUBRID_DATABASES` directory (`<out>/<side>/dbs`, volumes in
`<out>/<side>/db`), and run one after the other on the same install and port, `migrated` first. After every scenario
step the side runs `checks.sql` (step `check-<step>`) into `<side>/check/<step>.out`, always in CS mode (the Java SP runs in the server; a stopped server is started for the checks and stopped again):

| Step | Server | What |
|---|---|---|
| `prepare` | down | migrated: reflink copy → `migrate --verify --volumes-in-first-dir` → `recompile-plcsql.sql`, `recompile-views.sql`, `deferred-indexes.sql`, `update-statistics.sql` (csql -S) → `finalize` (deletes the source copy; skipped with `FASTMIG_OPS_LOB_BASE`). official: `createdb` with the source's volume 0 and log sizes → a reflink copy of the source's LOB files in `<out>/official/lob-src`, outside the lob base, every locator of a copy of the objects file pointed at it by an absolute path (§ Engine facts) → `loadjava` of the Java class → `loaddb -S` of the unload (`-s`, `-d`, `-i`, `--trigger-file`; 10.2: `--no-user-specified-name`) |
| `server-start` | up | `break-official-extra-row` follows on the official side when asked |
| `baseline` | up | checks only: the two paths agree before any write |
| `dml` | up | `[rounds]` × the DML round |
| `statistics` | up | `UPDATE STATISTICS ON ALL CLASSES` |
| `checkpoint` | up | `;checkpoint` in `csql --sysadm` |
| `kill9-recovery` | up | `kill -9` of the side's `cub_server`, start again (restart recovery) |
| `vacuumdb` | down | `vacuumdb -S` |
| `addvoldb` | down | `addvoldb -S --db-volume-size=32M` |
| `compactdb` | down | `compactdb -S` |
| `backupdb` | down | `backupdb -S -D <side>/backup` (full) |
| `dml-after-backup` | up | one DML round after the backup |
| `restoredb` | down | `restoredb -B <side>/backup` up to the end of the log: the state of `dml-after-backup` |
| `deletedb` | down | `deletedb -d` (volumes, log and backup) |

The DML round is `dml/round.sql` (every source), plus `dml/round_synonym.sql` (11.2 and later) and
`dml/round_plcsql.sql` (11.4 and later), all run as dba. It inserts, updates and deletes over every kind of object of
the baseline DB: tables of every type, PK/FK (with ON DELETE CASCADE)/unique/plain/composite DESC/function/filter
indexes, the serial and AUTO_INCREMENT columns, the views (an update through WITH CHECK OPTION), both triggers, RANGE,
HASH and LIST partitions and the partition-expression tables (rows move between partitions), overflow records, the
synonyms, the Java and PL/CSQL SPs, a GRANT and REVOKE, the `utf8_en_ci`/`utf8_ko_cs` columns, JSON, SET, BIT and LOB
columns. Each new key comes from the current max or count, so both sides and every round write the same rows; the
`event_log` payload rotation is the round's big write (it is what fills the log toward archives).

`checks.sql` prints, per table, the row count and a CRC32 sum over its columns; the catalog (classes, indexes,
triggers, serial names, SPs, users, grants); what the views and the Java SP return; and, per index, the count read
through that index (`USING INDEX <i>(+)`) against the count of the same predicate with `USING INDEX NONE`. A line
`INDEX!=HEAP` fails the check on that side; the two sides' outputs are compared for parity.

Values a serial or an AUTO_INCREMENT column generates are compared as counts and order only (the order of
`audit_log.id` and `invoice.inv_no` follows the rows behind them), never as values: the official path starts a cached
serial ahead by (`CACHE` − 1) × `INCREMENT` (11.4 target), develop's `cached_num` of an AUTO_INCREMENT serial differs
(docs/intended-differences.md, rows "position of a started serial with a cache" and "`_db_serial.cached_num`"), and a
`kill -9` loses each side's cache block. Decision approved by the map supervisor 2026-10-31 (workspace#588).
Left out of the CRC32 sums for that reason, and only these:

| Column | Generated by | Compared instead |
|---|---|---|
| `app_owner.customer.id` of the rows a DML round inserts (`code` `R%`) | AUTO_INCREMENT | the row count; the rows by `code` and every other column |
| `app_owner.audit_log.id` | AUTO_INCREMENT (trigger `trg_orders_ai`) | the row count, `audit_log_order` (ids rise with `ref_id`) |
| `app_owner.invoice.inv_no` | serial `s_invoice` | the row count, `invoice_order` (numbers rise with `order_id`) |
| `db_serial` current value, `cached_num` | the serials themselves | the serial names only |

Why no unloaddb and set comparison of the two sides after the scenario: both unloads then hold the generated values
above in every row that carries them, so they could match only through rules that hide those values. The runner judges
the state after the DML by `checks.sql` only, and compares the two unloads once, before any write (next section).

## Official-unload-comparison

The set comparison of the end-to-end run (e2e/README.md) compares the migrated target with the **source**'s unload,
so what the target engine derives itself on the official path, and the migration copied from the source, passes there:
the view column casts of [workspace#599](https://github.com/xmilex-git/workspace/issues/599) were byte-equal to the
source and differed from what develop's loaddb stores. This step closes that hole (T41
[workspace#600](https://github.com/xmilex-git/workspace/issues/600)): each side is unloaded right after `prepare`
(`unload-prepared`; the migrated side after its required SQL), both by the target install, and after both sides

```sh
cubrid-fastmig set-comparison <out>/official/unload/<db> <out>/migrated/unload/<db> --rules official \
  --source-version <label> --target-version <target> --diff <out>/parity/official-unload.diff
```

writes `parity/official-unload.out` and the row `parity official-unload-comparison 0 <rc>` (rc 0 same, 1 differ, 2 a
side has no unload or a usage error); any rc but 0 fails the run. The OFFICIAL rules mask only the rows of
[docs/intended-differences.md](../../docs/intended-differences.md) the pair shows, one rule each, and print how many
entries each changed:

| Rule | Pairs | Row it masks |
|---|---|---|
| `class-id-names` | all | not a state: the migration keeps the source's class ids, loaddb numbers classes in unload order |
| `password-hash` | all | not a state: develop re-salts an unsalted hash on every unload (CBRD-27432, workspace#582) |
| `lob-absolute-locator`, `lob-class-dir` | all | LOB locator: absolute on the official side (§ Engine facts), relative on the migrated side; `ces_NNN/<file>` still compared |
| `official-reuse-oid` | 10.2 source | "REUSE_OID flag of a user table of a 10.2 source" |
| `official-auto-increment-11-2` | 11.2 source | the 11.2 AUTO_INCREMENT serial `current_val`/`started` pair, rewritten to its next value on both sides |
| `official-serial-cache-position` | 11.4 target | "position of a started serial with a cache": the official `START WITH` moved back by (CACHE − 1) × INCREMENT |

Never masked: view query casts (the target must store the casts loaddb stores), deferred indexes and recompiled views
(the migrated side is unloaded after its required SQL ran), and everything else. On the 18 small rows of the T37
matrix the step finds the same entries (workspace#600 resolution).

## Output

```
<out>/meta.txt                  arguments, binary sha256, install, rounds, archives created, load and free disk before/after
<out>/steps.tsv                 side  step  seconds  rc     (side: migrated | official | parity)
<out>/<side>/log/<step>.log     the step's own output
<out>/<side>/check/<step>.out   checks.sql after the step
<out>/<side>/server-log/        the install's log files written while the side ran
<out>/<side>/unload/            unloaddb of the side right after prepare
<out>/parity/<step>.diff        diff of the two sides' check/<step>.out; empty = same
<out>/parity/official-unload.{out,diff}  set-comparison --rules official of the two unloads
<out>/server-log-summary.tsv    side  error_lines ("ERROR ***")  assert_lines  cores (any process of the side)  pl_drops
                                session_expired (the -1066 lines, not in error_lines: § Engine facts)
<out>/archives.tsv              side  last_archive_before  last_archive_after   (numbers of <db>_lgarNNN, -1 = none;
                                before = at baseline, after = after dml-after-backup)
```

`steps.tsv` has one `parity <step> 0 <rc>` row per checked step (rc 1 = the diff is not empty). T37 builds its table
from `steps.tsv`, `archives.tsv` and `server-log-summary.tsv`.

## Reuse

The side functions (argument setup, timed steps, server start and stop, `prepare_migrated`, `prepare_official`, the
server log summary, and the scenario pieces: checks, DML rounds, kill -9, archive numbers, fault counts) live in
[sides.sh](sides.sh), which the application query driver [queries/](queries/README.md) (T40
[workspace#595](https://github.com/xmilex-git/workspace/issues/595)) and the long run [longrun/](longrun/README.md) (T39
[workspace#594](https://github.com/xmilex-git/workspace/issues/594)) source too.

### HA (T38)

`dml/round.sql` and `checks.sql` are plain csql files: T38 runs them against its master and slave without this runner.

## Rounds

Default rounds: **6**. Measured on T1 11.4 clean → develop (optdebug develop 682b080b2, `--no-clone`, 64 MB log, 2026-10-31,
workspace#588). Archives the migrated side created between `baseline` and `dml-after-backup`, which adds one more round:

| `[rounds]` | DML rounds in all | archives created (migrated) | official |
|---|---|---|---|
| 1 | 2 | 1 | 1 |
| 4 | 5 | 1 | 2 |
| 5 | 6 | 2 | 3 |

5 is the smallest count that reaches 2 archives; the default adds one round of margin. One round takes about 4 s on that
DB. The 4.2 GB DB takes 1 round with `FASTMIG_OPS_MIN_ARCHIVES=0` (G12 5).

## Engine facts the runner works around

- **LOB files of the official side.** An 11.4 or develop unload names each LOB file relative to the source's lob-base
  (`file:ces_471/...`). Copied under a develop target's lob-base, such a file does not survive: develop removes the
  top-level `ces_*` directories of the lob-base at every boot as temporary (CBRD-23700), loaddb's SA boot included,
  and loaddb keeps the relative locator. Measured on develop 682b080b2: loaddb exits 0, and every product LOB is gone
  before the first query. The runner therefore keeps a reflink copy of the source's LOB files outside the lob-base
  (`<out>/official/lob-src`) and points every locator of a copy of the objects file at it by an absolute path, which
  loaddb keeps as is. The unload under `BASEDB_ROOT` is only read. Engine behaviour, outside this map (map supervisor
  2026-10-31).
- **No UPDATE of a LOB column.** On develop 682b080b2 a LOB written by an UPDATE of a table that has a BEFORE UPDATE
  trigger keeps a top-level `ces_*` locator, and the next boot deletes it: the committed value is lost. It happens on a
  fresh DB as well (`create table t (id int primary key, price int, b blob); create trigger trg_bu before update on t
  if new.price < 0 execute reject; insert ...; update t set b = bit_to_blob(X'AA') where id = 1;`, then a second
  `csql -S`: "External file ces_551/... was not found"). `product` has such a trigger, so the DML round inserts and
  deletes LOB rows instead. 11.4.6 keeps the value; filed as [#592](https://github.com/xmilex-git/workspace/issues/592).
- **Views over a REUSE_OID table are not updatable** on develop ("Vclass ... is not updatable ... REUSE_OID"); the
  round updates `product` directly and `checks.sql` reads `v_active_product`.
- **`auto_restart_server=no`**: otherwise the master revives the server `kill9-recovery` kills, and the runner's own
  start then finds the DB in use.
- **PL server start**: the server reports started before its PL server accepts calls, so `start` waits up to 60 s
  for its socket to listen and then for `dba.fm_hello`.
- **"-1066 Session expired"** error lines (session.c) come from the engine, on both sides and on a source never
  migrated: one per csql session that ran a join with a GROUP BY sort (develop 682b080b2 and 11.4.6; T40
  [workspace#595](https://github.com/xmilex-git/workspace/issues/595)). `server-log-summary.tsv` counts them as
  `session_expired` and leaves them out of `error_lines`.
- **10.2 official path**: 10.2 `unloaddb` → develop or 11.4 `loaddb --no-user-specified-name` fails at the first
  partitioned table owned by a non-DBA user (`ALTER CLASS [event_log] PARTITION BY …`: `Unknown class "dba.event_log"`,
  schema rolled back; the server's lock-hint lookup has no unqualified-name fallback, engine defect measured in
  [workspace#516](https://github.com/xmilex-git/workspace/issues/516)). `prepare_official` loads a copy of a 10.2
  schema in which the `change_owner` call of each partitioned table follows its `PARTITION BY` statement
  ([official_102_schema.awk](official_102_schema.awk)); the other calls stay in place, since a view whose tables still
  belong to dba stores `[dba.<t>]`. develop and 11.4.6 optdebug then load T1 10.2 clean and dirty with 0 failed objects,
  partitioned tables owned by APP_OWNER, both views on `[app_owner.*]` and readable (workspace#589).
- **core_pattern's directory may be a symlink** (`/data/core` on the dev host): cores are found with `find -H`; before
  2026-10-31 the runner found none there (workspace#594), so the core counts of earlier runs are not evidence.

## Known flakes

- **PL server drop** (cause unknown; 1 of 2 runs of the same build and settings, 2026-10-31): mid-run, every SP call
  failed with "Networking with PL server failed: -1" until the server restarted its PL server; no core, no OOM, no
  error in the PL log. It fails the run (error lines, checks). `server-log-summary.tsv` counts those lines as
  `pl_drops`, so a failure from it is told apart from a migration finding. Two more occurrences go to an issue.

## Migration defects it found

- [#591](https://github.com/xmilex-git/workspace/issues/591): the clone path zeroed the headers of unallocated pages,
  and crash recovery after `kill -9` asserted in `pgbuf_set_bcb_page_vpid` on the migrated side (official side
  passed). Fixed in cubrid-fastmig 81c18f0; the default (clone) run then passes end to end (§ Verified runs).

## Verified runs

T1 11.4 clean → develop, optdebug develop 682b080b2 (`~/optdebug/CUBRID-fastmig-dev` reflink copy), default clone path,
6 rounds, cubrid-fastmig 6f67eab (holds the #591 fix 81c18f0), 2026-10-31, workspace#588:

| Run | Result |
|---|---|
| pass | exit 0; 66 steps all as expected; 11 parity diffs empty; error lines, asserts, cores, PL drops 0 on both sides; 3 archives on the migrated side |
| `FASTMIG_OPS_BREAK=official-extra-row` | exit 1; every step 0; all 11 parity diffs non-empty from `baseline` on, each the one extra `region_list` row (count 100 vs 101, `p_ns` 50 vs 51) |

## Disk

One DB lives at a time: the migrated side's source copy is finalized away in `prepare`, and the official side starts
after the migrated side's DB, log and backup are deleted (`deletedb -d`). With `FASTMIG_OPS_LOB_BASE` the source copy
stays in `<out>/migrated/source`; remove it by its literal path. A failed side is deleted with `deletedb -d` too;
`<out>` keeps text only.
