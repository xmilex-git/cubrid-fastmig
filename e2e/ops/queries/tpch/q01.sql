-- TPC-H Query 1: Pricing Summary Report
-- T40 (workspace#595): AVG (DOUBLE) rounded to 6 decimals; else as TPC-H/scale10/queries/q1.sql
select
	l_returnflag,
	l_linestatus,
	sum(l_quantity) as sum_qty,
	sum(l_extendedprice) as sum_base_price,
	sum(l_extendedprice * (1 - l_discount)) as sum_disc_price,
	sum(l_extendedprice * (1 - l_discount) * (1 + l_tax)) as sum_charge,
	round(avg(l_quantity), 6) as avg_qty,
	round(avg(l_extendedprice), 6) as avg_price,
	round(avg(l_discount), 6) as avg_disc,
	count(*) as count_order
from
	lineitem
where
	l_shipdate <= DATE_SUB(DATE '1998-12-01', INTERVAL 90 DAY)
group by
	l_returnflag,
	l_linestatus
order by
	l_returnflag,
	l_linestatus;
