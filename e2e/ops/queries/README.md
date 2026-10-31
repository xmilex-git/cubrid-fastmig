# e2e/ops/queries — application query parity

Takes one source DB to the target version two ways — through cubrid-fastmig (side **migrated**) and through the
official path, source `unloaddb` → target `loaddb` (side **official**) — runs the same read-only **application
queries** (애플리케이션 질의, [GLOSSARY.md](../../../GLOSSARY.md)) on each side and compares each query's output byte for
byte. The operations scenario ([../README.md](../README.md)) compares row counts, CRC sums and catalog counts; this
adds joins, aggregates, subqueries, window functions, string/date/JSON functions, partition pruning and other users'
privileges. Ticket: T40 [workspace#595](https://github.com/xmilex-git/workspace/issues/595). A runtime check, not a unit
test (AGENTS.md § Build and check).

Pass: every query exits without an error on both sides, every query's output is the same bytes on both sides, and the
server error logs of both sides hold no `ERROR ***` line, no assert and no new core. The engine's `-1066 Session expired` lines
(session.c) are counted apart as `session_expired` and do not fail the run: a pre-existing engine defect, one line per
csql session that ran a join with a GROUP BY sort, seen on a source never migrated with 11.4.6 and develop alike
([#597](https://github.com/xmilex-git/workspace/issues/597); ../sides.sh `side_faults`). A query whose output differs is
judged by running it on the source DB with the source engine (a safe copy of the pristine, basedb/safe_copy.sh): the
side that matches the source is right, the other side gets a defect ticket.

## Run

```sh
FASTMIG_OPS_INSTALL=<own copy of the optdebug target install> FASTMIG_OPS_PORT=<claimed port> \
FASTMIG_OPS_SERVER_CTL=<workspace>/.agents/skills/cubrid-server-control/scripts/cubrid-server-ctl.sh \
  taskset -c 0-15 just ops-queries <root>/<label>/pristine/<variant> <develop|11.4> <tpch|basedb> <out dir>
# same as: bash e2e/ops/queries/run.sh target/release/cubrid-fastmig <pristine> <target version> <set> <out dir>

# the basedb set over every clean row of the version matrix with that target (e2e/ops-matrix/paths.tsv)
  taskset -c 0-15 just ops-queries-paths ~/fastmig-basedb <develop|11.4> <out root>
```

Arguments, environment, the two sides' preparation and the install rules are those of the operations scenario runner
([../README.md](../README.md) § Run, `prepare` row of § Sides and steps): both use [../sides.sh](../sides.sh). The
install is optdebug (G12 6), the caller's own copy. `FASTMIG_OPS_LOB_BASE` is required for a 10.2..11.3 source;
`run_paths.sh` sets it from the matrix row.

Paths (T40): the TPC-H DB 11.4 → develop and develop → develop; the basedb set on the nine supported pairs, `clean`
variant — 10.2, 11.2, 11.3, 11.4 → develop and develop → develop (lane `develop`), 10.2, 11.2, 11.3, 11.4 → 11.4
(lane `11.4`).

## Steps of a side

`prepare` (as the scenario runner) → `server-start` → per query `q-<name>` and `plan-<name>` → `server-stop` →
`deletedb` (`deletedb -d`). `migrated` runs first, then `official`; one DB lives at a time.

- `q-<name>`: `csql -C -N -t -i <file>` as the file's user, output into `<side>/result/<name>.out`. Fails on a csql exit
  or an `ERROR` line.
- `plan-<name>`: the same file after `SET OPTIMIZATION LEVEL 258;` (256 = print the simple plan, the one `;plan simple`
  prints; 2 = do not execute) into `<side>/plan/<name>.out`. Recorded, never compared and never failing: the migrated
  side's indexes are rebuilt and its statistics updated after the move, so a different plan is not a defect (T40).

## Query files

One file per query (some files hold two or three short statements on one subject), one subject per file:

| Line | Meaning |
|---|---|
| `-- user: <name> [<password>]` | run as that user; none = `dba` |
| `-- min-source: <label>` | the first source version that has the objects the file reads (`11.2`: synonyms, `11.4`: PL/CSQL); the file is skipped for older sources |

Rules every file keeps, so equal results print equal bytes:

- **Total order**: every result of more than one row has an ORDER BY that orders all its rows (the keys of the GROUP
  BY, or a unique column added after the query's own keys). The TPC-H files say where they add one.
- **No DOUBLE aggregate printed raw**: `AVG` returns DOUBLE and a DOUBLE sum depends on the order the rows are added,
  which a parallel scan over a different heap layout changes. The files round it (`ROUND(AVG(x), 6)`) or compute it in
  NUMERIC (`CAST(SUM(x) / COUNT(*) AS NUMERIC(20,4))`). NUMERIC sums are exact.
- **Read only**: no file writes (TPC-H q15's view is a WITH clause), so the order of the files does not matter and
  both sides read the state the move left.
- **No intended difference** (docs/intended-differences.md): no current serial value, no `cached_num` of an
  AUTO_INCREMENT serial, no grantor, no view or trigger text.

### tpch/ (22)

The 22 TPC-H queries of `/home/cubrid/dev/TPC-H/scale10/queries/` (CUBRID syntax, `qgen -d` parameters) with the
changes their second line names: ORDER BY made total in q3, q5, q10, q11, q18, q20; `AVG` rounded in q1; q15 as one
statement with a WITH clause.

### basedb/ (30)

Over the baseline DB (basedb/README.md), every source version:

| Files | Covers |
|---|---|
| 01–03 | joins, GROUP BY on an expression, HAVING, ORDER BY an aggregate with LIMIT |
| 04–05 | analytic functions: RANK, ROW_NUMBER per partition, running SUM, LAG, LEAD |
| 06–07 | NOT EXISTS, correlated and IN subqueries, recursive WITH, UNION ALL, outer join |
| 08–09 | string functions; DATE, DATETIME, TIMESTAMPTZ, DATETIMELTZ functions |
| 10–11 | the two views (the aggregating view, the WITH CHECK OPTION view) |
| 12 | the rows the AFTER INSERT trigger wrote |
| 13–16 | RANGE, HASH, LIST partition pruning; partitions keyed on expressions |
| 17 | the serial: definition and the values it generated |
| 18–19 | JSON functions, JSON_TABLE |
| 20 | BLOB and CLOB lengths and digests |
| 21–23 | `utf8_en_ci` and `utf8_ko_cs` sort and compare; NCHAR, BIT and SET values |
| 24 | the Java SP per row and in a predicate |
| 25 | the PL/CSQL function (`min-source: 11.4`) |
| 26 | the synonyms (`min-source: 11.2`) |
| 27–29 | `app_reader` through its own grants, through its group, and the catalog it sees |
| 30 | function and filter index predicates (the deferred indexes of the migrated side), overflow records |

## Output

```
<out>/meta.txt                    arguments, binary sha256, install, query count, load and free disk before/after
<out>/steps.tsv                   side  step  seconds  rc   (side: migrated | official | parity)
<out>/queries.tsv                 query  lines_migrated  lines_official  same (yes | NO)
<out>/<side>/result/<name>.out    the query's output
<out>/<side>/plan/<name>.out      its simple plan (not compared)
<out>/<side>/log/<step>.log       the step's own output
<out>/<side>/server-log/          the install's log files written while the side ran
<out>/parity/<name>.diff          only for a query whose outputs differ
<out>/server-log-summary.tsv      side  error_lines  assert_lines  cores  pl_drops  session_expired
```

`run_paths.sh` adds `<out root>/paths.tsv` (`id`, exit code, queries, queries the same).

## TPC-H DB

There is no `dbgen` on the dev host, so the SF1-scale DB is a key-consistent sample of the SF10 load files
(`/home/cubrid/dev/TPC-H/scale10/load_data`, md5s in `/home/cubrid/databases/tpch/README.md`). Rule (approved by the
map supervisor 2026-10-31, workspace#595):

| Table | Rows kept | Why |
|---|---|---|
| `part`, `partsupp` | `partkey ≤ 400,000` (20 %) | `partsupp` follows its part |
| `orders` | `orderkey ≤ 10,000,000` (1/6 of SF10's keys 1..60,000,000) | `o_orderdate` does not follow the key, so every date range keeps rows |
| `lineitem` | `orderkey ≤ 10,000,000` and `partkey ≤ 400,000` | every line keeps its order and its `partsupp` row (its supplier is one of its part's four) |
| `supplier`, `customer`, `nation`, `region` | all | every order and `partsupp` row keeps its parent |

The eight FOREIGN KEYs of `tpch-db/indexes.sql` are created after the data, so a row that breaks the sample's key
consistency fails the build. Estimated size: load files 572 MB fixed (customer 260, partsupp 244, part 53, supplier 15)
+ orders 316 MB (2.5 M rows) + lineitem 291 MB (about 2.0 M rows) = 1.18 GB; the SF10 DB's volumes are 1.57 × its
load files (19.5 GB / 12.4 GB, keys included), so about 1.85 GB of volumes. The build fails above 2048 MB.

Build, once per source label (11.4.6 for the 11.4 → develop path, develop for develop → develop), each with its own
reflink copy of the release install and a claimed port:

```sh
cp -a --reflink=always /home/cubrid/release/CUBRID-fastmig-11.4.6 <own install copy>
FASTMIG_OPS_SERVER_CTL=<workspace>/.agents/skills/cubrid-server-control/scripts/cubrid-server-ctl.sh TPCH_PORT=<claimed> \
  taskset -c 0-15 just tpch-db 11.4 <own install copy> <root>
# then the develop label with a copy of /home/cubrid/release/CUBRID-fastmig-dev; <root>/sample/ is reused
```

[tpch-db/build.sh](tpch-db/build.sh): the sample into `<root>/sample/` (once per root) → `createdb` (1 GB volume,
256 MB log, the server adds 256 MB volumes, `log_max_archives=0`) → server start → CS `loaddb` of
[tpch-db/schema.sql](tpch-db/schema.sql) (tables without keys), each table's data (`--no-statistics`),
[tpch-db/indexes.sql](tpch-db/indexes.sql) (8 PRIMARY then 8 FOREIGN KEYs) → `UPDATE STATISTICS ... WITH FULLSCAN` →
row counts → CS `unloaddb` into `unload/sf1/` → stop → `vacuumdb -S` → reflink `pristine/sf1/` → volume size check. The
two `.sql` files are byte copies of `/home/cubrid/databases/tpch/load/{schema_nopk,index_all}`. The DB is `tpch114_sf1`
or `tpchdev_sf1` under `<root>/<label>/` in the baseline layout, so `run.sh` takes `<root>/<label>/pristine/sf1`.

The TPC-H DBs, their copies and `<root>/sample/` are removed by literal path when T40's runs are read.
