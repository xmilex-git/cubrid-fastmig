-- old-source-edge-cases, fixture m, as DBA (workspace#554): cases 2, 3, 5, 7. Runs unchanged on 10.2, 11.2
-- and 11.3.

-- 2. system names inside string literals of a view spec and a trigger text
create table t_lit (id int primary key, cname varchar(64));
insert into t_lit values (1, 'db_trigger');
insert into t_lit values (2, 'db_user');
insert into t_lit values (3, 'db_serial');
insert into t_lit values (4, 't_lit');
create view v_lit as select class_name from db_class
  where class_name = 'db_trigger' or class_name in ('db_user', 'db_serial', 'db_password');
create trigger tr_lit after insert on t_lit if obj.cname = 'db_password'
  execute insert into t_lit values (obj.id + 100, 'from db_trigger, db_user');

-- 3. SELECT on system classes granted to ordinary users
grant select on db_password to u1;
grant select on db_trigger to u1;
grant select on db_authorizations to u2;

-- 5. an OBJECT column holding a stored procedure row and an argument row
create function put_line2(s varchar) return varchar as language java
  name 'FmHello.hello(java.lang.String) return java.lang.String';
create table t_sp (id int primary key, p object);
insert into t_sp select 1, p from _db_stored_procedure p where p.sp_name = 'put_line2';
insert into t_sp select 2, a from _db_stored_procedure_args a where a.sp_name = 'put_line2';

-- 7. trailing 0x00 bytes: a unique key, a foreign key pair, a plain index
create table t_nul (id int primary key, v varchar(20) unique);
insert into t_nul values (1, 'a' + chr(0));
insert into t_nul values (2, 'b');
insert into t_nul values (3, 'c' + chr(0) + chr(0));
create table t_nulp (v varchar(20) primary key);  -- 10.2: a foreign key references a primary key only
insert into t_nulp values ('a' + chr(0));
insert into t_nulp values ('b');
create table t_nulc (id int, v varchar(20), foreign key (v) references t_nulp (v));
insert into t_nulc values (1, 'a');
insert into t_nulc values (2, 'b' + chr(0));
insert into t_nulc values (3, 'b');
create table t_nuli (id int primary key, v varchar(20));
create index i_t_nuli_v on t_nuli (v);
insert into t_nuli values (1, 'c');
insert into t_nuli values (2, 'c' + chr(0));
insert into t_nuli values (3, 'c ');
insert into t_nuli values (4, 'd');
insert into t_nuli values (5, 'c' + chr(0) + 'x');
