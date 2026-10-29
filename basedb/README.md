# basedb — baseline database builder

Scripts that build the **baseline databases** (기준 DB, see [GLOSSARY.md](../GLOSSARY.md)): small
databases made by the engine itself, one set per version, used to check on-disk structures
against real pages and to judge migrations. Decision and recorded results:
[T1 기준 DB 세트 (workspace#479)](https://github.com/xmilex-git/workspace/issues/479#issuecomment-6496835580).

The DBs already built on the dev host are the reference; **do not rebuild them to use them**.
A rebuild produces different log LSAs (append_lsa, page LSAs), so every number recorded in T1
and in later tickets stops matching. P1 and every later step start from the pristine copy,
taken with `cp -a --reflink=always <BASEDB_ROOT>/<label>/pristine/<variant> <your dir>`.

A pristine copy still names the **registered** DB: its `_vinf` and every volume header hold absolute paths under
`<BASEDB_ROOT>/<label>/<variant>/`. Never run an engine utility (`addvoldb`, `csql -S`, a server) on such a copy:
the engine follows those paths and writes into the registered DB (workspace#494 did this once with `addvoldb -S`;
the three changed files were restored from pristine). Read a copy with `cubrid-fastmig migrate
--volumes-in-first-dir`, or migrate it first — the target's headers and `_vinf` carry the target's paths.

## Run

```sh
bash basedb/build.sh <label: 11.4|develop> <install> <port> <variant: clean|dirty>
# or
BASEDB_INSTALL=<install> BASEDB_PORT=<port> just basedb <label> <variant>
```

- `<install>`: the CUBRID install to build with. T1 used `/home/cubrid/release/CUBRID-fastmig-11.4.6`
  (11.4.6.1963) and `/home/cubrid/release/CUBRID-fastmig-dev` (develop 682b080b2).
  The script overwrites `<install>/conf/cubrid.conf` on every run.
- `<port>`: a `cubrid_port_id` claimed beforehand; the server is started and stopped through
  the workspace's `cubrid-server-control` wrapper.
- Host paths come from the environment; the defaults are the dev host's:

  | Variable | Default | Used for |
  |---|---|---|
  | `BASEDB_ROOT` | `/home/cubrid/fastmig-basedb` | output root and size check |
  | `TOOLING` | `/home/cubrid/dev/workspace` | the `cubrid-server-control` wrapper |
  | `BASEDB_JAVA_HOME` | `/usr/lib/jvm/java-1.8.0-openjdk` | `javac`/`loadjava` for the Java SP |

- The script refuses to run when any output directory already exists; remove it by literal path first.
- Size cap: `du -sm $BASEDB_ROOT` must stay ≤ 5120 MB (du counts the reflinked pristine copy
  twice). One variant takes about 513 MB registered, 465–473 MB pristine, 17–23 MB unload.

## Output

Under `$BASEDB_ROOT/<label>/` (`CUBRID_DATABASES` points here), DB name `fm114_<variant>` or `fmdev_<variant>`:

| Path | Content |
|---|---|
| `<variant>/` | the registered DB; lob-base-path `file:<this dir>/lob` |
| `pristine/<variant>/` | reflink copy taken right after the final shutdown — the starting point |
| `unload/<variant>/` | `unloaddb` output, the set-comparison reference |
| `evidence/<variant>/` | csql output, `counts.sql` result, `loghdr.txt` (append/chkpt LSA), `vacuum_pending.txt`, `pristine.sha256`, `unload.sha256`, `sizes.txt` |

After the diagnostic SA runs, the registered DB is rsynced back to the pristine bytes.

## Variants

- **clean**: load, full SA vacuum, clean shutdown. No vacuum work is left.
- **dirty**: clean, then `50_dirty.sql` on a server started with `vacuum_disable=yes`, then a
  clean shutdown. Vacuum work is left; any SA boot runs it, so work from the pristine copy.

## Scripts

Run in this order by `build.sh` (user, mode in parentheses):

| File | Makes | From |
|---|---|---|
| `00_users.sql` (dba) | users `app_owner`, `app_group`, `app_reader` (group member), user comments | T1 |
| `10_schema.sql` (app_owner) | tables of every common type, collations, PK/FK/UNIQUE/composite/DESC indexes, AUTO_INCREMENT, RANGE/HASH/LIST partitions, overflow table `doc_big`; function and filter indexes on `g5_expr`, `g5_fx`, `region_list` (`i_f1`, `i_f2`, `i_f4`, `i_f5`) | T1; G5 (workspace#486) |
| `20_data.sql` (app_owner) | rows, all functions of `seq_n.n` so both versions load the same data; LOBs on 20 `product` rows | T1 |
| `30_objects.sql` (app_owner) | serial, views (one WITH CHECK OPTION), triggers, synonym, PL/CSQL function and procedure, GRANTs | T1 |
| `31_dba_objects.sql` (dba) | dba tables (one `utf8_ko_cs`), synonym, Java SP `fm_hello` | T1; G6 (workspace#488) for `java/` |
| `40_post.sql` (app_owner) | record relocations, AUTO_INCREMENT/trigger/serial/SP use | T1 |
| `45_orphan_lob.sql` (app_owner, 11.4 only) | 10 orphan LOB files: 11.4 DROP TABLE leaves them behind | G6 (workspace#488) |
| `46_partition_fx.sql` (app_owner) | partition expressions with operators renumbered after 11.4, CHAR/NUMERIC constants, a HASH table with a NUMERIC column | R7 (workspace#487), T3 (workspace#489) |
| `50_dirty.sql` (app_owner, dirty only) | deletes and updates left for vacuum | T1 |
| `counts.sql` (dba, SA) | one row count per object and catalog counts | T1 |
| `FmHello.java` | class behind the Java SP, compiled to Java 8 and `loadjava`ed | T1 |

Fixtures the engine refuses are absent on purpose: 11.4.6 rejects a UNIQUE function index and
a NUMERIC partition key, so neither exists in an 11.4 source.
