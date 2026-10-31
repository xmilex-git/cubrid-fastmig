-- TPC-H Query 15: Top Supplier
-- T40 (workspace#595): the view revenue0 of q15.sql as a WITH clause, so the query set writes nothing; else as q15_select.sql
with revenue0 (supplier_no, total_revenue) as (
	select
		l_suppkey,
		sum(l_extendedprice * (1 - l_discount))
	from
		lineitem
	where
		l_shipdate >= date '1996-01-01'
		and l_shipdate < DATE_ADD(DATE '1996-01-01', INTERVAL 3 MONTH)
	group by
		l_suppkey
)
select
	s_suppkey,
	s_name,
	s_address,
	s_phone,
	total_revenue
from
	supplier,
	revenue0
where
	s_suppkey = supplier_no
	and total_revenue = (
		select
			max(total_revenue)
		from
			revenue0
	)
order by
	s_suppkey;
