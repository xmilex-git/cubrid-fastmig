-- old-source-edge-cases, fixture h, as DBA (workspace#554): case 6. A 10.2 official load stops at the first row develop
-- hashes to another partition, so it lives apart from fixture m. Runs unchanged on 10.2, 11.2 and 11.3.
-- 6. HASH partitions keyed on NCHAR and NCHAR VARYING, 1000 keys with a space inside
create table t_digit (c nchar varying(1));
insert into t_digit values (N'0');
insert into t_digit values (N'1');
insert into t_digit values (N'2');
insert into t_digit values (N'3');
insert into t_digit values (N'4');
insert into t_digit values (N'5');
insert into t_digit values (N'6');
insert into t_digit values (N'7');
insert into t_digit values (N'8');
insert into t_digit values (N'9');
create table t_nc (k nchar(16), v int) partition by hash (k) partitions 7;
insert into t_nc select a.c || N' ' || b.c || c.c || N' z', 0 from t_digit a, t_digit b, t_digit c;
create table t_nv (k nchar varying(16), v int) partition by hash (k) partitions 7;
insert into t_nv select N'k ' || a.c || b.c || N' ' || c.c, 0 from t_digit a, t_digit b, t_digit c;
