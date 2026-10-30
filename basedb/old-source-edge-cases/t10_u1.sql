-- old-source-edge-cases, fixture t, as U1 (workspace#554).
-- 3. a view of an ordinary user over a system class DBA granted it SELECT on
create view v_trg_u1 as select name from db_trigger;
select * from v_trg_u1;
