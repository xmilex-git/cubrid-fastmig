-- Engine smoke of a migrated baseline DB (e2e/run.sh step 7): every SELECT reads through an index; run.sh checks
-- that none errors and that the plan of each shows an index scan. Tables and indexes: basedb/10_schema.sql.
;plan simple
SELECT COUNT(*) FROM app_owner.customer WHERE id > 0;
SELECT id, name FROM app_owner.customer WHERE name >= 'A' ORDER BY name LIMIT 3;
SELECT id, status FROM app_owner.orders WHERE id = 1;
SELECT order_id, line_no FROM app_owner.order_item WHERE order_id = 1 AND line_no > 0;
SELECT kind, ev_date FROM app_owner.event_log WHERE kind = 'login' AND ev_date >= DATE '2000-01-01' LIMIT 3;
