-- run as app_owner; every value is a function of seq_n.n, so both builds load the same rows
INSERT INTO seq_n VALUES (1);
INSERT INTO seq_n SELECT n + 1 FROM seq_n;
INSERT INTO seq_n SELECT n + 2 FROM seq_n;
INSERT INTO seq_n SELECT n + 4 FROM seq_n;
INSERT INTO seq_n SELECT n + 8 FROM seq_n;
INSERT INTO seq_n SELECT n + 16 FROM seq_n;
INSERT INTO seq_n SELECT n + 32 FROM seq_n;
INSERT INTO seq_n SELECT n + 64 FROM seq_n;
INSERT INTO seq_n SELECT n + 128 FROM seq_n;
INSERT INTO seq_n SELECT n + 256 FROM seq_n;
INSERT INTO seq_n SELECT n + 512 FROM seq_n;
INSERT INTO seq_n SELECT n + 1024 FROM seq_n;
INSERT INTO seq_n SELECT n + 2048 FROM seq_n;
INSERT INTO seq_n SELECT n + 4096 FROM seq_n;
INSERT INTO seq_n SELECT n + 8192 FROM seq_n;
INSERT INTO seq_n SELECT n + 16384 FROM seq_n;
INSERT INTO seq_n SELECT n + 32768 FROM seq_n;

INSERT INTO customer (id, code, name, name_n, grade, region, credit, score, ratio, born, created, tz_at, ltz_at, flags, profile, note)
SELECT n, LPAD(n, 8, '0'), 'Cust ' || n, N'고객' || n, CAST(ELT(MOD(n, 3) + 1, N'A', N'B', N'C') AS NCHAR(2)),
       ELT(MOD(n, 4) + 1, 'N', 'S', 'E', 'W'), MOD(n * 37, 100000) / 100.0, n / 7.0e0, MOD(n, 100) / 3.0,
       ADDDATE(DATE '1960-01-01', MOD(n * 13, 15000)), DATETIME '2020-01-01 00:00:00' + n * 60000,
       TIMESTAMPTZ '2021-06-01 00:00:00 Asia/Seoul' + n, DATETIMELTZ '2021-06-01 00:00:00' + n * 1000,
       CASE MOD(n, 2) WHEN 0 THEN X'A5' ELSE X'5A' END, JSON_OBJECT('n', n, 'tier', MOD(n, 3)), REPEAT('x', MOD(n, 50))
FROM seq_n WHERE n <= 5000;
UPDATE customer SET tags = {'vip', 'kr'} WHERE MOD(id, 10) = 0;
UPDATE customer SET tags = {'new'} WHERE MOD(id, 10) = 1;

INSERT INTO product (id, sku, title, price, weight, active, spec)
SELECT n, 'SKU-' || LPAD(n, 6, '0'), 'Product ' || n, MOD(n * 97, 100000) / 100.0, n / 10.0, MOD(n, 2),
       JSON_OBJECT('w', n, 'tags', JSON_ARRAY('a', MOD(n, 5)))
FROM seq_n WHERE n <= 1000;
UPDATE product SET image = BIT_TO_BLOB(X'89504E470D0A1A0A0000000D49484452'), manual = CHAR_TO_CLOB(REPEAT('manual ', 500)) WHERE id <= 20;

INSERT INTO orders (id, customer_id, ordered_on, status, total, memo)
SELECT n, MOD(n * 7, 5000) + 1, ADDDATE(DATE '2023-01-01', MOD(n, 1000)), ELT(MOD(n, 3) + 1, 'N', 'P', 'C'),
       MOD(n * 101, 1000000) / 100.0, CASE WHEN MOD(n, 500) = 0 THEN REPEAT('m', 3000) END
FROM seq_n WHERE n <= 20000;

INSERT INTO order_item (order_id, line_no, product_id, qty, unit_price)
SELECT o.id, k.n, MOD(o.id * k.n, 1000) + 1, MOD(o.id, 5) + 1, MOD(o.id * k.n * 13, 10000) / 100.0
FROM orders o, seq_n k WHERE k.n <= 3;

INSERT INTO event_log (id, ev_date, kind, payload)
SELECT n, ADDDATE(DATE '2023-01-01', MOD(n, 900)), ELT(MOD(n, 4) + 1, 'login', 'view', 'buy', 'logout'), REPEAT(CHR(65 + MOD(n, 26)), 400)
FROM seq_n WHERE n <= 40000;

INSERT INTO session_hash (user_id, token, last_seen)
SELECT MOD(n, 2000) + 1, MD5(n), DATETIME '2024-01-01 00:00:00' + n * 1000
FROM seq_n WHERE n <= 8000;

INSERT INTO region_list (id, code, label)
SELECT n, ELT(MOD(n, 4) + 1, 'N', 'S', 'E', 'W'), 'region ' || n
FROM seq_n WHERE n <= 100;

INSERT INTO doc_big (id, body)
SELECT n, REPEAT('d', 20000) || n
FROM seq_n WHERE n <= 20;

INSERT INTO g5_expr (id, email, amount, deleted)
SELECT n, 'User' || n || '@Example.COM', MOD(n * 17, 1000), CASE WHEN MOD(n, 5) = 0 THEN 1 ELSE 0 END
FROM seq_n WHERE n <= 500;

INSERT INTO g5_fx (id, name, a, b, c, d, code)
SELECT n, 'Name' || n, MOD(n, 10), 'Bee' || MOD(n, 37), MOD(n, 30), CASE WHEN MOD(n, 4) = 0 THEN NULL ELSE 'd' || n END, 'Code-' || n
FROM seq_n WHERE n <= 300;
