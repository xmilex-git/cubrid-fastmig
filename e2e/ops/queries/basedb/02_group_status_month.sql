-- GROUP BY on an expression with HAVING: orders per status and month.
SELECT status, TO_CHAR(ordered_on, 'YYYY-MM') AS ym, COUNT(*), SUM(total), CAST(SUM(total) / COUNT(*) AS NUMERIC(20,4))
FROM app_owner.orders
GROUP BY status, TO_CHAR(ordered_on, 'YYYY-MM')
HAVING COUNT(*) > 100
ORDER BY status, ym;
