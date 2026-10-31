-- user: app_reader
-- app_reader through its own grants: SELECT on customer and on the view.
SELECT COUNT(*), SUM(credit), MIN(code), MAX(code) FROM app_owner.customer;
SELECT id, name, n_orders, amount FROM app_owner.v_customer_orders WHERE id <= 10 ORDER BY id;
