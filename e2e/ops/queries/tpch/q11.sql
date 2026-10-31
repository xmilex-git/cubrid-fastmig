-- TPC-H Query 11: Important Stock Identification
-- T40 (workspace#595): ORDER BY made total with ps_partkey; else as q11.sql
select
	ps_partkey,
	sum(ps_supplycost * ps_availqty) as [value]
from
	partsupp,
	supplier,
	nation
where
	ps_suppkey = s_suppkey
	and s_nationkey = n_nationkey
	and n_name = 'GERMANY'
group by
	ps_partkey having
		sum(ps_supplycost * ps_availqty) > (
			select
				sum(ps_supplycost * ps_availqty) * 0.0000100000
			from
				partsupp,
				supplier,
				nation
			where
				ps_suppkey = s_suppkey
				and s_nationkey = n_nationkey
				and n_name = 'GERMANY'
		)
order by
	[value] desc,
	ps_partkey;
