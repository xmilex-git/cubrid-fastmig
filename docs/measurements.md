# Speed on the big databases: cubrid-fastmig against the official path and `cp` (workspace#559)

Two comparisons on the biggest databases this project built (about 22 million rows, 49 indexes; volumes without
log archives 4,289 MB for 11.4 and 4,417 MB for develop), each tool under its fastest settings. Wall time is what
the map's acceptance criteria use.

- Binary: cubrid-fastmig `60a96c6` release (sha256 `f0a94a67040e18c7…`).
- Host: Xeon Silver 4216, XFS on NVMe, `cp` from coreutils 8.30. Every run is pinned to CPUs 16-31.
- cubrid-fastmig runs `migrate --workers 16 --mem 16G --volumes-in-first-dir`, without `--verify`. The clone
  default is reported as *clone*; `--no-clone`, where every volume is byte-copied, is reported as *copy*.
- The source is a fresh reflink copy of the database without log archives. One cold run reads it first. Then
  3 warm runs per method go in turn per round, and the source is read once before each warm run. Cells are
  medians of the 3 warm runs.
- Each run waits until the 1-minute load average is at most 4. All 20 runs ran on 2026-10-31 15:57–16:19 KST,
  with exit code 0. Every run started at a 1-minute load of at most 4.19, and no other engine or build ran on
  CPUs 16-31 at any start. Load sampling (every 10 s) saw one process above 50 % on CPUs 0-15 once (68 %). A
  load spike to 37.8 (16:07) fell inside a load gate wait, not inside a run.

## (A) 11.4 → develop

| Path | Wall (s) |
|---|---|
| Official path: 11.4 unloaddb → develop loaddb, CS mode, `--no-logging-index`, 16 workers / parallelism / index build parallelism ¹ | 148.9 |
| cubrid-fastmig clone | **17.4** |
| cubrid-fastmig copy | 19.5 |
| cubrid-fastmig clone, cold source | 21.0 |

¹ Cited from the stage-1 baseline ([workspace#484](https://github.com/xmilex-git/workspace/issues/484)), which
used the same CPU budget, with a warm source DB and a 6.0 s unload. When the same settings were measured again for
this ticket, the unload took 21.6 s, so the cited value favors the official path.

cubrid-fastmig stages (clone, median s): precheck 3.39, volumes and clone unshare 0.95, page treatment 1.95,
index build 10.05, fsync 1.06. For copy, the volumes stage varied from 0.81 to 7.47 s (median 2.63) over the
3 runs.

## (B) develop → develop

| Path | Wall (s) | Copy stage (s) |
|---|---|---|
| `cp -a` (byte copy) + `sync` of every copied file | 3.19 | 3.19 (cp 1.35 + sync 1.84) |
| `cp -a --reflink=always` | 0.013 | 0.013 |
| cubrid-fastmig clone | 13.2 | 0.67 (volumes and clone unshare 0.03 + fsync 0.64) |
| cubrid-fastmig copy | 15.1 | 2.74 (volumes + fsync) |
| cubrid-fastmig clone, cold source | 13.6 | — |

The two columns give different answers:

- **Wall:** cubrid-fastmig (clone 13.2 s, copy 15.1 s) is slower than `cp -a` + `sync` (3.19 s). Index build
  takes about 10 s of it (10.2 s for clone), and precheck takes 2.2 s.
- **Copy stage only:** cubrid-fastmig copy (2.74 s) is faster than `cp -a` + `sync` (3.19 s). cubrid-fastmig clone
  (0.67 s) is slower than `cp --reflink` (0.013 s).

The same-version path rebuilds every index (ADR 0005). Its heap patch removes the vacuum residue that the target
vacuum can never reach. With the patch and the old B-trees kept, the verifier found leaf entries without their
records on a T1 develop source
([workspace#501](https://github.com/xmilex-git/workspace/issues/501),
[#503](https://github.com/xmilex-git/workspace/issues/503)).

Per-run logs, `report.json` and load samples are kept in the workspace scratch of workspace#559.
