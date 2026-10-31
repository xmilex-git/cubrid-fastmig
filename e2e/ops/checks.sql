-- Checks after every step of the operations scenario (e2e/ops/README.md), run as dba with csql -t on either side.
-- Per table: row count and a CRC32 sum (BIGINT: an INT sum overflows) over every column but the values a serial or
-- AUTO_INCREMENT generates (intended differences between the two sides, docs/intended-differences.md: compared as
-- counts and order only). Per index: the count read through that index against the heap count of the same predicate;
-- the runner fails on "INDEX!=HEAP".
-- Source: G12 workspace#587 (1); T36 workspace#588.

-- tables
SELECT 'customer', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', CASE WHEN code LIKE 'R%' THEN NULL ELSE id END, code, name, HEX(name_n),
  HEX(grade), region, credit, score, ratio, born, created, tz_at, ltz_at, HEX(flags), profile, note))),
  SUM(CASE WHEN tags IS NULL THEN 0 ELSE 1 END) FROM app_owner.customer;
SELECT 'product', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', id, sku, title, price, weight, active, spec,
  MD5(HEX(BLOB_TO_BIT(image))), MD5(CLOB_TO_CHAR(manual))))) FROM app_owner.product;
SELECT 'orders', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', id, customer_id, ordered_on, status, total, MD5(memo)))) FROM app_owner.orders;
SELECT 'order_item', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', order_id, line_no, product_id, qty, unit_price))) FROM app_owner.order_item;
SELECT 'invoice', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', order_id, amount))) FROM app_owner.invoice;
SELECT 'audit_log', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', tbl, ref_id, act))) FROM app_owner.audit_log;
-- order: AUTO_INCREMENT ids follow the insert order of the orders behind them
SELECT 'audit_log_order', COUNT(*) FROM (SELECT id, LAG(id) OVER (ORDER BY ref_id) p FROM app_owner.audit_log
  WHERE tbl = 'orders') t WHERE p > id;
SELECT 'invoice_order', COUNT(*) FROM (SELECT inv_no, LAG(inv_no) OVER (ORDER BY order_id) p FROM app_owner.invoice) t
  WHERE p > inv_no;
SELECT 'event_log', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', id, ev_date, kind, MD5(payload)))) FROM app_owner.event_log;
SELECT 'session_hash', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', user_id, token, last_seen))) FROM app_owner.session_hash;
SELECT 'region_list', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', id, code, label))) FROM app_owner.region_list;
SELECT 'doc_big', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', id, MD5(body)))) FROM app_owner.doc_big;
SELECT 'g5_expr', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', id, email, amount, deleted))) FROM app_owner.g5_expr;
SELECT 'g5_fx', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', id, name, a, b, c, d, code))) FROM app_owner.g5_fx;
SELECT 'seq_n', COUNT(*), SUM(CAST(0 AS BIGINT) + n) FROM app_owner.seq_n;
SELECT 'part_ascii', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', id, c))) FROM app_owner.part_ascii;
SELECT 'part_hex', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', id, c))) FROM app_owner.part_hex;
SELECT 'part_conv', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', id, c))) FROM app_owner.part_conv;
SELECT 'part_inet', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', id, ip))) FROM app_owner.part_inet;
SELECT 'part_char_const', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', id, c))) FROM app_owner.part_char_const;
SELECT 'part_num_const', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', id, v))) FROM app_owner.part_num_const;
SELECT 'part_num_hash', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', k, amt))) FROM app_owner.part_num_hash;
SELECT 'app_settings', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', k, v))) FROM dba.app_settings;
SELECT 'ko_text', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', id, txt))) FROM dba.ko_text;
-- rows per partition
SELECT 'partitions', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', class_name, partition_name))) FROM db_partition;
SELECT 'event_log_p2024', COUNT(*) FROM app_owner.event_log__p__p2024;
SELECT 'region_list_p_ns', COUNT(*) FROM app_owner.region_list__p__p_ns;
SELECT 'part_hex_p0', COUNT(*) FROM app_owner.part_hex__p__p0;
SELECT 'part_inet_p0', COUNT(*) FROM app_owner.part_inet__p__p0;

-- catalog: classes, indexes, triggers, serials (name only), stored procedures, users, grants
SELECT 'classes', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', owner_name, class_name, class_type))) FROM db_class
  WHERE is_system_class = 'NO';
SELECT 'indexes', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', class_name, index_name, is_unique, is_primary_key, key_count)))
  FROM db_index WHERE class_name IN (SELECT class_name FROM db_class WHERE is_system_class = 'NO');
SELECT 'triggers', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', trigger_name, target_class_name))) FROM db_trigger;
SELECT 'serials', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(name)) FROM db_serial;
SELECT 'stored_procs', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', sp_name, sp_type, lang))) FROM db_stored_procedure;
SELECT 'users', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(name)) FROM db_user;
SELECT 'auths', COUNT(*), SUM(CAST(0 AS BIGINT) + CRC32(CONCAT_WS('|', grantee_name, grantor_name, object_name, auth_type, is_grantable)))
  FROM db_auth WHERE grantee_name <> 'DBA';
-- what the SPs, views and synonyms return
SELECT 'fm_hello', dba.fm_hello('ops') FROM db_root;
SELECT 'v_customer_orders', COUNT(*), SUM(n_orders), SUM(amount) FROM app_owner.v_customer_orders;
SELECT 'v_active_product', COUNT(*), SUM(price) FROM app_owner.v_active_product;

-- index = heap: the same predicate through the index (USING INDEX i(+) forces it) and with no index
SELECT 'pk_customer_id', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.customer WHERE id > -2147483648 USING INDEX pk_customer_id(+)) a,
  (SELECT COUNT(*) FROM app_owner.customer WHERE id > -2147483648 USING INDEX NONE) b) t;
SELECT 'u_customer_code', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.customer WHERE code >= '' USING INDEX u_customer_code(+)) a,
  (SELECT COUNT(*) FROM app_owner.customer WHERE code >= '' USING INDEX NONE) b) t;
SELECT 'idx_customer_name', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.customer WHERE name >= '' USING INDEX idx_customer_name(+)) a,
  (SELECT COUNT(*) FROM app_owner.customer WHERE name >= '' USING INDEX NONE) b) t;
SELECT 'idx_customer_region_credit', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.customer WHERE region IN ('N', 'S', 'E', 'W') USING INDEX idx_customer_region_credit(+)) a,
  (SELECT COUNT(*) FROM app_owner.customer WHERE region IN ('N', 'S', 'E', 'W') USING INDEX NONE) b) t;
SELECT 'idx_customer_born_desc', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.customer WHERE born >= DATE '0001-01-01' USING INDEX idx_customer_born_desc(+)) a,
  (SELECT COUNT(*) FROM app_owner.customer WHERE born >= DATE '0001-01-01' USING INDEX NONE) b) t;
SELECT 'pk_product_id', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.product WHERE id > -2147483648 USING INDEX pk_product_id(+)) a,
  (SELECT COUNT(*) FROM app_owner.product WHERE id > -2147483648 USING INDEX NONE) b) t;
SELECT 'u_product_sku', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.product WHERE sku >= '' USING INDEX u_product_sku(+)) a,
  (SELECT COUNT(*) FROM app_owner.product WHERE sku >= '' USING INDEX NONE) b) t;
SELECT 'pk_orders_id', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.orders WHERE id > -9223372036854775807 USING INDEX pk_orders_id(+)) a,
  (SELECT COUNT(*) FROM app_owner.orders WHERE id > -9223372036854775807 USING INDEX NONE) b) t;
SELECT 'fk_orders_customer', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.orders WHERE customer_id > -2147483648 USING INDEX fk_orders_customer(+)) a,
  (SELECT COUNT(*) FROM app_owner.orders WHERE customer_id > -2147483648 USING INDEX NONE) b) t;
SELECT 'idx_orders_date_status', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.orders WHERE ordered_on >= DATE '0001-01-01' USING INDEX idx_orders_date_status(+)) a,
  (SELECT COUNT(*) FROM app_owner.orders WHERE ordered_on >= DATE '0001-01-01' USING INDEX NONE) b) t;
SELECT 'pk_order_item_order_id_line_no', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.order_item WHERE order_id > -9223372036854775807 USING INDEX pk_order_item_order_id_line_no(+)) a,
  (SELECT COUNT(*) FROM app_owner.order_item WHERE order_id > -9223372036854775807 USING INDEX NONE) b) t;
SELECT 'fk_item_order', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.order_item WHERE order_id > -9223372036854775807 USING INDEX fk_item_order(+)) a,
  (SELECT COUNT(*) FROM app_owner.order_item WHERE order_id > -9223372036854775807 USING INDEX NONE) b) t;
SELECT 'fk_item_product', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.order_item WHERE product_id > -2147483648 USING INDEX fk_item_product(+)) a,
  (SELECT COUNT(*) FROM app_owner.order_item WHERE product_id > -2147483648 USING INDEX NONE) b) t;
SELECT 'idx_item_product', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.order_item WHERE product_id > -2147483648 USING INDEX idx_item_product(+)) a,
  (SELECT COUNT(*) FROM app_owner.order_item WHERE product_id > -2147483648 USING INDEX NONE) b) t;
SELECT 'pk_invoice_inv_no', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.invoice WHERE inv_no > -2147483648 USING INDEX pk_invoice_inv_no(+)) a,
  (SELECT COUNT(*) FROM app_owner.invoice WHERE inv_no > -2147483648 USING INDEX NONE) b) t;
SELECT 'pk_audit_log_id', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.audit_log WHERE id > -2147483648 USING INDEX pk_audit_log_id(+)) a,
  (SELECT COUNT(*) FROM app_owner.audit_log WHERE id > -2147483648 USING INDEX NONE) b) t;
SELECT 'pk_event_log_id_ev_date', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.event_log WHERE id > -9223372036854775807 USING INDEX pk_event_log_id_ev_date(+)) a,
  (SELECT COUNT(*) FROM app_owner.event_log WHERE id > -9223372036854775807 USING INDEX NONE) b) t;
SELECT 'idx_event_kind_date', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.event_log WHERE kind >= '' USING INDEX idx_event_kind_date(+)) a,
  (SELECT COUNT(*) FROM app_owner.event_log WHERE kind >= '' USING INDEX NONE) b) t;
SELECT 'u_session_hash_user_id_token', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.session_hash WHERE user_id > -2147483648 USING INDEX u_session_hash_user_id_token(+)) a,
  (SELECT COUNT(*) FROM app_owner.session_hash WHERE user_id > -2147483648 USING INDEX NONE) b) t;
SELECT 'idx_region_list_id', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.region_list WHERE id > -2147483648 USING INDEX idx_region_list_id(+)) a,
  (SELECT COUNT(*) FROM app_owner.region_list WHERE id > -2147483648 USING INDEX NONE) b) t;
SELECT 'i_f5', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.region_list WHERE id > 50 USING INDEX i_f5(+)) a,
  (SELECT COUNT(*) FROM app_owner.region_list WHERE id > 50 USING INDEX NONE) b) t;
SELECT 'pk_doc_big_id', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.doc_big WHERE id > -2147483648 USING INDEX pk_doc_big_id(+)) a,
  (SELECT COUNT(*) FROM app_owner.doc_big WHERE id > -2147483648 USING INDEX NONE) b) t;
SELECT 'pk_g5_expr_id', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.g5_expr WHERE id > -2147483648 USING INDEX pk_g5_expr_id(+)) a,
  (SELECT COUNT(*) FROM app_owner.g5_expr WHERE id > -2147483648 USING INDEX NONE) b) t;
SELECT 'idx_g5_lower_email', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.g5_expr WHERE LOWER(email) >= '' USING INDEX idx_g5_lower_email(+)) a,
  (SELECT COUNT(*) FROM app_owner.g5_expr WHERE LOWER(email) >= '' USING INDEX NONE) b) t;
SELECT 'idx_g5_amount_live', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.g5_expr WHERE amount > -2147483648 AND deleted = 0 USING INDEX idx_g5_amount_live(+)) a,
  (SELECT COUNT(*) FROM app_owner.g5_expr WHERE amount > -2147483648 AND deleted = 0 USING INDEX NONE) b) t;
SELECT 'pk_g5_fx_id', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.g5_fx WHERE id > -2147483648 USING INDEX pk_g5_fx_id(+)) a,
  (SELECT COUNT(*) FROM app_owner.g5_fx WHERE id > -2147483648 USING INDEX NONE) b) t;
SELECT 'i_f1', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.g5_fx WHERE UPPER(name) >= '' USING INDEX i_f1(+)) a,
  (SELECT COUNT(*) FROM app_owner.g5_fx WHERE UPPER(name) >= '' USING INDEX NONE) b) t;
SELECT 'i_f2', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.g5_fx WHERE a > -2147483648 USING INDEX i_f2(+)) a,
  (SELECT COUNT(*) FROM app_owner.g5_fx WHERE a > -2147483648 USING INDEX NONE) b) t;
SELECT 'i_f4', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.g5_fx WHERE c > 10 AND d IS NOT NULL USING INDEX i_f4(+)) a,
  (SELECT COUNT(*) FROM app_owner.g5_fx WHERE c > 10 AND d IS NOT NULL USING INDEX NONE) b) t;
SELECT 'pk_seq_n_n', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.seq_n WHERE n > -2147483648 USING INDEX pk_seq_n_n(+)) a,
  (SELECT COUNT(*) FROM app_owner.seq_n WHERE n > -2147483648 USING INDEX NONE) b) t;
SELECT 'pk_part_num_hash_k', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM app_owner.part_num_hash WHERE k > -2147483648 USING INDEX pk_part_num_hash_k(+)) a,
  (SELECT COUNT(*) FROM app_owner.part_num_hash WHERE k > -2147483648 USING INDEX NONE) b) t;
SELECT 'pk_app_settings_k', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM dba.app_settings WHERE k >= '' USING INDEX pk_app_settings_k(+)) a,
  (SELECT COUNT(*) FROM dba.app_settings WHERE k >= '' USING INDEX NONE) b) t;
SELECT 'pk_ko_text_id', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM dba.ko_text WHERE id > -2147483648 USING INDEX pk_ko_text_id(+)) a,
  (SELECT COUNT(*) FROM dba.ko_text WHERE id > -2147483648 USING INDEX NONE) b) t;
SELECT 'idx_ko_text_txt', CASE WHEN a = b THEN 'index=heap' ELSE 'INDEX!=HEAP ' || a || ' ' || b END FROM (SELECT
  (SELECT COUNT(*) FROM dba.ko_text WHERE txt >= '' USING INDEX idx_ko_text_txt(+)) a,
  (SELECT COUNT(*) FROM dba.ko_text WHERE txt >= '' USING INDEX NONE) b) t;
