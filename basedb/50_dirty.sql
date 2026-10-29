-- run as app_owner on a server started with vacuum_disable=yes, then stop the server at once
DELETE FROM event_log WHERE MOD(id, 3) = 0;
DELETE FROM orders WHERE id > 19000 AND id <= 20000;
UPDATE customer SET credit = credit + 1 WHERE MOD(id, 7) = 0;
UPDATE doc_big SET body = body || 'u' WHERE id <= 5;
INSERT INTO orders (id, customer_id, ordered_on, status, total) VALUES (30001, 1, DATE '2025-04-01', 'N', 1.00);
