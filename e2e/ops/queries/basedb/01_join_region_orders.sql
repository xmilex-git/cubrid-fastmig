-- Inner join and aggregates: orders per customer region (ENUM), totals and date range.
SELECT c.region, COUNT(o.id), SUM(o.total), MIN(o.ordered_on), MAX(o.ordered_on), COUNT(DISTINCT c.id)
FROM app_owner.customer c JOIN app_owner.orders o ON o.customer_id = c.id
GROUP BY c.region
ORDER BY c.region;
