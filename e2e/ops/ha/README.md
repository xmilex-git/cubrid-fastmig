# e2e/ops/ha — HA check of migrated databases

Shows that a master DB and a slave DB, each moved to develop by cubrid-fastmig on its own, form a working develop HA
pair: replication, failover and fail-back. The tool does not move an HA pair as one (ADR 0003: no HA transition);
this is the operator's procedure checked end to end. Decision: G12 [workspace#587](https://github.com/xmilex-git/workspace/issues/587) (4);
ticket: T38 [workspace#590](https://github.com/xmilex-git/workspace/issues/590). Runtime check, not a unit test.

## Two nodes on one host

CUBRID HA names its nodes by host name (`ha_node_list=group@a:b`, compared with `hostname` on each node) and runs one
`cub_master` per host on `ha_port_id`, so two nodes cannot share a host name or a network namespace; two port pairs on
this host do not make a pair. The nodes are two rootless podman containers of the cubridci image on a private network
(`pair.sh`), the way the workspace ctp-run skill runs `ha_shell`:

| | |
|---|---|
| host names | `fmmaster` (heartbeat started first, so master), `fmslave`; fixed addresses `.11`/`.12` of the network's subnet, given to both nodes with `--add-host` |
| namespaces | own network and IPC per node: default ports and SHM ids, no port claim, nothing collides with host servers |
| paths | the run dir is bind-mounted at its host path: `_vinf`, `databases.txt` and volume headers are valid in the node and on the host, where cubrid-fastmig reads the stopped DB |
| user | CUBRID runs as the container's root = this user on the host (rootless) |
| cores | `/data/core` bound at its path: a node's core lands with the host's (core_pattern is host-global) |
| installs | each node gets reflink copies of the source and target installs; `ha_conf.sh` overwrites their `cubrid.conf`/`cubrid_ha.conf` |
| `CUBRID_TMP` | `/var/tmp/cubrid` inside the node: the default `$CUBRID/tmp` under the run dir is 88 characters, and the PL server socket then exceeds `sun_path` (108): the PL server, so `cub_server`, does not start (run-01, 2026-10-31) |
| LOB base | source pair: each node keeps the LOB base `safe_copy.sh` wrote (inside that node's DB copy; it writes no LOB). Target pair: the master keeps the one migrate wrote and the slave takes it (§ Procedure, `ha_lob_base`). Never the registered baseline's: the scenario's LOB deletes remove files (T36) |

## Run

```sh
FASTMIG_HA_SOURCE_INSTALL=~/release/CUBRID-11.4.6.1963 FASTMIG_HA_TARGET_INSTALL=~/optdebug/CUBRID-fastmig-dev \
FASTMIG_HA_DML="<ops>/dml/round.sql <ops>/dml/round_synonym.sql <ops>/dml/round_plcsql.sql" FASTMIG_HA_CHECKS=<ops>/checks.sql \
FASTMIG_HA_SIDE=migrated \
  taskset -c 0-15 bash e2e/ops/ha/run.sh target/release/cubrid-fastmig ~/fastmig-basedb/11.4/pristine/clean \
  <workspace>/.git_ignored_dir/scratch/590/run-NN
# the same with FASTMIG_HA_SIDE=official into run-MM, then
bash e2e/ops/ha/parity.sh <...>/run-NN <...>/run-MM
```

`<ops>` is T36's `e2e/ops` (workspace#588, bce276e or later): the DML round files of the source label (synonyms from
11.2, PL/CSQL from 11.4) and `checks.sql`, run with `-N` like T36. One side per run keeps at most three DB copies.

The script deletes nothing: remove `run-NN` by its literal path afterwards (it holds two installs and up to three DBs
of the baseline size).

## Procedure (run.sh)

1. **Source pair.** Both nodes start from the same pristine (`basedb/safe_copy.sh`: the same `db_creation`, as a slave
   restored from the master's backup has). HA conf per the manual's quick setup; heartbeat on the master, then on the
   slave, and only then the modes (`cubrid changemode`: `active` / `standby`): a master stays `to-be-active` until the
   slave joins (run-02). A replicated marker table `fm590_sync` (primary key) gives the sync point: a token inserted on
   the master is visible on the slave. The source pair writes only the marker: T36's `round.sql` and `checks.sql`
   are written for the target line (run-03). Then the slave's `db_ha_apply_info` rows, and heartbeat stop: slave,
   then master (normal shutdown on both).
2. **Migrate.** On the host, one node at a time: `plan`, `migrate --verify --volumes-in-first-dir`, the deferred SQL
   in the node (SA), `finalize`. Then the two targets' `db_creation` (from `fastmig/source-fingerprint`) must be equal
   (G4 keeps it from the source).
3. **Target pair.** HA conf on the target installs, `databases.txt` db-host `fmmaster:fmslave`; heartbeat start,
   modes, the slave's `db_ha_apply_info` at start, sync, DML rounds, sync, checks.
4. **Failover.** Heartbeat stop on the master; the slave turns `active`; DML there; heartbeat start on the old master,
   which comes back `standby`; sync from the new master; checks.
5. **Evidence.** Heartbeat status, assert hits and ERROR counts in the log files this run wrote, cores newer than the
   run. Expected ERROR lines: HA mode changes (-970), the disconnects of a heartbeat stop (-989, -4).

`check/<phase>.diff` is master against slave. CUBRID HA synchronizes only tables with a primary key (cubrid-manual
`ko/ha.rst` "테이블 기본키(primary key)"), so the diff need not be empty: on T1 it lists `session_hash`,
`region_list` and the `part_*` tables of basedb on both sides. The pass rule is parity: the same diffs and the same
checks as the official pair.

The target pair shares one LOB storage: the slave's `databases.txt` takes the master's lob-base-path (`ha_lob_base`).
CUBRID HA replicates LOB locators, not LOB files, and allows no LOB work on a slave or after failover when the storage
is local (cubrid-manual `ko/ha.rst` "LOB 칼럼 메타 데이터(Locator)는 복제되고, LOB 데이터는 복제되지 않는다");
with a local storage the slave's `checks.sql` read "External file ... was not found" (run-08).

`FASTMIG_HA_SAME_COPY_LOG_PATH=1` puts the target pair's copied logs where the source pair's were (`ha_copy_log_base`
= the source `CUBRID_DATABASES`; the old copied logs are moved aside): the operator who keeps `CUBRID_DATABASES`.

## HA apply info is not carried

`db_ha_apply_info` (`_db_ha_apply_info` in develop) holds each applier's position in its master's log, found by
(`db_name`, `copied_log_path`) and trusted when its `db_creation_time` equals the copied log header's
(`log_applier.c la_get_last_ha_applied_info`). G4 keeps `db_creation`, and the target log starts at a later page
(`copy::new_log_start`), so a carried row sends the applier into a log chain the target lacks. Runs on T1 clean, 11.4.6 →
develop (run-04..06) and develop → develop (run-07), 2026-10-31:

| Run | Tool | Copied-log path | Result |
|---|---|---|---|
| run-04 | carried the row | new | row not matched; applier starts at the copied log's end; replication, failover, fail-back pass |
| run-05 | carried the row | the source pair's | applier starts at required LSA 9205\|15704; "cannot find archive log for 9205 page", -81; slave heartbeat start fails |
| run-06 | empties the table | the source pair's | plan "1 rows reset" on both nodes; applier starts in the new log (9265\|16240, 9266\|368); replication, failover, fail-back pass; no assert, no core |
| run-07 (develop → develop) | empties the table | the source pair's | same-version writer: "1 rows reset" on both nodes; applier starts in the new log (9235\|9032, 9236\|688); replication, failover, fail-back pass; no assert, no core |

So migrate empties the table on both paths (`catalog::ha_apply_info`, ADR 0002 amendment), the start `restoreslave`
gives a rebuilt slave. Both nodes have rows: the master's applier of the slave's log writes one too.

## Other facts of the pair

- `CUBRID_TMP` is `/var/tmp/cubrid` inside the node: the default `$CUBRID/tmp` under the run dir is 88 characters, the
  PL server socket under it exceeds `sun_path` (108; the engine caps `CUBRID_TMP` at 96), and the PL server, so
  `cub_server`, fails to start ("PL server can not be started", -113; run-01). With the short path it starts (run-02).
- The source pair's copied logs (`<CUBRID_DATABASES>/<db>_<host>`) stay in the source `CUBRID_DATABASES`.

## Official pair (FASTMIG_HA_SIDE=official, official_pair.sh)

The manual's HA upgrade (cubrid-manual `upgrade.rst` "HA 환경에서 DB 마이그레이션", C1-C9), in place of steps 1 and 2:
on the master `createdb` with the source's volume and log sizes, `loadjava` of the baseline's Java class, `loaddb` of
the baseline's unload (as T36's official side), `backupdb -S`; on the slave `restoredb -u` of that backup into the
slave's `databases.txt` paths (both nodes see the whole run dir, so a plain restore would write the master's files).
The marker table is created once the pair runs. The source's LOB files are copied to each node's `/fm590-private`
(`pair.sh`): one absolute path for the locators, a different directory per node, as on two hosts. Steps 3-5 are the
migrated side's. `parity.sh` compares every `check/` file of the two runs; pass = all same.
