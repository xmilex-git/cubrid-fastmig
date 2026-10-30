# Catalog templates

Data files for catalog regeneration ([ADR 0011](../../../docs/adr/0011-catalog-template-is-build-time-engine-output.md),
decision [workspace#482](https://github.com/xmilex-git/workspace/issues/482) 2): the system class objects and static
catalog rows of a database the target engine created with `createdb`, one file per target line and charset.
`template_store.rs` compiles them into the binary.

| File | Target | Locale given to `createdb` |
|---|---|---|
| `develop-iso88591.tpl` | develop | `en_US.iso88591` |
| `develop-euckr.tpl` | develop | `ko_KR.euckr` |
| `develop-utf8.tpl` | develop | `en_US.utf8` |
| `11.4-iso88591.tpl` | 11.4 | `en_US.iso88591` |
| `11.4-euckr.tpl` | 11.4 | `ko_KR.euckr` |
| `11.4-utf8.tpl` | 11.4 | `en_US.utf8` |

Each file says in its `meta` lines which install and engine release made it. The current files:
develop: `/home/cubrid/release/CUBRID-fastmig-dev`, `CUBRID 11.5.0 (11.5.0.2781-682b080)` (develop 682b080b2, the T1 install).
11.4: `/home/cubrid/release/CUBRID-fastmig-11.4.6`, `CUBRID 11.4.6 (11.4.6.1963-0e7d3c1)` (v11.4.6.1963, the T1 install; workspace#533).

## Rebuild

```sh
bash crates/catalog/template/build.sh <work dir>     # TEMPLATE_INSTALL=<install> to use another engine
TEMPLATE_INSTALL=/home/cubrid/release/CUBRID-fastmig-11.4.6 TEMPLATE_LINE=11.4 bash crates/catalog/template/build.sh <work dir>
```

`<work dir>` must not exist (put it under the workspace repo's `.git_ignored_dir/scratch/`); it holds the fresh
databases and can be removed afterwards. The script runs `just run template-extract`.

## Content

The format is described in `src/template.rs`. Per develop file: 74 `class` lines (30 catalog tables, 25 system views,
19 `INFORMATION_SCHEMA` views) and the rows of `_db_data_type`, `dual`, `_db_user`, `_db_password`,
`_db_authorization`, `_db_auth`, `db_root`, `_db_charset`, `_db_collation`, `_db_stored_procedure` and
`_db_stored_procedure_args` (`ROW_CLASSES` in `src/template_extract.rs`). Per 11.4 file: 49 `class` lines (29 catalog
tables, 20 system views; 11.4 has no `INFORMATION_SCHEMA`) and 95 rows of the same tables under their 11.4 names (`db_user`, `db_password`,
`db_authorization`).

Two builds from the same install differ only in the `created_time`/`updated_time` values of the `_db_user` and
`_db_auth` rows; regeneration replaces those with the migration start time (workspace#482 decision 8).
