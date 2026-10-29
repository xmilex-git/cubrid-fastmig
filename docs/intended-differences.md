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

Not listed here because a decision fixes them as rules, not as kept source state: `_db_serial.start_val` NULL for a
started serial (workspace#482 decision 8; the official path writes the declared start), and new `created_time` /
`updated_time` values (the migrate start, in local time like the engine, workspace#543; the official path writes the
load time). Rows the source already stamps keep their times on a develop → develop run.
