-- run as app_owner: relocations, overflow already in doc_big, trigger/serial/AI/SP use
-- override (11.2, 11.3): no PL/CSQL calls (f_line_total, p_close_order); the Java SP is called without the owner prefix (dba.fm_hello is rejected before 11.4)
UPDATE customer SET note = REPEAT('y', 900) WHERE MOD(id, 25) = 0;

ALTER TABLE customer AUTO_INCREMENT = 10001;
INSERT INTO customer (code, name, credit) VALUES ('AI000001', 'ai one', 1.00);
INSERT INTO customer (code, name, credit) VALUES ('AI000002', 'ai two', 2.00);
INSERT INTO customer (code, name, credit) VALUES ('AI000003', 'ai three', 3.00);

INSERT INTO orders (id, customer_id, ordered_on, status, total) VALUES (20001, 10001, DATE '2025-03-01', 'N', 10.00);
INSERT INTO orders (id, customer_id, ordered_on, status, total) VALUES (20002, 10002, DATE '2025-03-02', 'N', 20.00);

SELECT fm_hello ('fastmig') FROM db_root;
SELECT s_invoice.NEXT_VALUE FROM db_root;
