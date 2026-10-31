-- TPC-H Query 4: Order Priority Checking
-- T40 (workspace#595): as TPC-H/scale10/queries/q4.sql (its ORDER BY is total, or one row)
select
	o_orderpriority,
	count(*) as order_count
from
	orders
where
	o_orderdate >= date '1993-07-01'
	and o_orderdate < DATE_ADD(DATE '1993-07-01', INTERVAL 3 MONTH)
	and exists (
		select
			*
		from
			lineitem
		where
			l_orderkey = o_orderkey
			and l_commitdate < l_receiptdate
	)
group by
	o_orderpriority
order by
	o_orderpriority;
