# e2e — end-to-end run of a baseline DB

One command takes a baseline DB (basedb/README.md) through every step of a migration and the checks of
[workspace#484](https://github.com/xmilex-git/workspace/issues/484), timing each step (**끝-끝 실행, end-to-end run**,
see [GLOSSARY.md](../GLOSSARY.md)). This is a runtime check, not a unit test (AGENTS.md § Build and check). Verdicts
come from the tool's own exit codes (`verify`, `set-comparison`, `finalize`) and the engine's.

## Run

```sh
FASTMIG_E2E_INSTALL=<own install copy> FASTMIG_E2E_PORT=<claimed port> \
FASTMIG_E2E_SERVER_CTL=<workspace>/.agents/skills/cubrid-server-control/scripts/cubrid-server-ctl.sh \
  just e2e <BASEDB_ROOT>/<label>/pristine/<variant> develop <out dir>
```

- `<install>`: the target-line install (develop). Use your own copy (`cp -a --reflink=always`): the run overwrites
  its `conf/cubrid.conf` (port, `data_buffer_size=512M`, `stored_procedure=yes`).
- Target version: `develop`, or `11.4` with the 11.4.6 install from an 11.4 source (workspace#530) or a 10.2, 11.2
  or 11.3 one (workspace#552). An 11.2/11.3 source is compared with its own unload under the pair's rules; a 10.2
  source has no rules, so `FASTMIG_E2E_SOURCE_UNLOAD` names the official path's 11.4 unload (10.2 unloaddb → 11.4
  loaddb `--no-user-specified-name` → unloaddb).
- Optional env: `FASTMIG_E2E_WORKERS` (worker limit, default 4), `FASTMIG_E2E_CPUS` (taskset CPU list for migrate and
  verify only; also turns on the quiet gate. To keep the engine processes — csql, servers, unloaddb — on a CPU set
  too, start the whole runner under `taskset -c <cpus> bash e2e/run.sh ...`: they inherit it),
  `FASTMIG_E2E_CHECKDB=1` (engine checkdb, #484 (5): the small 11.4 -> develop
  target only), `FASTMIG_E2E_BREAKER` (the #484 (6) probe binary), `FASTMIG_E2E_SOURCE_UNLOAD` (unloaddb prefix of the
  source, default `<BASEDB_ROOT>/<label>/unload/<variant>/<db>`), `FASTMIG_E2E_LOB_BASE` (see below).
- A 10.2..11.3 baseline's LOB locators are absolute paths under the base it was built in
  (`<BASEDB_ROOT>/<label>/<variant>/lob`). Give that base as `FASTMIG_E2E_LOB_BASE` so the run takes the in-place
  path (locators under the base, files copied into develop's per-attribute directories, `lob-after-boot`); the run
  only reads that directory, `lob-base-before`/`lob-base-unchanged` compare its file list and sha256, and finalize is
  recorded `skipped` (it would delete that base's files). Not covered: a source whose directory was moved after its
  LOBs were written — its locators lie outside the base, so migrate leaves them and their files where they are (G6
  workspace#488), a place develop's boot does not clean.

## Steps

Each step is one row `step seconds rc` in `<out>/steps.tsv`, its output in `<out>/log/<step>.log`.

| Step | What | #484 |
|---|---|---|
| `copy` | reflink copy of the pristine to `<out>/source`; never opened by the engine (its headers name the registered DB) | |
| `plan` | `plan --json` → `<out>/plan.json`; a refusal ends the run | (9) |
| `migrate` | `migrate --volumes-in-first-dir`; migrate's own phases go to `<out>/phases.tsv` | (4) |
| `verify-target`, `verify-source` | the target against the source, the source alone | (2) |
| `sql-*` | `deferred-indexes.sql`, `recompile-plcsql.sql`, `recompile-views.sql`, `update-statistics.sql` with csql -S; an ERROR in the output fails the step (csql -i exits 0 after a failed statement) | (1), (8), (10) |
| `target-unload`, `set-comparison` | engine unloaddb of the target against the source's engine unload | (1) |
| `server-start` … `server-stop` | CS server: `smoke.sql` (index-scan SELECTs), `lob.sql` (LOB contents) | (10) |
| `smoke-plans` | every smoke SELECT errored nowhere and planned an index scan | |
| `lob-contents` | the engine reads every source LOB back unchanged | (8) |
| `lob-after-boot` | every manifest LOB file is still in `<target>/lob` after the engine booted and stopped | (8) |
| `checkdb` | engine checkdb -S (optional) | (5) |
| `finalize` | deletes the source copy; run only when every path of the source fingerprint is in `<out>/source` | (8) |
| `break-*`, `verify-broken-*` | breaks the finalized target cumulatively; verify must exit 1 after each | (6), (8) |
| `target-deletedb` | engine deletedb of the registered target | |
| `lob-base-before`, `lob-base-unchanged` | with `FASTMIG_E2E_LOB_BASE`: that base's file list and sha256 before the run and the same after it | (8) |

Not covered: (3) the cp comparison and median-of-3 (run the command 3 times under `FASTMIG_E2E_CPUS`, as the
unload→load baseline), (7) the lock smoke and partition pruning.

## Disk

At most two DB copies live at once: the source copy and the target. A registered target is removed with
`cubrid deletedb`. What is left (a failed target, the source copy's kept files, logs) stays in `<out>`, removed by its
literal path once read.
