# e2e/ops-matrix — the operations scenario over every supported version pair

Runs the operations scenario runner ([e2e/ops/README.md](../ops/README.md)) on every row of the version matrix and
builds the pass table [docs/ops-verification.md](../../docs/ops-verification.md). Matrix and pass rule: G11
[workspace#587](https://github.com/xmilex-git/workspace/issues/587) decision 2; this driver: T37
[workspace#589](https://github.com/xmilex-git/workspace/issues/589). A runtime check, not a unit test (AGENTS.md).

## Rows

[paths.tsv](paths.tsv), one row per runner run: the nine supported pairs — 10.2, 11.2, 11.3, 11.4 → develop;
10.2, 11.2, 11.3 → 11.4; 11.4 → 11.4; develop → develop — on the small T1 baseline DBs, `clean` and `dirty` each
(18 rows), plus 11.4 → develop on the 4.2 GB DB once (`114-dev-big`, one round, no archive minimum).

| Column | Meaning |
|---|---|
| `id` | `<source>-<target>-<variant>`, the run directory name |
| `label`, `variant` | the baseline: `<root>/<label>/pristine/<variant>` (basedb/README.md) |
| `target` | `--target-version`; also the lane |
| `size` | `small`: root `FASTMIG_OPS_MATRIX_BASEDB_ROOT`; `big`: root `FASTMIG_OPS_MATRIX_BIG_ROOT` |
| `lob_base` | `FASTMIG_OPS_LOB_BASE` under the root, for a 10.2..11.3 source (its locators are absolute under the base it was built in, e2e/README.md); read only |
| `rounds`, `min_archives` | `[rounds]` and `FASTMIG_OPS_MIN_ARCHIVES`; `-` = the runner's default (6 rounds, 2 archives: e2e/ops/README.md § Rounds) |

The source unload the official side loads is the runner's default (`<root>/<label>/unload/<variant>/<db>`), except
for the big row, which keeps no unload: [big_unload.sh](big_unload.sh) makes one with the 11.4 release engine
(the same `unloaddb -S -u dba` as basedb/build.sh) and the lane passes it as `FASTMIG_OPS_SOURCE_UNLOAD`.

## Lanes

A lane runs its rows one after the other on one install and one port; separate lanes may run at once, each with its
own install copy (the runner overwrites `conf/cubrid.conf` and reads the install's `log/`) and its own port.

| Lane | Rows | Install (the lane's own reflink copy) |
|---|---|---|
| `develop` | the 10 small rows → develop | optdebug develop (`~/optdebug/CUBRID-fastmig-dev` on the dev host) |
| `11.4` | the 8 small rows → 11.4 | optdebug 11.4.6 (`~/optdebug/CUBRID-fastmig-11.4.6`) |
| `big` | `114-dev-big` | optdebug develop |

The lane refuses a release-build install, an install of the wrong line (G11 decision 6: the target engine has its
asserts on) and a shared install under `~/optdebug` or `~/release` (the runner overwrites its conf). Lanes in parallel
do not share a PL server socket: the runner keeps `CUBRID_TMP` per port.

Rerun rule: a failed row is rerun once, and only when its `server-log-summary.tsv` shows `pl_drops` > 0 (the PL
server drop, e2e/ops/README.md § Known flakes); the rerun takes the next `run-NN`, and the table shows the earlier
run with its exit code and PL drops next to the verdict. Any other failure is a finding, never rerun away.

```sh
# the big lane only: its official side's unload, once
bash e2e/ops-matrix/big_unload.sh <BIG_ROOT>/11.4/pristine/clean <work dir> <own copy of the 11.4 release install>
# then remove <work dir>/copy by its literal path

FASTMIG_OPS_INSTALL=<own optdebug copy> FASTMIG_OPS_PORT=<claimed port> \
FASTMIG_OPS_SERVER_CTL=<workspace>/.agents/skills/cubrid-server-control/scripts/cubrid-server-ctl.sh \
[FASTMIG_OPS_MATRIX_BASEDB_ROOT=~/fastmig-basedb] \
[FASTMIG_OPS_MATRIX_BIG_ROOT=<BIG_ROOT> FASTMIG_OPS_MATRIX_BIG_UNLOAD=<work dir>/unload/fm114_clean] \
  taskset -c 0-15 just ops-matrix <develop|11.4|big> <out root> [id...]

just ops-matrix-summary <out root> > docs/ops-verification.md
```

`[id...]` reruns only those rows; each run takes the next free `<out root>/<id>/run-NN`, and the summary reads the
highest one per row. `FASTMIG_OPS_MATRIX_DRY_RUN=1` prints each row's runner command and checks its pristine, unload and
LOB base exist, without writing or running anything (the binary need not be built yet).

## Output

```
<out root>/<id>/run-NN/              the runner's out dir (e2e/ops/README.md § Output)
<out root>/<id>/run-NN.console       the runner's stdout and stderr
<out root>/<id>/run-NN.rc            its exit code
<out root>/lane-<lane>.tsv           id  run  rc  seconds, one row per run
<out root>/lane-<lane>-install.txt   the install, its cubrid_rel, libcubrid sha256, binary sha256
```

[summarize.sh](summarize.sh) turns these into docs/ops-verification.md: per row the failed / all step and check
rows, the differing / all parity steps, the server log counts of both sides (errors, asserts, cores and PL server drops, the runner's known flake), the archives created and the verdict
(the runner's exit code). A failing row gets a defect ticket, recorded in [defects.tsv](defects.tsv) (`id`,
ticket) and shown in the table.

## Disk

The runner keeps one side's DB at a time plus, for a row with a LOB base, the source copy it leaves in
`<id>/run-NN/migrated/source` (remove it by its literal path after the row). Two lanes at once therefore hold up to
four small copies (about 0.5 GB each), an exception to the two-copy rule approved by the map supervisor
(2026-10-31, workspace#589). The big lane runs alone. `big_unload.sh` adds one 12 GB reflink copy (removed
once the unload exists) and the unload itself. Run directories hold text only; remove them by literal path once the
table is committed.
