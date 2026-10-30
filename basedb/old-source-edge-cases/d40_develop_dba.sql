-- old-source-edge-cases, fixture d: the develop checks as DBA (workspace#554), on a target only the pre-check could
-- have stopped: the copied rows, then a call of each name.
select sp_name, owner from db_stored_procedure where sp_name like '%.%' order by 1;
select "x.y"('a') from db_root;
call "dbms_output.put_line"('a');
