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
`INDEX!=HEAP` (harness 26a81fa; runs 15-16). Runs 09-14 used the harness before that review (no failback, assert and
core read by hand).

| Pair | Side | Run | Steps (source pair, migrate, target start, replication, failover, failback) | HA apply info reset | Checks errors | Asserts, cores | Parity |
|---|---|---|---|---|---|---|---|
| 11.4.6 → develop | migrated | run-15 | 46, all exit 0; ends in the first roles | 1 per node | 0 | 0, 0 (checked) | 9/9 same |
| 11.4.6 → develop | official | run-16 | 35, all exit 0; ends in the first roles | — | 0 | 0, 0 (checked) | |
| develop → develop | migrated | run-13 | all exit 0, failback half (old master back as slave) | 1 per node | 0 | 0, 0 (read) | 6/6 same |
| develop → develop | official | run-14 | all exit 0, failback half | — | 0 | 0, 0 (read) | |

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
