-- RANGE partition pruning on ev_date: the 2024 range read through the table and through its partition.
SELECT kind, COUNT(*), MIN(ev_date), MAX(ev_date) FROM app_owner.event_log
WHERE ev_date >= DATE '2024-01-01' AND ev_date < DATE '2025-01-01'
GROUP BY kind ORDER BY kind;
SELECT 'p2024', COUNT(*) FROM app_owner.event_log__p__p2024;
SELECT 'p2023_kind_date', COUNT(*) FROM app_owner.event_log WHERE kind = 'buy' AND ev_date < DATE '2023-03-01';
