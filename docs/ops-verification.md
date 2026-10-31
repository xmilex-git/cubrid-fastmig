# Operations verification

What runs on migrated databases besides `verify` and the set comparison: the operations scenario (GLOSSARY) and its
parity with the official path. Decision: G12 [workspace#587](https://github.com/xmilex-git/workspace/issues/587).
The version matrix (T37 [workspace#589](https://github.com/xmilex-git/workspace/issues/589)) adds its section here.

## HA

T38 [workspace#590](https://github.com/xmilex-git/workspace/issues/590), G12 (4): the tool does not move an HA pair as
one; master and slave are migrated one by one and the target pair is configured on the target line. Procedure, harness
and the facts behind them: [e2e/ops/ha/README.md](../e2e/ops/ha/README.md).

Runs of 2026-10-31: T1 clean baselines, target CUBRID 11.5.0.2781-682b080 optdebug, two podman nodes, one DML round of
T36 (`e2e/ops/dml/round.sql`, `round_synonym.sql`, `round_plcsql.sql` at bce276e) on the master, `checks.sql` on both
nodes after the target pair's replication, after failover and after failback; the target pair copies logs where the
source pair did (`FASTMIG_HA_SAME_COPY_LOG_PATH=1`). Migrated: cubrid-fastmig with the HA apply info reset (1b53a47).
Official: the manual's HA upgrade (`loaddb`, `backupdb`, `restoredb -u` on the slave, whose `_vinf` names only its own
directory). run.sh fails a step on an assert in this run's logs or step outputs, on a core of either node and on
`INDEX!=HEAP` (harness 398ec54; runs 15-18, runs 17-18 with the tool at 762c7fa). Runs 09-14 used the harness before
that review (no failback, assert and core read by hand); runs 17-18 replace runs 13-14 in the table.

| Pair | Side | Run | Steps (source pair, migrate, target start, replication, failover, failback) | HA apply info reset | Checks errors | Asserts, cores | Parity |
|---|---|---|---|---|---|---|---|
| 11.4.6 → develop | migrated | run-15 | 46, all exit 0; ends in the first roles | 1 per node | 0 | 0, 0 (checked) | 9/9 same |
| 11.4.6 → develop | official | run-16 | 35, all exit 0; ends in the first roles | — | 0 | 0, 0 (checked) | |
| develop → develop | migrated | run-17 | 46, all exit 0; ends in the first roles | 1 per node | 0 | 0, 0 (checked) | 9/9 same |
| develop → develop | official | run-18 | 35, all exit 0; ends in the first roles | — | 0 | 0, 0 (checked) | |

Master and slave differ on both sides alike (`check/target.diff`, `check/failover.diff`, `check/failback.diff`
equal between the sides): only tables without a primary key (`session_hash`, `region_list`, `part_*` of basedb).[^pk]
The target pair shares one LOB storage (the slave takes the master's lob-base-path).[^lob]
Before the reset, a carried `_db_ha_apply_info` row stopped the slave's applier (run-05, ADR 0002 amendment).

Not run: an 11.4 target (out of G12's HA scope), a node with two or more apply-info rows, the 4.2 GB DB, more than one
DML round.

[^pk]: CUBRID HA replicates by primary key and synchronizes only tables that have one (cubrid-manual `ko/ha.rst`
    "테이블 기본키(primary key)": "기본키가 설정된 테이블에 대해서만 CUBRID HA 그룹 내의 노드 간 데이터 동기화가
    가능하다"). Not a migration defect: the official pair differs the same way.
[^lob]: CUBRID HA replicates LOB locators, not LOB files (cubrid-manual `ko/ha.rst`: "LOB 칼럼 메타 데이터(Locator)는
    복제되고, LOB 데이터는 복제되지 않는다"); with a local LOB storage the slave read "External file ... was not found"
    (run-08).

## Application queries

T40 [workspace#595](https://github.com/xmilex-git/workspace/issues/595): the read-only application query sets of
[e2e/ops/queries/](../e2e/ops/queries/README.md) on the migrated side and on the official side (unloaddb → loaddb), the
outputs compared byte for byte. Target engines: optdebug develop 682b080b2 and optdebug 11.4.6 (each run on its own
reflink copy). Pass: every query the same bytes on both sides, no error line, assert or core on either side; the
`-1066 Session expired` lines are counted apart (`session_expired`, pre-existing engine defect
[#597](https://github.com/xmilex-git/workspace/issues/597)).

| Path | Set | Queries | Same | Errors / asserts / cores (migrated, official) | session_expired | cubrid-fastmig | Verdict |
|---|---|---|---|---|---|---|---|
| 11.4 → develop | TPC-H SF1 sample | 22 | 22 | 0/0/0, 0/0/0 | 1, 1 | 77525c0 | pass |
| develop → develop | TPC-H SF1 sample | 22 | 22 | 0/0/0, 0/0/0 | 1, 1 | 77525c0 | pass |
| 11.2 → develop | basedb clean | 29 | 29 | 0/0/0, 0/0/0 | 1, 1 | e347f48 | pass |
| 11.3 → develop | basedb clean | 29 | 29 | 0/0/0, 0/0/0 | 1, 1 | e347f48 | pass |
| 11.4 → develop | basedb clean | 30 | 30 | 0/0/0, 0/0/0 | 1, 1 | e347f48 | pass |
| develop → develop | basedb clean | 30 | 30 | 0/0/0, 0/0/0 | 1, 1 | e347f48 | pass |
| 11.2 → 11.4 | basedb clean | 29 | 29 | 0/0/0, 0/0/0 | 5, 1 | 77525c0 | pass |
| 11.3 → 11.4 | basedb clean | 29 | 29 | 0/0/0, 0/0/0 | 1, 1 | 77525c0 | pass |
| 11.4 → 11.4 | basedb clean | 30 | 30 | 0/0/0, 0/0/0 | 1, 5 | 77525c0 | pass |
| 10.2 → develop | basedb clean | 28 | 28 | 0/0/0, 0/0/0 | 1, 1 | b37616e | pass |
| 10.2 → 11.4 | basedb clean | 28 | 28 | 0/0/0, 0/0/0 | 1, 1 | b37616e | pass |

Queries per source: 28 for 10.2 (no synonyms, no PL/CSQL), 29 for 11.2 and 11.3 (no PL/CSQL), 30 for 11.4 and develop.
The 11.4-target rows ran before #599's fix, which touches develop targets only. Plans are recorded per query
(`<side>/plan/`), not compared.

- **10.2 sources: the official path.** It needs the workaround of [#516](https://github.com/xmilex-git/workspace/issues/516)
  (without it loaddb stops at the first partitioned table). Its first form (main 8708e0f: every `change_owner` moved
  to the end) resolved the views' queries while their tables still belonged to dba (`[dba.customer]`), and the official
  side failed queries 10, 11 and 27 (25/28 the same; judged by the source, the migrated side was right). The narrowed
  form (main b37616e, `e2e/ops/official_102_schema.awk`: only the partitioned tables' `change_owner` moves) gives the
  rows above.
- **Migration defect found: [#599](https://github.com/xmilex-git/workspace/issues/599).** On the first runs (cubrid-fastmig
  8bbcd53), every 10.2, 11.2, 11.3 and 11.4 → develop row asserted on the migrated side at query 10
  (`qdata_check_output_column`, cores `core.transaction.{3635142,3641792,3653875}`): a migrated view kept the source's
  `numeric(38,2)` column with a spec that has no cast, a state develop never builds. Fixed by c4baf42 and 73764c6
  (`recompile-views.sql`); the develop rows above are the rerun.
- **TPC-H source DB.** No `dbgen` on the host: a key-consistent sample of the SF10 load files (`partkey ≤ 300,000`,
  `orderkey ≤ 6,000,000`; rule and procedure in e2e/ops/queries/README.md § TPC-H DB). Rows: orders 1,500,000, lineitem
  899,197, partsupp 1,200,000, part 300,000, customer 1,500,000, supplier 100,000; the 8 FOREIGN KEYs build. Volumes:
  1920 MB built with 11.4.6, 1408 MB with develop. Official side's prepare (loaddb -S) 297 s and 290 s, migrated 60 s and 48 s.
