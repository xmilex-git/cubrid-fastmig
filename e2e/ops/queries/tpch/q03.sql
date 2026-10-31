-- TPC-H Query 3: Shipping Priority
-- T40 (workspace#595): ORDER BY made total with l_orderkey (LIMIT); else as q3.sql
select
	l_orderkey,
	sum(l_extendedprice * (1 - l_discount)) as revenue,
	TO_CHAR(o_orderdate, 'YYYY-MM-DD') as o_orderdate,
	o_shippriority
from
	customer,
	orders,
	lineitem
where
	c_mktsegment = 'BUILDING'
	and c_custkey = o_custkey
	and l_orderkey = o_orderkey
	and o_orderdate < date '1995-03-15'
	and l_shipdate > date '1995-03-15'
group by
	l_orderkey,
	o_orderdate,
	o_shippriority
order by
	revenue desc,
	o_orderdate,
	l_orderkey
LIMIT 10;
