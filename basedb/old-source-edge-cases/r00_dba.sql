-- old-source-edge-cases, fixture r, as DBA (workspace#554): case 1 on system classes whose rows the migration
-- moves (`_db_class`, regenerated) or drops (`db_authorizations`, gone in develop); plan refuses both.
create table t_cls (id int primary key, c _db_class);
insert into t_cls select 1, c from _db_class c where c.class_name = 't_cls';
create table t_auths (id int primary key, a db_authorizations);
insert into t_auths select 1, a from db_authorizations a;
