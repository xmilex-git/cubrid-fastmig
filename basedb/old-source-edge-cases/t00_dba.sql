-- old-source-edge-cases, fixture t, as DBA (workspace#554): view texts over a system class the official path cannot
-- load on develop (develop's db_trigger is a view without these columns), so they live apart from fixture m.
-- 2. system names in string literals of a view whose FROM names a system class the tool rewrites
create view v_lit2 as select name, 'from db_trigger where db_user' as s from db_trigger;
create table t_tl (id int primary key, cname varchar(64));
create trigger tr_lit2 after insert on t_tl if obj.id < 100
  execute insert into t_tl values (obj.id + 100, (select 'db_trigger ' + name from db_trigger where name = 'tr_lit2'));
