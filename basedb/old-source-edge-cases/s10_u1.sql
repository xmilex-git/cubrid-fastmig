-- old-source-edge-cases, fixture s, as U1 (workspace#554).
-- 1. a system class domain in a table of an ordinary user
create table t_dom_u1 (id int primary key, u db_user);
insert into t_dom_u1 select 1, u from db_user u where u.name = 'U1';
create view v_dom_u1 as select id, u.name as uname from t_dom_u1;
