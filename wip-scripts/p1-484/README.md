# #484 measurement scripts (4번 세션)

Speed comparison excludes verify (user 2026-10-30): the timed fastmig rows run `migrate` without `--verify`; one
`--verify` run per path is kept as consistency evidence (migration defects 0), with no median.

| Script | Does |
|---|---|
| `build_big_dev.sh` | big develop input: basedb/build.sh develop clean, rows multiplier 180, into `../basedb-big/develop/` |
| `run_same.sh <method> <rep>` | one same-version run of the big develop DB: `cp_a`, `cp_reflink`, `fm_copy`, `fm_clone` (`*_verify` for evidence) |
| `run_114.sh <method> <W> <rep>` | one 11.4 -> develop run of 3번's big 11.4 DB: `fm` (clone), `fm_copy` (`*_verify` for evidence), W 1/4/16 |
| `gen_meas.sh` | writes `meas_same_big.sh` and `meas_114_big.sh` (3 reps, then one `--verify` run; literal `rm -rf` paths) |
| `summarize_same.sh [tsv]` | median of reps per method and stage (for u114.tsv join method and W first) |
| `smoke114_big.sh` | the one big 11.4 -> develop failure-finding smoke (not a measurement) |

Rules shared with 3번's baseline (`../../baseline/scripts/env.sh`): CPUs 16-31 (W=1: 31, 4: 28-31, 16: 16-31) under
taskset, quiet gate (busy > W/2 or nvme0n1 > 50 %util: wait 60 s, at most twice), page cache not dropped and the
source read once before each run, log archives never copied or read. Results: `../results/{same,u114}.tsv`, per-run
`../runs/`.
