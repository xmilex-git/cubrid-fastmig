-- TPC-H Query 6: Forecasting Revenue Change
-- T40 (workspace#595): as TPC-H/scale10/queries/q6.sql (its ORDER BY is total, or one row)
select
	sum(l_extendedprice * l_discount) as revenue
from
	lineitem
where
	l_shipdate >= date '1994-01-01'
	and l_shipdate < DATE_ADD(DATE '1994-01-01', INTERVAL 1 YEAR)
	and l_discount between .06 - 0.01 and .06 + 0.01
	and l_quantity < 24;
