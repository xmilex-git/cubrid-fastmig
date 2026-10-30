-- old-source-edge-cases, fixture d, as DBA (workspace#554): stored procedure names with a dot.
-- 4. one named like a builtin of develop's DBMS_OUTPUT package, one not
create procedure "dbms_output.put_line"(s varchar) as language java
  name 'FmHello.hello(java.lang.String) return java.lang.String';
create function "x.y"(s varchar) return varchar as language java
  name 'FmHello.hello(java.lang.String) return java.lang.String';
create table t_plain (id int primary key);
insert into t_plain values (1);
