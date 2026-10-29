-- run as app_owner, right after 20_data.sql and only when the rows multiplier k > 1 (build.sh replaces
-- @ROWS_MULTIPLIER@ with k). Adds (k - 1) times the T1 rows of customer, orders, order_item and event_log;
-- T1 rows stay as they are. New ids start at 1000001, clear of the ids 40_post.sql inserts later.
-- Runs before 30_objects.sql, so trg_orders_ai does not fire for these orders.
-- seq_n holds 1..65536; m = (b.n - 1) * 65536 + a.n numbers up to 65536^2 rows.
-- Interval arithmetic is INT: MOD keeps m * 60000 and m * 1000 under 2^31 (MOD(m, 5000000) * 60000 overflowed at k = 180).

INSERT INTO customer (id, code, name, name_n, grade, region, credit, score, ratio, born, created, tz_at, ltz_at, flags, profile, note)
SELECT 1000000 + m, LPAD(1000000 + m, 8, '0'), 'Cust ' || m, N'고객' || m, CAST(ELT(MOD(m, 3) + 1, N'A', N'B', N'C') AS NCHAR(2)),
       ELT(MOD(m, 4) + 1, 'N', 'S', 'E', 'W'), MOD(m * 37, 100000) / 100.0, m / 7.0e0, MOD(m, 100) / 3.0,
       ADDDATE(DATE '1960-01-01', MOD(m * 13, 15000)), DATETIME '2020-01-01 00:00:00' + MOD(m, 30000) * 60000,
       TIMESTAMPTZ '2021-06-01 00:00:00 Asia/Seoul' + m, DATETIMELTZ '2021-06-01 00:00:00' + MOD(m, 2000000) * 1000,
       CASE MOD(m, 2) WHEN 0 THEN X'A5' ELSE X'5A' END, JSON_OBJECT('n', m, 'tier', MOD(m, 3)), REPEAT('x', MOD(m, 50))
FROM (SELECT (b.n - 1) * 65536 + a.n AS m FROM seq_n a, seq_n b
      WHERE b.n <= CEIL (5000 * (@ROWS_MULTIPLIER@ - 1) / 65536.0)) s
WHERE m <= 5000 * (@ROWS_MULTIPLIER@ - 1);

INSERT INTO orders (id, customer_id, ordered_on, status, total, memo)
SELECT 1000000 + m, 1000001 + MOD(m * 7, 5000 * (@ROWS_MULTIPLIER@ - 1)), ADDDATE(DATE '2023-01-01', MOD(m, 1000)),
       ELT(MOD(m, 3) + 1, 'N', 'P', 'C'), MOD(m * 101, 1000000) / 100.0, CASE WHEN MOD(m, 500) = 0 THEN REPEAT('m', 3000) END
FROM (SELECT (b.n - 1) * 65536 + a.n AS m FROM seq_n a, seq_n b
      WHERE b.n <= CEIL (20000 * (@ROWS_MULTIPLIER@ - 1) / 65536.0)) s
WHERE m <= 20000 * (@ROWS_MULTIPLIER@ - 1);

INSERT INTO order_item (order_id, line_no, product_id, qty, unit_price)
SELECT o.id, k.n, MOD(o.id * k.n, 1000) + 1, MOD(o.id, 5) + 1, MOD(o.id * k.n * 13, 10000) / 100.0
FROM orders o, seq_n k WHERE o.id > 1000000 AND k.n <= 3;

INSERT INTO event_log (id, ev_date, kind, payload)
SELECT 1000000 + m, ADDDATE(DATE '2023-01-01', MOD(m, 900)), ELT(MOD(m, 4) + 1, 'login', 'view', 'buy', 'logout'),
       REPEAT(CHR(65 + MOD(m, 26)), 400)
FROM (SELECT (b.n - 1) * 65536 + a.n AS m FROM seq_n a, seq_n b
      WHERE b.n <= CEIL (40000 * (@ROWS_MULTIPLIER@ - 1) / 65536.0)) s
WHERE m <= 40000 * (@ROWS_MULTIPLIER@ - 1);
