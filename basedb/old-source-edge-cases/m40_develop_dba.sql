-- old-source-edge-cases, fixture m: the develop checks as DBA, run after either path (workspace#554).
-- 2
select * from v_lit order by 1;
select owner_name, vclass_name, vclass_def from db_vclass where vclass_name in ('v_lit') order by 1, 2;
select name, condition, action_definition from _db_trigger where name like '%tr_lit';
insert into t_lit select max(id) + 1, 'db_password' from t_lit;
select * from t_lit order by id;
-- 3
select grantor_name, grantee_name, object_name, auth_type from db_auth
  where grantee_name in ('U1', 'U2') order by 2, 3, 4;
-- 5
select t.id, s.unique_name from t_sp t, _db_stored_procedure s where t.p = s order by 1;
select t.id, a.arg_name from t_sp t, _db_stored_procedure_args a where t.p = a order by 1;
select id, p is null from t_sp order by 1;
-- 7
select id, hex(v) from t_nul order by v;
select count(*) from t_nul where v = 'a';
select count(*) from t_nul where v = 'a' + chr(0);
select id, hex(v) from t_nulc order by id;
select c.id from t_nulc c where not exists (select 1 from t_nulp p where p.v = c.v) order by 1;
select hex(v) from t_nulp order by v;
select /*+ recompile */ id, hex(v) from t_nuli where v >= 'c' and v < 'd' order by v;
;plan simple
select /*+ recompile */ id from t_nuli where v = 'c' + chr(0);
