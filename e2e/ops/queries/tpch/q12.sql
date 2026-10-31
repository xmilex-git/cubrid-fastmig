-- TPC-H Query 12: Shipping Modes and Order Priority
-- T40 (workspace#595): as TPC-H/scale10/queries/q12.sql (its ORDER BY is total, or one row)
select
	l_shipmode,
	sum(case
		when o_orderpriority = '1-URGENT'
			or o_orderpriority = '2-HIGH'
			then 1
		else 0
	end) as high_line_count,
	sum(case
		when o_orderpriority <> '1-URGENT'
			and o_orderpriority <> '2-HIGH'
			then 1
		else 0
	end) as low_line_count
from
	orders,
	lineitem
where
	o_orderkey = l_orderkey
	and l_shipmode in ('MAIL', 'SHIP')
	and l_commitdate < l_receiptdate
	and l_shipdate < l_commitdate
	and l_receiptdate >= date '1994-01-01'
	and l_receiptdate < DATE_ADD(DATE '1994-01-01', INTERVAL 1 YEAR)
group by
	l_shipmode
order by
	l_shipmode;
