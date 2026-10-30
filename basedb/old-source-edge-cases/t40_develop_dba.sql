-- old-source-edge-cases, fixture t: the develop checks as DBA (workspace#554).
select * from v_lit2 order by 1;
select owner_name, vclass_name, vclass_def from db_vclass where vclass_name in ('v_lit2', 'v_trg_u1') order by 1, 2;
select name, action_definition from _db_trigger where name like '%tr_lit2';
insert into t_tl values (1, 'x');
select * from t_tl order by id;
