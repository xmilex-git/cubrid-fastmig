# owner-qualified-names-102 — the engine's official path for 10.2 text that names other owners' objects

Small 10.2 fixtures where views and triggers name tables, serials and Java SPs of other owners, taken through the
engine's official path (10.2 `unloaddb` → develop `loaddb -u dba --no-user-specified-name`, the manual's way for
sources before 11.2). The dumps under `expected/` record the owner-qualified names and the texts the engine stores,
and how those views and triggers behave on develop. They are what the text qualification of G8
[workspace#517](https://github.com/xmilex-git/workspace/issues/517) is compared with. Measured in T6
[workspace#516](https://github.com/xmilex-git/workspace/issues/516) on 10.2.18.9024 → develop 682b080b2 (release builds).

## Run

```sh
bash run.sh <fixture: a|b|c> <10.2 install copy> <develop install copy> <out dir>
```

Use reflink copies of `/home/cubrid/release/CUBRID-fastmig-{10.2.18,dev}` as the installs. The script writes their
`conf/cubrid.conf`. Every step runs in SA mode, so the script takes no port. It creates its own DBs under
`<out dir>/<fixture>/db`, and deletes each one when its step is done.
SQL files are run in name order, and the owner is the last word of the file name. On 10.2 the script runs
`<f>0*`…`<f>3*`. On develop, after the load, it runs `<f>4*`.

| Fixture | Content |
|---|---|
| a | u1's views and triggers over u2's table, AUTO_INCREMENT table, serial, Java SP and a partitioned table; one DBA table; u1's partitioned table |
| b | a without the partitioned tables owned by non-DBA users (the develop load stops at them, below); one partitioned table owned by DBA |
| c | trigger text using serials of three owners: its own (u1), another user (u2), DBA |

## Results (`expected/`)

1. **Partitioned table owned by a non-DBA user: the load fails.** `ALTER CLASS [t] PARTITION BY …` stops schema loading
   with `Unknown class "dba.t"` and rolls the whole schema back (`partition-load-failures.txt`). This is fixture a,
   and also the T4 baseline DBs `fm102_clean` and `fm102_dirty` at line 133 (`event_log`, owner APP_OWNER).
   Source of the error: `do_redistribute_partitions_data` (execute_schema.c:5843@develop) compiles
   `UPDATE [dba.t] …`. The class lock hint then goes to the server's `xlocator_find_lockhint_class_oids`
   (locator_sr.c:11301), and that function has no lookup for `--no-user-specified-name`. The lookup exists only in the
   client's `locator_find_class_with_purpose`. A partitioned table owned by DBA loads (fixture b, `pexpr` =
   `SELECT  year([d]) FROM [dba.tdp]`).
2. **View specs: every class and SP reference becomes `[owner.name]`.** This holds for the other owner's table
   (`[u2.t2]`), DBA's table (`[dba.td]`), the view's own owner (`[u1.t1]`), a view over a view (`[u1.v1_t2]`) and an
   SP call (`[u2.sp2]`, wrapped in `cast(… as varchar)`). No `dba.` prefix is left (`b-develop-engine-load.txt`).
3. **Trigger texts: class and SP references become `[owner.name]`, serial references become `[dba.name]` whatever
   their owner.**
   - Class and SP references in conditions and actions resolve to their real owner (`[u2.t2_log]`, `[dba.td]`,
     `[u2.sp2]`). Subqueries get printed out in full (`select distinct [u2.t2].[a] from [u2.t2] [u2.t2]`).
   - Every `serial_next_value`/`serial_current_value` argument is stored as `[dba.<serial>]`: the trigger owner's
     own serial `s1`, u2's `s2` and DBA's `sd` alike (`c-develop-engine-load.txt`). Only a DBA-owned serial ends up
     correct.
   - The `CAST(... AS VARCHAR(20))` around a serial reference is dropped.
4. **On develop as the original owners** (`b-develop-checks.txt`, `c-develop-checks.txt`):
   - Views over tables of u1, u2 and DBA, the view over a view, and the view over DBA's partitioned table read the
     same rows as on 10.2.
   - A view or trigger that calls another owner's SP fails with `EXECUTE authorization failure`. Before 11.4 any
     user can call any SP, and 10.2 has no `GRANT EXECUTE`, so the official path produces no grant. After
     `GRANT EXECUTE ON PROCEDURE sp2 TO u1`, both work.
   - Every trigger that uses a serial not owned by DBA fails when it fires (`Serial 'dba.s2' not defined`,
     `Serial 'dba.s1' not defined`), so every INSERT on its table fails. A grant does not fix this, because the stored
     text names an object that does not exist.

The texts of the source (10.2) are in `<f>-10.2-source.txt`. For example, the trigger action is stored as
`serial_next_value([s2], 1)` without an owner. The 10.2 runs of the same checks are in `<f>-10.2-check-u1.txt`.
