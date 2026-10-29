-- run as app_owner, after 20_data.sql
CREATE SERIAL s_invoice START WITH 1000 INCREMENT BY 5 MAXVALUE 99999999 CACHE 10;
INSERT INTO invoice (inv_no, order_id, amount) SELECT s_invoice.NEXT_VALUE, id, total FROM orders WHERE id <= 300;

CREATE VIEW v_customer_orders AS
  SELECT c.id, c.name, COUNT(o.id) AS n_orders, SUM(o.total) AS amount
  FROM customer c LEFT JOIN orders o ON o.customer_id = c.id
  GROUP BY c.id, c.name;
CREATE VIEW v_active_product AS SELECT id, sku, price FROM product WHERE active = 1 WITH CHECK OPTION;

CREATE TRIGGER trg_orders_ai AFTER INSERT ON orders
  EXECUTE INSERT INTO audit_log (tbl, ref_id, act) VALUES ('orders', obj.id, 'insert');
CREATE TRIGGER trg_product_bu BEFORE UPDATE ON product IF new.price < 0 EXECUTE REJECT;

CREATE SYNONYM syn_cust FOR customer;

CREATE OR REPLACE FUNCTION f_line_total (p_qty INT, p_price NUMERIC) RETURN NUMERIC AS
BEGIN
  RETURN p_qty * p_price;
END;

CREATE OR REPLACE PROCEDURE p_close_order (p_id BIGINT) AS
BEGIN
  UPDATE orders SET status = 'C' WHERE id = p_id;
END;

GRANT SELECT ON customer TO app_reader;
GRANT SELECT ON v_customer_orders TO app_reader;
GRANT SELECT, INSERT, UPDATE ON orders TO app_group WITH GRANT OPTION;
GRANT EXECUTE ON PROCEDURE f_line_total TO app_reader;
