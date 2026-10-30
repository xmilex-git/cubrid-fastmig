-- old-source-edge-cases, fixture s: the develop checks as DBA (workspace#554).
select class_name, attr_name, data_type, domain_class_name from db_attribute
  where class_name in ('t_dom', 't_dom_u1') order by 1, 2;
select id, u.name, s.name from t_dom order by id;
select * from v_dom order by id;
select owner_name, vclass_name, vclass_def from db_vclass where vclass_name in ('v_dom', 'v_dom_u1') order by 1, 2;
insert into t_dom select 3, u, null from _db_user u where u.name = 'U2';
select id, u.name from t_dom order by id;
