# tc — ported testcases

Runs CUBRID shell testcases that go through `unloaddb` → `loaddb` with the migrated database in their
place (**포팅 TC, ported testcase**, see [GLOSSARY.md](../GLOSSARY.md)). Each testcase runs unchanged.
Where it first calls `unloaddb` (the **switch point**), every database it registered is put through
`cubrid-fastmig migrate`, `verify` and `set-comparison`. Then the testcase goes on against the migrated
databases with its own expected answers. Decision and results: [workspace#502](https://github.com/xmilex-git/workspace/issues/502).

This is a runtime check, not a unit test (AGENTS.md § Build and check).

## Private testcases stay private

The testcases live in a private repository (`cubrid-testcases-private-ex`). This repository holds only
the harness: no testcase body, expected answer, name or path. The list of testcases to run is a
local file passed as an argument.

## Run

```sh
just release
FASTMIG_TC_CTP=<cubrid-testtools>/CTP FASTMIG_TC_SERVER_CTL=<workspace>/.agents/skills/cubrid-server-control/scripts/cubrid-server-ctl.sh \
  just tc <tc-root> <tc-list> <install> <out dir>
```

- `<tc-root>`: a `cubrid-testcases-private-ex` checkout, read only.
- `<tc-list>`: one testcase directory per line, relative to `<tc-root>` (e.g. `shell/<dir>/<case>`). Text
  after a tab is a note. Lines starting with `#` are skipped.
- `<install>`: the CUBRID install. It is the source and the target engine (same-version path).
- `<out dir>`: a new directory. It holds one job per testcase and mode, plus `summary.tsv`.
- env: `FASTMIG_TC_MODES` (default `0 1`: a baseline run of the testcase as is, then the run through the
  switch point), `FASTMIG_TC_TIMEOUT` (seconds per job, default 720 as CTP shell), `FASTMIG_TC_MAX_GB`
  (stop when the out dir grows past it, default 20).
- Give `run.sh` a copy of the binary that no build touches (e.g. `<scratch>/bin/<sha>/cubrid-fastmig`), not
  `target/release`: every job records the sha256 of the binary it ran (`bin=` in the verdict), and a run whose
  jobs ran different binaries ends `RESULT: FAILED` (`mixed binaries`).
- On a host shared with measurements, pin the run to the CPUs left to it (`taskset -c 0-15 bash tc/run.sh ...`);
  the namespace jobs inherit the affinity.

## Files

| File | Job |
|---|---|
| `run.sh` | Loops over the list. Lists the host's `cub_server` pids before and after. Prints the verdict table. |
| `run_case.sh` | One testcase in its own namespace job. Writes the verdict line. |
| `shim/init.sh` | The `init.sh` the testcase sources: CTP's own, plus kill guards and the `cubrid`/`cub_admin` overrides. |
| `shim/switch_point.sh` | The switch point. |

## Isolation

CTP's `init` and `finish` run `pkill cub`. Each job therefore runs in user, mount, ipc, pid and net
namespaces (`unshare -Urmipnf`), the way workspace `tools/fast-gate/fast-cs-csql.sh` does, so a kill
or a port reaches nothing outside the job. The install is mounted as a volatile overlay on its own
path, with `data_buffer_size=512M parallelism=4 max_parallel_workers=100` appended (the CTP runner's
pin). Two more guards sit on top: the shim makes `pkill`, `killall`, `xkill` and `xkill_pid` log
instead of kill, and `server start|stop` goes through the cubrid-server-control wrapper.
`CUBRID_DATABASES` is the symlink `<job>/dbs`. The switch point flips it from `db-source` to
`db-target`, so every later process sees the target.

## Verdict

One line per job in `summary.tsv`. A job PASSes when all of these hold:

- The testcase's `<case>.result` has at least one OK and no NOK.
- No engine core was written during the job (`cores.txt`).
- The server error logs have no `Internal error`.
- The job did not time out.
- Switch runs only: at least one database was switched, and every step in `switch.tsv` exited 0. The
  steps are source unload, migrate, verify, deferred SQL, target unload and set comparison.

`verify_codes` counts the migration defects by code. Use it to read a FAIL caused by verify.

Reading a FAIL: a baseline NOK is the testcase's environment or answer, not the tool. A switch FAIL
with a passing baseline is a tool defect until shown otherwise; on the same-version path the target
must behave like the source.

A clean job deletes its databases with `cubrid deletedb`. A failed one keeps them, with a failed
source renamed `<db>_src`. The out dir is removed by its literal path once read.
