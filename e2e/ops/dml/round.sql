-- One DML round of the operations scenario (e2e/ops/README.md), run as dba on a target of every source label.
-- Every new key comes from the current max or count, so each side and each round writes the same rows; values a serial
-- or AUTO_INCREMENT column generates differ between the sides (docs/intended-differences.md) and are never keyed on.
-- Objects: basedb/10_schema.sql, 30_objects.sql, 31_dba_objects.sql, 46_partition_fx.sql. Synonyms and PL/CSQL are in
-- round_synonym.sql and round_plcsql.sql (sources that have them). Source: G11 workspace#587 (3).

-- orders: 2000 new rows (trigger trg_orders_ai -> audit_log AUTO_INCREMENT), their items, invoices from the serial
INSERT INTO app_owner.orders (id, customer_id, ordered_on, status, total, memo)
  SELECT m.mx + s.n, MOD((m.mx + s.n) * 7, 5000) + 1, ADDDATE(DATE '2025-01-01', MOD(m.mx + s.n, 300)), 'N',
         MOD((m.mx + s.n) * 101, 1000000) / 100.0, CASE WHEN MOD(s.n, 500) = 0 THEN REPEAT('r', 3000) END
  FROM (SELECT MAX(id) mx FROM app_owner.orders) m, app_owner.seq_n s WHERE s.n <= 2000;
INSERT INTO app_owner.order_item (order_id, line_no, product_id, qty, unit_price)
  SELECT o.id, k.n, MOD(o.id * k.n, 1000) + 1, MOD(o.id, 5) + 1, MOD(o.id * k.n * 13, 10000) / 100.0
  FROM app_owner.orders o, app_owner.seq_n k
  WHERE k.n <= 3 AND o.id > (SELECT MAX(order_id) FROM app_owner.order_item);
INSERT INTO app_owner.invoice (inv_no, order_id, amount)
  SELECT app_owner.s_invoice.NEXT_VALUE, id, total FROM app_owner.orders
  WHERE MOD(id, 10) = 0 AND id > (SELECT MAX(order_id) FROM app_owner.invoice);
UPDATE app_owner.orders SET status = 'P', total = total + 1 WHERE MOD(id, 13) = 0;
-- the oldest 200 orders go, their items by ON DELETE CASCADE
DELETE FROM app_owner.orders WHERE id < (SELECT MIN(id) FROM app_owner.orders) + 200;

-- customer: 50 AUTO_INCREMENT rows keyed by code, half of the earlier ones deleted; the oldest customer goes with its
-- orders (ON DELETE CASCADE); collation utf8_en_ci name, SET, JSON, NCHAR, BIT, desc index columns
INSERT INTO app_owner.customer (code, name, name_n, region, credit, born, profile)
  SELECT 'R' || LPAD(m.mx + s.n, 7, '0'), 'Round ' || (m.mx + s.n), N'라운드' || s.n, ELT(MOD(s.n, 4) + 1, 'N', 'S', 'E', 'W'),
         s.n, ADDDATE(DATE '1990-01-01', s.n), JSON_OBJECT('r', m.mx + s.n)
  FROM (SELECT NVL(MAX(CAST(SUBSTR(code, 2) AS INT)), 0) mx FROM app_owner.customer WHERE code LIKE 'R%') m,
       app_owner.seq_n s WHERE s.n <= 50;
DELETE FROM app_owner.customer WHERE code LIKE 'R%' AND MOD(CAST(SUBSTR(code, 2) AS INT), 2) = 0
  AND CAST(SUBSTR(code, 2) AS INT) <= (SELECT MAX(CAST(SUBSTR(code, 2) AS INT)) FROM app_owner.customer WHERE code LIKE 'R%') - 50;
DELETE FROM app_owner.customer WHERE id = (SELECT MIN(id) FROM app_owner.customer);
UPDATE app_owner.customer SET credit = credit + 1, born = ADDDATE(born, 1), name = UPPER(name) WHERE MOD(id, 7) = 0;
UPDATE app_owner.customer SET tags = {'vip', 'r'}, flags = X'FF', grade = N'Z' WHERE MOD(id, 10) = 5;
UPDATE app_owner.customer SET profile = JSON_SET(profile, '$.tier', 9), name_n = name_n || N'가' WHERE MOD(id, 10) = 3 AND CHAR_LENGTH(name_n) < 40;

-- product: the price through the WITH CHECK OPTION view, one LOB row rewritten in turn (trigger trg_product_bu fires)
UPDATE app_owner.v_active_product SET price = price + 0.01 WHERE id <= 50;
UPDATE app_owner.product SET image = BIT_TO_BLOB(X'89504E470D0A1A0A' || X'00000001'),
                             manual = CHAR_TO_CLOB(REPEAT('round ', 300) || id)
  WHERE id = MOD((SELECT COUNT(*) FROM app_owner.orders), 20) + 1;
UPDATE app_owner.product SET spec = JSON_SET(spec, '$.w', id * 2) WHERE MOD(id, 9) = 0;

-- partitions: RANGE event_log, the big write of the round (every payload rotated by one character); HASH and LIST
INSERT INTO app_owner.event_log (id, ev_date, kind, payload)
  SELECT m.mx + s.n, ADDDATE(DATE '2023-06-01', MOD(s.n, 900)), ELT(MOD(s.n, 4) + 1, 'login', 'view', 'buy', 'logout'),
         REPEAT(CHR(65 + MOD(s.n, 26)), 400)
  FROM (SELECT MAX(id) mx FROM app_owner.event_log) m, app_owner.seq_n s WHERE s.n <= 4000;
UPDATE app_owner.event_log SET payload = SUBSTR(payload, 2) || SUBSTR(payload, 1, 1);
DELETE FROM app_owner.event_log WHERE id < (SELECT MIN(id) FROM app_owner.event_log) + 3000;
DELETE FROM app_owner.session_hash WHERE user_id = MOD((SELECT COUNT(*) FROM app_owner.orders), 2000) + 1;
INSERT INTO app_owner.session_hash (user_id, token, last_seen)
  SELECT MOD(m.c + s.n, 2000) + 1, MD5(m.c + s.n + 100000), DATETIME '2025-01-01 00:00:00' + s.n * 1000
  FROM (SELECT COUNT(*) c FROM app_owner.orders) m, app_owner.seq_n s WHERE s.n <= 100;
UPDATE app_owner.region_list SET label = SUBSTR(label || 'r', 1, 40), code = ELT(MOD(id, 4) + 1, 'S', 'N', 'W', 'E')
  WHERE MOD(id, 5) = 0;

-- partition expressions (46_partition_fx.sql): rows move between partitions
UPDATE app_owner.part_ascii SET c = CHR(65 + MOD(id + ASCII(c), 26)) || id WHERE MOD(id, 3) = 0;
UPDATE app_owner.part_hex SET c = ELT(MOD(id + LENGTH(c), 2) + 1, 'A', 'B') WHERE MOD(id, 4) = 0;
UPDATE app_owner.part_conv SET c = HEX(id + 7) WHERE MOD(id, 5) = 0;
UPDATE app_owner.part_inet SET ip = ELT(MOD(id, 3) + 1, '9.0.0.', '11.0.0.', '10.0.0.') || MOD(id * 3, 250) WHERE MOD(id, 6) = 0;
UPDATE app_owner.part_char_const SET c = ELT(MOD(id + ASCII(c), 2) + 1, 'aaaa', 'bbbb') WHERE MOD(id, 7) = 0;
UPDATE app_owner.part_num_const SET v = MOD(v * 3, 1000) WHERE MOD(id, 8) = 0;
UPDATE app_owner.part_num_hash SET amt = amt + 0.25 WHERE MOD(k, 9) = 0;

-- overflow records, function and filter indexes
UPDATE app_owner.doc_big SET body = SUBSTR(body, 1, 20000) || id || 'r' WHERE id <= 5;
UPDATE app_owner.g5_expr SET deleted = 1 - deleted, email = UPPER(email) WHERE MOD(id, 7) = 0;
UPDATE app_owner.g5_fx SET name = LOWER(name), c = MOD(c + 11, 30), d = NVL(d, 'd' || id) WHERE MOD(id, 6) = 0;

-- dba objects: utf8_ko_cs table, Java SP
UPDATE dba.ko_text SET txt = CASE WHEN CHAR_LENGTH(txt) > 40 THEN '가나다' || id ELSE txt || '라' END WHERE MOD(id, 3) = 0;
UPDATE dba.app_settings SET v = dba.fm_hello(k);

-- users and grants: a privilege granted and revoked again
GRANT UPDATE ON app_owner.customer TO app_reader;
REVOKE UPDATE ON app_owner.customer FROM app_reader;
