-- Recursive WITH, UNION ALL and outer join.
WITH RECURSIVE r (n) AS (SELECT 1 FROM db_root UNION ALL SELECT n + 1 FROM r WHERE n < 12)
SELECT r.n, COUNT(o.id), SUM(o.total)
FROM r LEFT JOIN app_owner.orders o ON MONTH(o.ordered_on) = r.n AND YEAR(o.ordered_on) = 2024
GROUP BY r.n
ORDER BY r.n;
SELECT 'orders', status, COUNT(*) FROM app_owner.orders GROUP BY status
UNION ALL
SELECT 'event_log', kind, COUNT(*) FROM app_owner.event_log GROUP BY kind
ORDER BY 1, 2;
