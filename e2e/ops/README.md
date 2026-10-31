# e2e/ops — operations scenario runner

Takes one baseline DB (basedb/README.md) to the target version two ways — through cubrid-fastmig (the **migrated** side)
and through the engine's official path, source `unloaddb` → target `loaddb` (the **official** side) — then runs the
same **operations scenario** (운영 시나리오, [GLOSSARY.md](../../GLOSSARY.md)) on each side and compares what each step
leaves behind (**parity**). Decision: G11 [workspace#587](https://github.com/xmilex-git/workspace/issues/587); runner:
T36 [workspace#588](https://github.com/xmilex-git/workspace/issues/588). Users of the runner: the version matrix T37
[workspace#589](https://github.com/xmilex-git/workspace/issues/589) and HA T38
[workspace#590](https://github.com/xmilex-git/workspace/issues/590). This is a runtime check, not a unit test
(AGENTS.md § Build and check).

Pass: every step exits as expected, every parity diff is empty, the server error logs of both sides hold no assert
and no new core appeared, and the migrated side created at least `FASTMIG_OPS_MIN_ARCHIVES` log archives. Server
statistics and the wording of error logs are not compared (G11 1).

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
| `FASTMIG_OPS_INSTALL` | required | the target-line install, **optdebug** (G11 6), the caller's own reflink copy: `conf/cubrid.conf` is overwritten. Both sides run on it |
| `FASTMIG_OPS_PORT` | required | claimed `cubrid_port_id` |
| `FASTMIG_OPS_SERVER_CTL` | required | the workspace `cubrid-server-control` wrapper |
| `FASTMIG_OPS_ROUNDS` | measured default | as `[rounds]` |
| `FASTMIG_OPS_MIN_ARCHIVES` | `2` | log archives the migrated side must create during the scenario; `0` turns the check off (the 4.2 GB DB, 1 round) |
| `FASTMIG_OPS_WORKERS` | `4` | migrate worker limit |
| `FASTMIG_OPS_SOURCE_UNLOAD` | `<BASEDB_ROOT>/<label>/unload/<variant>/<db>` | the unloaddb prefix the official side loads (a 10.2 source: see e2e/README.md) |
| `FASTMIG_OPS_LOB_BASE` | none | as `FASTMIG_E2E_LOB_BASE` for a 10.2..11.3 source |
| `FASTMIG_OPS_BREAK` | none | fail-before probe: `official-extra-row` inserts one row on the official side only after `baseline`; the run must then exit 1 with a parity diff from that step on |

Exit: `0` pass, `1` a step, a parity diff, an assert/core or the archive count failed, `2` bad arguments or an
existing `<out dir>`.

## Sides and steps

Both sides keep the source DB name, each in its own `CUBRID_DATABASES` directory, and run one after the other on the
same install and port (`migrated` first). `prepare` makes the side's DB; the scenario steps then run in this order,
and after each one the side runs `checks.sql` into `<side>/check/<step>.out`:

| Step | What |
|---|---|
| `prepare` | migrated: reflink copy → `migrate --verify --volumes-in-first-dir` → `deferred-indexes.sql`, `recompile-plcsql.sql`, `update-statistics.sql`. official: `createdb` with the source's volume and log sizes → `loaddb` of the unload (`-s`, `-d`, `-i`, `--trigger-file`) → Java class `loadjava` and LOB files under the side's lob base, as the official path needs them |
| `baseline` | server start, no change: the two paths agree before any write |
| `dml` | `[rounds]` × the DML round (`dml/round.sql`): INSERT/UPDATE/DELETE over every kind of object of the baseline DB (tables, PK/FK/unique/plain/function/filter indexes, serial and AUTO_INCREMENT, views incl. WITH CHECK OPTION, triggers, RANGE/HASH/LIST partitions, synonyms, Java and PL/CSQL SPs, users and grants, collations, JSON, LOB) |
| `statistics` | `UPDATE STATISTICS ON ALL CLASSES` |
| `checkpoint` | `;checkpoint` in `csql --sysadm` |
| `kill9-recovery` | `kill -9` of `cub_server`, start again (restart recovery) |
| `vacuumdb` | server stop, `vacuumdb -S` |
| `addvoldb` | `addvoldb -S` of one data volume |
| `compactdb` | `compactdb -S` |
| `backupdb` | `backupdb -S` (full) into `<side>/backup` |
| `dml-after-backup` | one DML round after the backup |
| `restoredb` | `restoredb -S` from that backup up to the end of the log: the same state as `dml-after-backup` |
| `final` | `unloaddb` of the side; then `set-comparison` of migrated against official (step `parity-unload`) |

`checks.sql` (per source label, like basedb) holds `basedb/counts.sql`, a content digest per table and, per index, a
count read through that index (`USING INDEX`, a range covering every key) next to the heap count: index = heap on each
side, the same numbers on both sides.

Values generated by a serial or an AUTO_INCREMENT column are compared as counts and order only, never as values: the
started serial position and `cached_num` are intended differences (docs/intended-differences.md), and a `kill -9`
loses each side's cache block.

## Output

```
<out>/meta.txt                    arguments, binary sha256, install, rounds, load and free disk before/after
<out>/steps.tsv                   one row per step: side  step  seconds  rc   (side: migrated | official | parity)
<out>/<side>/log/<step>.log       the step's own output
<out>/<side>/check/<step>.out     checks.sql after the step
<out>/parity/<step>.diff          diff of migrated/check/<step>.out and official/check/<step>.out; empty = same
<out>/parity/unload.diff          set-comparison diff of the two final unloads
<out>/server-log-summary.tsv      per side: error lines, assert lines, new cores (pid-matched under core_pattern)
<out>/archives.tsv                per side: log archives created during the scenario
```

`steps.tsv` has a `parity <step> 0 <0|1>` row per scenario step (1 = the diff is not empty); T37 builds its table from
`steps.tsv`, `archives.tsv` and `server-log-summary.tsv`.

## Reuse for HA (T38)

`dml/round.sql` and `checks.sql` are plain csql files: T38 runs them against its master and slave without this runner.

## Rounds

Default rounds: _measured on T1 11.4 clean → develop, recorded here with the archive count (workspace#588)._

## Disk

At most two DB copies live at once: the migrated side's source copy is finalized away after `prepare`, and the
official side starts after the migrated side's DB is deleted. Both sides' DBs and backups are removed (registered:
`cubrid deletedb`; a reflink copy: by literal path) before the runner exits; `<out>` keeps text only.
