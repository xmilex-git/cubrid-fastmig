-- old-source-edge-cases, fixture s, as DBA (workspace#554). Runs unchanged on 10.2, 11.2 and 11.3.
-- 1. columns whose domain is a system class, read through path expressions
create serial s_dba;
create table t_dom (id int primary key, u db_user, s db_serial);
insert into t_dom select 1, u, (select s from db_serial s where s.name = 's_dba') from db_user u where u.name = 'U1';
insert into t_dom select 2, u, null from db_user u where u.name = 'DBA';
create view v_dom as select id, u.name as uname, s.name as sname from t_dom;
