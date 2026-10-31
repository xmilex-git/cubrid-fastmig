# e2e/ops/longrun — long run of the operations scenario

The T36 runner ([../README.md](../README.md)) runs the operations scenario for minutes. The **long run** (긴 운영,
[GLOSSARY.md](../../../GLOSSARY.md)) keeps the migrated DB and the official-path DB up all night under the same paced
DML and hourly operations, then restores and PITRs both at the end. Ticket: T39
[workspace#594](https://github.com/xmilex-git/workspace/issues/594); decisions: G12
[workspace#587](https://github.com/xmilex-git/workspace/issues/587); schedule approved by the map supervisor
2026-10-31. Runtime check, not a unit test.

Pass, per path: both sides exit 0 — every step as expected, at least `MIN_ARCHIVES` log archives created and
`MIN_CHECKPOINTS` checkpoints, restore and PITR equal to the state they must reproduce, the restore to before the
move refused, no error line, assert or core — and every check of the migrated side equals the official side's
(parity). RSS and volume size per cycle are tabulated side by side (`summary.md`) and judged by the reader: same
shape, not same numbers.

## Run

```sh
FASTMIG_LONGRUN_INSTALL=~/optdebug/CUBRID-fastmig-dev FASTMIG_LONGRUN_PORTS="<p1> <p2> <p3> <p4>" \
FASTMIG_OPS_SERVER_CTL=<workspace>/.agents/skills/cubrid-server-control/scripts/cubrid-server-ctl.sh \
  taskset -c 0-15 just ops-longrun <out dir>
# same as: bash e2e/ops/longrun/run.sh target/release/cubrid-fastmig <out dir>
```

The paths are the rows of [paths.tsv](paths.tsv) (T39: 11.4 clean → develop by the default clone path, develop →
develop), each on two sides, so four servers run at once, each on its own reflink copy of the install
(`<out>/installs/<path>-<side>`, its conf overwritten) and its own port. Ports go to the rows in order: row 1
migrated, row 1 official, row 2 migrated, row 2 official.

| Variable | Default | Meaning |
|---|---|---|
| `FASTMIG_LONGRUN_INSTALL` | required | optdebug develop install (G12 6); only copied |
| `FASTMIG_LONGRUN_PORTS` | required | two claimed `cubrid_port_id`s per path |
| `FASTMIG_OPS_SERVER_CTL` | required | the workspace `cubrid-server-control` wrapper |
| `FASTMIG_LONGRUN_BASEDB_ROOT` | `~/fastmig-basedb` | T1 baseline DBs (basedb/README.md) |
| `FASTMIG_LONGRUN_PATHS` | all rows | ids of paths.tsv, space-separated |
| `FASTMIG_LONGRUN_CYCLES` | `8` | cycles (≥ 8 hours at the default length) |
| `FASTMIG_LONGRUN_ROUNDS` | `30` | DML rounds per cycle (about 0.33 archives a round, T36 measure: ≥ 50 archives in 8 cycles) |
| `FASTMIG_LONGRUN_CYCLE_SECONDS` | `3600` | length of a cycle |
| `FASTMIG_LONGRUN_KILL_EVERY` | `4` | every this many cycles: kill -9, vacuumdb, restart, a new backup chain |
| `FASTMIG_LONGRUN_PITR_CYCLE` | cycles − 2 | the cycle whose end holds the PITR mark |
| `FASTMIG_LONGRUN_MIN_ARCHIVES` | `50` | archives a side must create after the move |
| `FASTMIG_LONGRUN_MIN_CHECKPOINTS` | `100` | checkpoints a side must complete, over all its server lifetimes |
| `FASTMIG_LONGRUN_STAT_SECONDS` | `60` | window of the statdump in each snapshot (§ One side, Counters) |
| `FASTMIG_LONGRUN_MAX_ARCHIVES` | `40` | `log_max_archives` of every side (§ One side, Archives) |
| `FASTMIG_LONGRUN_PREALLOC_MB` | `1536` | size of the volume each side adds before its first backup (§ One side, Volumes) |
| `FASTMIG_LONGRUN_CAP_GB` | `50` | disk cap of the size estimate (§ Disk) |
| `FASTMIG_LONGRUN_GROWTH_MB` | `2` | MB one DML round adds to a side, for the estimate (§ Disk) |
| `FASTMIG_LONGRUN_BREAK` | none | `pitr-extra-row`: § Fail-before probe |
| `FASTMIG_OPS_WORKERS` | `4` | migrate worker limit |

Exit: `0` pass, `1` fail, `2` bad arguments, an existing `<out>`, a non-optdebug or 11.4 install, or the disk gate.
`<out>/result` is written last, first line `RESULT: OK` or `RESULT: FAILED`.

## One side

`side_loop.sh` prepares the side exactly as the T36 runner (`../sides.sh`: migrate `--verify` → finalize, or createdb →
loaddb), with `checkpoint_interval=2min` and `log_max_archives=MAX_ARCHIVES` added to the conf, and starts the server. Then:

| When | Steps (`<side>/log/<step>.log`, rows of `<path>/<side>/steps.tsv`) |
|---|---|
| start | `check-baseline`; `c00-backup` (level 0, chain `chain-c00`); snapshot row 0 |
| every cycle `cNN` | `cNN-dml`: `ROUNDS` DML rounds spread over the first 80 % of the cycle, then `check-cNN-dml` · `cNN-statistics` (UPDATE STATISTICS) · `cNN-backup` (online `backupdb -C`) · `cNN-snapshot` (row of `hours.tsv`) · sleep to the cycle's end |
| cycle `PITR_CYCLE` | after the DML: `cNN-pitr-mark`: the PITR mark, a reflink copy of the lob-base into `<side>/pitr-lob`, then `check-pitr-snapshot` |
| cycle multiple of `KILL_EVERY` | before the backup: `cNN-kill9` (`kill -9` of the server and its `cub_pl`), `cNN-vacuumdb` (`-S`: its boot runs crash recovery, then vacuum), `cNN-compactdb` (first such cycle only), `cNN-start`, `check-cNN-restart` |
| end | `check-end`, length check, server stop, § End, `deletedb -d` |

The server is never stopped between kill -9 cycles: vacuum is the server's own (G12; map supervisor 2026-10-31), and
the snapshot records its progress with the RSS.

**Counters.** `cubrid statdump` counts from its own start (it prints zeros when run once), so the snapshot runs
`statdump -i STAT_SECONDS -c` and keeps the second dump: `Num_vacuum_log_pages_vacuumed` and `..._to_vacuum` over that
window. Checkpoints are the `Checkpoint finished` lines of the side's server error logs, over every server lifetime.
The `-1066 Session expired` lines are counted apart (`session_expired`), not as error lines (../README.md § Engine facts).

**Backups.** Level 0 at cycle 0 and every `KILL_EVERY` cycles, each in a new chain directory
`<side>/backup/chain-cNN`; the next cycle adds a level 1 and the later ones a level 2 (since that level 1) to the
current chain. Volume names are `<db>_bk<level>v<unit>` (file_io.c), so a later level 2 replaces an earlier one and the
chain stays whole; one directory per chain because a level 0 would overwrite the level-0 volume an earlier PITR needs.
restoredb reads only the level 0 unless `-l` is given (util_admin.c, logpb_restore), so the `latest` and `pitr` probes
pass the level of the chain's last backup before their time.

**Volumes.** Right after the move each side adds one generic volume of `PREALLOC_MB` (`addvoldb -S`, step `prealloc`),
before its first backup, so no volume grows or is added during the run. A volume that grows or is added after a
chain's level 0 breaks the restore of that chain's levels 1 and 2: an incremental backup leaves out pages whose LSA is
still NULL, and restoredb fills them only from level 0 (engine defect
[workspace#601](https://github.com/xmilex-git/workspace/issues/601), develop and 11.4.6; optdebug asserts in
`pgbuf_set_bcb_page_vpid`, release skips the pages). An operator who takes incremental backups pre-sizes volumes the
same way. Night run 1 grew a side by up to 237 MB in three cycles (64 and 128 MB steps), so 1536 MB covers 8 cycles
with margin. Night runs 1 and 2 (stopped at cycles 3 and 1) ran without this step.

**Archives.** The engine deletes archives beyond `log_max_archives` without regard to backups
(`logpb_remove_archive_logs_exceed_limit`), and a restore stops at a missing one (restoredb asks for it, reads EOF from
the side's stdin and quits). A restore replays the archives from the backup it starts at: the PITR from the chain's
last backup before the mark (default: cycle 5's level 1 to cycle 6's mark, one cycle), `latest` from the newest
chain's last backup. 40 archives hold about five cycles at 8 a cycle, and 8 cycles still delete some. The smoke with
`MAX_ARCHIVES=2` shows a PITR that passes while archives are being deleted (§ Smoke).

## End

After `check-end` the server stops and `recovery.sh` restores. Each restore goes into its own empty directory
`<side>/restore/<probe>` with its own databases.txt (`restoredb -u`), the side's active log and archives copied in
and a copy of the lob-base, is started there for its checks, stopped and removed at once by its line of `<out>/cleanup/<path>-<side>.sh`, which
`run.sh` writes before the run with every `rm -rf` path spelled out (house rule: literal paths only). The side's DB is
not touched. A
restore over the running directory or over an earlier restore keeps files the backup does not hold, with page LSAs
newer than the PITR stop: the first smoke asserted in `log_recovery.c:531` that way (official side, 2026-10-31). Results in `<side>/recovery.tsv` (probe, rc, wanted rc, diff rc):

| Probe | restoredb | Must equal |
|---|---|---|
| `latest` | `-B` newest chain `-l <its last level>`, up to the end of the log | `check/end.out` |
| `pitr` | `-B` the mark's chain `-l <its last level before the mark>` `-d <mark>`, then the lob-base copy of the mark put back | `check/pitr-snapshot.out` |
| `chain0-bktime` | `-B chain-c00 -d backuptime` | recorded; when it restores, `check/baseline.out` (nothing wrote before it) |
| `before-move` | `-B chain-c00 -d <move − 60 s>` | refused with "… must be after the time … of the specified backup" (and no other failure) |
| `chain0-next` | `-B chain-c00 -d <cycle 1's backup>` (level 0, replays cycle 1's archives) | recorded only (§ Restorable range) |

Each restore directory also gets the side's `<db>_keys` and its `java`, `java_static` directories: restoredb does not
restore them, and the server needs them where its volumes are. `chain0-bktime`, `before-move` and `chain0-next` (the
range probes) run after the error lines and cores are counted: a refusal is an error line by design, and a restore
that needs an archive `log_max_archives` already deleted asks for it, reads EOF and aborts with `LOG FATAL ERROR:
log_fetch_from_archive` and a core (smoke with 3 kept archives, 2026-10-31); their cores are kept as `range_cores` in
faults.tsv. Asserts are counted after them. Each restore
into an empty directory first logs `-551 Unable to locate volume information path file <probe>/db/<db>_vinf` (the
file it is about to write): those lines are left out of the error lines and kept as `restore_fresh_dir` in faults.tsv.

**LOB files.** They live outside the volumes, the backups and the log, and a round deletes the files of the rows it
deletes: a PITR to before such a DELETE restores rows whose files are gone ("External file ... was not found", measured
in the smoke). An operator keeps a file copy of the lob-base for that; the runner copies it at the mark and puts it
back after the PITR restore. The other probes restore states whose files still exist.

## Restorable range

A restore starts at a backup, and the first backup of either side is taken right after the move (chain-c00): no
backup holds the DB before the move, so the restorable range starts at chain-c00's backup time. `before-move` shows
the engine refusing a time before that backup ("must be after the time … of the specified backup",
`logpb_check_stop_at_time`); any DB refuses so, it is not a property of the migration. The range also ends where the archives end: even
`chain0-bktime` (`-d backuptime`) needs the archive written during its online backup, so once MAX_ARCHIVES newer
archives exist the first chain no longer restores at all; `chain0-next` replays cycle 1's archives as well. Their rc
records where the range starts at the end of the run. An operator who needs PITR back to a given time keeps the archives from the
backup before it to that time: a `log_max_archives` that covers that span, or the archives copied off with the backups. The measured rc of both is the documented range (docs/ops-verification.md "Long run").

## Fail-before probe

`FASTMIG_LONGRUN_BREAK=pitr-extra-row` inserts one `region_list` row on both sides right after the PITR mark and before
its snapshot. The PITR restore stops at the mark and lacks the row, so both sides must fail on `pitr`
(`recovery/pitr.diff` the one row), while parity stays equal. Run it short (§ Smoke).

## Output

```
<out>/meta.txt, sides.tsv, summary.md, result
<out>/installs/<path>-<side>/            install copies (remove by literal path)
<out>/cleanup/<path>-<side>.sh           the literal rm -rf lines of that side's restored DBs and pitr-lob (generated)
<out>/<path>/<side>.console              the side process's stdout and stderr
<out>/<path>/<side>/                     the side's out dir (sides.sh ops_setup); the side's own files one level down:
<out>/<path>/<side>/steps.tsv            side step seconds rc
<out>/<path>/<side>/<side>/hours.tsv     one row per cycle: rounds, server pid, RSS, checkpoints, vacuum counters,
                                         archives (first, last, kept), volume, backup and free /home MB, faults so far
<out>/<path>/<side>/<side>/times.tsv     moved, backup (cycle, level, chain) and pitr (mark) events
<out>/<path>/<side>/<side>/{check,log,stat,recovery,server-log}/, length.tsv, recovery.tsv, faults.tsv, cores.txt, rc
<out>/<path>/<side>/<side>/backup/chain-cNN/   backup chains (remove by literal path)
<out>/<path>/<side>/<side>/pitr-lob/      the lob-base at the PITR mark; removed by its cleanup line after the PITR probe
<out>/<path>/<side>/<side>/restore/<probe>/   a restored DB while its checks run; removed by its cleanup line
<out>/<path>/parity/<check>.diff         migrated against official; empty = same (rows in <out>/<path>/parity.tsv)
```

## Watching

`<out>/summary.md` is rewritten every minute while `run.sh` runs and holds each side's last cycle row and the step it
is in now with that step's age (its newest step log). A running side whose step age passes 30 minutes is stalled (a
cycle's longest quiet stretch is its end sleep, under 12 minutes; the dml step log grows every round). A watcher polls with a bound longer than the whole run (cycles × cycle seconds + 2 h for
prepare and the end) until `<out>/result` exists, or the `run.sh` process is gone without it (failure), and reads
`summary.md` each hour.

Stopping: TERM or INT to `run.sh` stops the side processes (their whole process group when `run.sh` leads one, as
under `setsid`), then every `cub_server`, `cub_master` and `cub_pl` whose environment holds `CUBRID=<out>/installs/`,
and writes `RESULT: FAILED (stopped …)`; `stop.log` lists what it signalled. Otherwise a side would go on to its end
and start restored DBs on ports already released.

## Disk

Per side: `grown = pristine + cycles × rounds × GROWTH_MB`, plus `PREALLOC_MB`, plus `(MAX_ARCHIVES + 3) × active log` (the kept
archives, the active log and two spare), plus `(cycles / KILL_EVERY + 1) × 2 × grown` for the chains, plus `1 × grown` for the one restored DB of § End that exists at a time. Every side added up must stay under
`FASTMIG_LONGRUN_CAP_GB`, and `/home` must have twice that free, or `run.sh` exits 2. T1 baseline (465 MB, 64 MB log),
defaults: about 11.8 GB per side, 47 GB in all (cap 50 GB, map supervisor 2026-10-31). `GROWTH_MB` 2 is an upper estimate of a round's net rows (orders
+1800, items +5400, audit +2000, event_log +1000 × 400 B, with indexes); `hours.tsv` `volume_mb` measures it.
`deletedb -d` at a side's end removes its volumes, log and the backups its log lists; the older chains and the
install copies stay in `<out>` for the caller to remove by literal
path.

## Smoke

Two short cycles, the kill block and every end probe, under 10 minutes for the four sides:

```sh
FASTMIG_LONGRUN_CYCLES=2 FASTMIG_LONGRUN_ROUNDS=2 FASTMIG_LONGRUN_CYCLE_SECONDS=90 FASTMIG_LONGRUN_KILL_EVERY=2 \
FASTMIG_LONGRUN_PITR_CYCLE=1 FASTMIG_LONGRUN_STAT_SECONDS=5 FASTMIG_LONGRUN_MIN_ARCHIVES=0 FASTMIG_LONGRUN_MIN_CHECKPOINTS=0 ... just ops-longrun <out>
```

Archives deleted under the probes: `FASTMIG_LONGRUN_PATHS=114-dev FASTMIG_LONGRUN_CYCLES=3 FASTMIG_LONGRUN_ROUNDS=6
FASTMIG_LONGRUN_KILL_EVERY=2 FASTMIG_LONGRUN_PITR_CYCLE=3 FASTMIG_LONGRUN_MAX_ARCHIVES=3` (other settings as above).
The sides create more archives than 3, so the engine deletes some; the PITR (chain-c02 level 0 to cycle 3's mark) and
`latest` read only the archives after their backup and must pass, while `chain0-bktime` and `chain0-next` (the first
chain) abort for want of deleted ones (recorded, with their cores as `range_cores`).
