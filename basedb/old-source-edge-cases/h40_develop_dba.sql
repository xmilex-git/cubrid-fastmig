-- old-source-edge-cases, fixture h: the develop checks as DBA (workspace#554).
-- 6: rows per partition, and keys pruning does not find in their partition
select 'nc', 0, count(*) from t_nc__p__p0 union all select 'nc', 1, count(*) from t_nc__p__p1 union all
  select 'nc', 2, count(*) from t_nc__p__p2 union all select 'nc', 3, count(*) from t_nc__p__p3 union all
  select 'nc', 4, count(*) from t_nc__p__p4 union all select 'nc', 5, count(*) from t_nc__p__p5 union all
  select 'nc', 6, count(*) from t_nc__p__p6;
select 'nv', 0, count(*) from t_nv__p__p0 union all select 'nv', 1, count(*) from t_nv__p__p1 union all
  select 'nv', 2, count(*) from t_nv__p__p2 union all select 'nv', 3, count(*) from t_nv__p__p3 union all
  select 'nv', 4, count(*) from t_nv__p__p4 union all select 'nv', 5, count(*) from t_nv__p__p5 union all
  select 'nv', 6, count(*) from t_nv__p__p6;
select count(*) from t_nc where k = N'1 23 z';
select count(*) from t_nv where k = N'k 12 3';
-- 6: every key with its partition
select 'nc', 0, k from t_nc__p__p0 union all select 'nc', 1, k from t_nc__p__p1 union all
  select 'nc', 2, k from t_nc__p__p2 union all select 'nc', 3, k from t_nc__p__p3 union all
  select 'nc', 4, k from t_nc__p__p4 union all select 'nc', 5, k from t_nc__p__p5 union all
  select 'nc', 6, k from t_nc__p__p6 order by 3;
select 'nv', 0, k from t_nv__p__p0 union all select 'nv', 1, k from t_nv__p__p1 union all
  select 'nv', 2, k from t_nv__p__p2 union all select 'nv', 3, k from t_nv__p__p3 union all
  select 'nv', 4, k from t_nv__p__p4 union all select 'nv', 5, k from t_nv__p__p5 union all
  select 'nv', 6, k from t_nv__p__p6 order by 3;
