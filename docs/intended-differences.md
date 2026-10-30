# Intended differences

Target states that differ from the engine's official upgrade path (source `unloaddb` → target `loaddb`, R12
[workspace#521](https://github.com/xmilex-git/workspace/issues/521)) and that the map decided to keep
([GLOSSARY](../GLOSSARY.md) "의도된 차이"). Each row is a state the target engine can produce itself and keeps
information the official path drops. When a set comparison shows one, report it from this list; never add a
normalization rule that hides it. Measured on 11.3.6 / 11.2.9 → develop 682b080b2 in T15
[workspace#538](https://github.com/xmilex-git/workspace/issues/538).

| What | Official path | fastmig | Sources | Why kept | Decision |
|---|---|---|---|---|---|
| grantor of a grant given with grant option by a third user | the class owner (loaddb runs every GRANT as DBA, `au_compare_grantor_and_return`) | the third user | 10.2..11.3 | the grant chain survives; the third user can still REVOKE what it granted, and owner/DBA REVOKE works as on a fresh 11.4 (DBA- and owner-group grantors do become the owner) | (b), R12 [workspace#521](https://github.com/xmilex-git/workspace/issues/521); code `catalog/src/rows_before_11_4.rs` |
| `_db_serial.cached_num` of an AUTO_INCREMENT serial | `auto_increment_cache_size` (default 20, CBRD-26983 #7411: develop stores it when the column's serial is created) | the source value (0 for every serial made before develop) | 10.2..11.4 | an engine state (`auto_increment_cache_size=0`); 0 keeps the source's behavior of no skipped values after a restart; the only cost is a catalog write per generated value, as on the source | (a), T15 [workspace#538](https://github.com/xmilex-git/workspace/issues/538) |
| serial in a trigger condition or action of a source before user schema (`serial_next_value([s], 1)`) | `[dba.s]` whatever the serial's owner; the trigger fails on firing (`Serial 'dba.s1' not defined`, workspace#547) | `[owner.s]`, the serial's real owner; the trigger fires as on the source | 10.2 (11.0) | the official path stores a name that does not exist; the real owner is what the source text resolved to (one serial namespace before 11.2) | G8 correction [workspace#517](https://github.com/xmilex-git/workspace/issues/517); measured on the T6 fixture c, workspace#544; code `catalog/src/owner_qualified_texts.rs` |
| print shape of a view or trigger text of a source before user schema | re-printed by the develop parser (`select distinct`, `[u2.t2] [u2.t2]` aliases, the insert column list spelled out, `cast(.. as varchar)` around a procedure call, a `cast` around a serial dropped) | the source text with only its names qualified (`[u2.t2] [t2]` where the text qualifies by the bare name) | 10.2 (11.0) | same names resolved, same meaning; the tool does not re-print SQL | G8 correction [workspace#517](https://github.com/xmilex-git/workspace/issues/517), workspace#544 |
| stored procedure call in a view or trigger text of an 11.2 or 11.3 source (`[sp2]([u2.t2].[b])`) | the load fails: develop `loaddb` resolves the unqualified call as `dba.sp2` and stops the schema at the first such view ("Function sp2 is undefined", 11.3.6 → develop) | `[u2.sp2]`, the procedure's real owner, as the 10.2 official path stores it | 11.2, 11.3 | before 11.4 a procedure name had one namespace, so the real owner is what the source text resolved to | G8 [workspace#517](https://github.com/xmilex-git/workspace/issues/517); code `catalog/src/owner_qualification.rs` |
| `REUSE_OID` flag of a user table of a 10.2 source | set: 10.2 `unloaddb` prints `REUSE_OID` only for a reuse class and nothing otherwise, so develop `loaddb` applies its default `create_table_reuseoid=yes` | the source flag (0 for a table 10.2 made without OID reuse) | 10.2 | placement preservation keeps every OID and OBJECT reference (ADR 0002), so the class keeps the OID behavior it had on the source; the user can still `ALTER TABLE .. REUSE_OID` | workspace#549; code `catalog/src/class_rewrite.rs` (flags copied) |
| password object of a user a 10.2 source made without a password | a `_db_password` row with a NULL password (`CREATE USER` in the unloaded schema) | none: `_db_user.password` stays NULL as on the source | 10.2 | develop `au_set_password_internal` creates the object when the user first gets a password; login without a password works either way | workspace#549; code `catalog/src/same_slot_rows.rs` (same-slot rows kept) |

Not listed here because a decision fixes them as rules, not as kept source state: `_db_serial.start_val` NULL for a
started serial (workspace#482 decision 8; the official path writes the declared start), and new `created_time` /
`updated_time` values (the migrate start, in local time like the engine, workspace#543; the official path writes the
load time). Rows the source already stamps keep their times on a develop → develop run.

Not a difference from the official path either, but a change every 10.2 source sees: a view or trigger text on
`db_trig` (rewritten to develop `db_trigger`, workspace#542) owned by a user without SELECT on a trigger's target class
returns that trigger's row on develop; the 10.2 `db_trig` hid it, 11.4 `db_trig` and develop `db_trigger` show it
(measured on 10.2.18 / 11.4.6 / develop, workspace#539). Users with rights, the trigger owner and DBA see the same rows.
