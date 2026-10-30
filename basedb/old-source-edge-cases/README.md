# old-source-edge-cases — the remaining edge cases of a 10.2..11.3 source, on both paths

Small fixtures for the seven edge cases of T26 [workspace#554](https://github.com/xmilex-git/workspace/issues/554).
Each is built on a 10.2, 11.2 or 11.3 source and taken to develop two ways: through cubrid-fastmig (`plan`, `migrate
--verify`, then `post-migration-fixes.sql`) and through the engine's official path (source `unloaddb` → develop
`loaddb -u dba`, with `--no-user-specified-name` from 10.2). The same develop checks run on both targets. It is not a
baseline DB.

## Run

```sh
bash run.sh <fixture: m|h|s|t|d|r> <label: 10.2|11.2|11.3> <source install> <develop install> <cubrid-fastmig> <out dir>
```

Use `/home/cubrid/release/CUBRID-fastmig-{10.2.18,11.2.9,11.3.6}` as the source and the optdebug develop install, so
that the checks also catch develop asserts. Every step runs in SA mode, so no port is taken and no conf is written.
SQL files run in name order, and the owner is the last word of the file name. On the source the script runs
`<f>0*`…`<f>3*`, and on each target `<f>4*`. The fastmig path runs before `unloaddb`, because an `unloaddb` that
crashes leaves the source unclean, and plan refuses an unclean source.

| Fixture | Case | Content |
|---|---|---|
| m | 2, 3, 5, 7 | literals naming system classes in a view and a trigger, SELECT on system classes granted to U1 and U2, an OBJECT column holding a stored procedure row and an argument row, trailing 0x00 in a unique key, a foreign key pair and a plain index |
| h | 6 | HASH partitions on NCHAR(16) and NCHAR VARYING(16), 1000 keys with a space inside |
| s | 1 | columns typed `db_user` and `db_serial` (owned by DBA and by U1) and views reading them through path expressions |
| t | 2, 3 | a DBA view and trigger reading `db_trigger` with literals naming system classes, and a U1 view over `db_trigger` that DBA granted |
| d | 4 | stored procedures named `dbms_output.put_line`, `dbms_output.enable` (U1) and `x.y` |
| r | 1 | columns typed `_db_class` (rows regenerated) and `db_authorizations` (dropped in develop) |

## Results

Measured on 10.2.18.9024, 11.2.9.0866 and 11.3.6.1321 → develop 682b080b2 (optdebug), cubrid-fastmig with
workspace#554. Every fastmig target that plan accepted passed `migrate --verify` with 0 migration defects. No develop
assert fired.

1. **System class domain (s, r).** The official path gives no target. 10.2 → develop `loaddb` stops the whole schema
   with `The domain of u cannot be a vclass`, because develop's `db_user` is a view. The 11.2 and 11.3 `unloaddb` crash
   in `emit_domain_def` (`unload_schema.c:3879@11.3`). fastmig keeps the domain on the same class, which develop names
   `_db_user` or `_db_serial`, and keeps every value. DBA's path expressions, views and inserts work. U1's fail
   (`SELECT is not authorized on _db_user`), so plan warns about each such attribute of an owner outside the DBA
   group. `GRANT SELECT ON _db_user TO u1` on the target makes them work. A domain on a system class whose rows the
   migration moves or drops (fixture r: `_db_class`, `db_authorizations`) is refused, a rule from workspace#482.
2. **System names inside string literals (m, t).** Both paths leave every literal as it is. fastmig rewrites only the
   `FROM db_trigger` of a DBA text to `_db_trigger` (workspace#542). On develop, the view and the trigger return the
   literal unchanged (`'db_trigger tr_lit2'`), and `v_lit` returns the same rows on both paths. The official path
   cannot load fixture t at all (`Class (db_trigger) does not have attribute [name]`).
3. **SELECT on system classes granted to a user (m, t).** Neither path carries these grants: `unloaddb` prints no grant
   on a system class, and fastmig rebuilds the system-class grants from the template. On both targets U1 gets
   `SELECT is not authorized on _db_trigger` / `_db_password`. A U1 view over `db_trigger` is kept with the
   workspace#542 warning. The official path does not load it.
4. **A stored procedure name with a dot (d).** 11.4 and develop refuse such a name, and the develop parser crashes on its
   `CREATE` (`pt_set_sp_owner_to_table_column_types`, `parser_support.c:12026@develop`). So the official `loaddb`
   ends with SIGSEGV from all three sources. plan now refuses each such name (P3). Before this change the rows were
   copied. On develop `"x.y"(…)` then failed with `Function x.y is undefined`, and `call "dbms_output.put_line"(…)` ran
   develop's builtin `DBMS_OUTPUT.PUT_LINE` in place of the user's procedure.
5. **OBJECT pointing at a stored procedure row (m).** `unloaddb` prints a reference to a system class row as NULL, so
   the official path loses it. fastmig keeps it: same-slot rows keep their OIDs, so on develop it names the migrated
   `_db_stored_procedure` (`dba.put_line2`) and `_db_stored_procedure_args` rows. This is listed in
   [intended differences](../../docs/intended-differences.md).
6. **NCHAR/NCHAR VARYING HASH partitions from 10.2 (h).** 10.2 weighs a space inside the key as 0, as it does for
   VARCHAR (R14). All 1000 keys of each table sit apart from where develop hashes them. Pruning finds 0 rows for
   `k = N'1 23 z'`. The 10.2 official `loaddb` stops at the first such row (`Invalid value for partition
   definition`). plan now warns with the moved-row count (1000 and 1000), and `post-migration-fixes.sql` re-partitions
   both tables. After the fix, every key is in develop's partition, and the same lookup finds its row. 11.2 and 11.3 sit
   as develop hashes them, so both paths give the same placement (diff 0).
7. **Trailing 0x00 (m).** 10.2 compares two strings as equal once trailing 0x00 and spaces are cut. develop counts
   them, but weighs a space as 0x00 inside the common length, so `'c '` = `'c\0'` while `'c'` < `'c\0'`. Both targets
   return the same rows. They differ only in the order of keys develop calls equal. Both keep the two foreign key rows
   (`'a'` → `'a\0'`, `'b\0'` → `'b'`) that no longer find a parent. fastmig warns about them, as workspace#541 does for
   trailing spaces. One key record of a rebuilt index holds two such values, and the verifier counted that as a
   migration defect (`key-value-differs`), so migrate refused to register the 10.2 target. The verifier now accepts a
   leaf key whose image is that of one of its record's rows.
