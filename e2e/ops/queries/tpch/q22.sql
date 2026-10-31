-- TPC-H Query 22: Global Sales Opportunity
-- T40 (workspace#595): as TPC-H/scale10/queries/q22.sql (its ORDER BY is total, or one row)
select
	cntrycode,
	count(*) as numcust,
	sum(c_acctbal) as totacctbal
from
	(
		select
			SUBSTRING(c_phone, 1, 2) as cntrycode,
			c_acctbal
		from
			customer
		where
			SUBSTRING(c_phone, 1, 2) in
				('13', '31', '23', '29', '30', '18', '17')
			and c_acctbal > (
				select
					avg(c_acctbal)
				from
					customer
				where
					c_acctbal > 0.00
					and SUBSTRING(c_phone, 1, 2) in
						('13', '31', '23', '29', '30', '18', '17')
			)
			and not exists (
				select
					*
				from
					orders
				where
					o_custkey = c_custkey
			)
	) as custsale
group by
	cntrycode
order by
	cntrycode;
