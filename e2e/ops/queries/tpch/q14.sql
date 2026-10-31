-- TPC-H Query 14: Promotion Effect
-- T40 (workspace#595): as TPC-H/scale10/queries/q14.sql (its ORDER BY is total, or one row)
select
	100.00 * sum(case
		when p_type like 'PROMO%'
			then l_extendedprice * (1 - l_discount)
		else 0
	end) / sum(l_extendedprice * (1 - l_discount)) as promo_revenue
from
	lineitem,
	part
where
	l_partkey = p_partkey
	and l_shipdate >= date '1995-09-01'
	and l_shipdate < DATE_ADD(DATE '1995-09-01', INTERVAL 1 MONTH);
