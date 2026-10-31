-- min-source: 11.4
-- The PL/CSQL function in the select list and an aggregate over it.
SELECT order_id, line_no, qty, unit_price, app_owner.f_line_total(qty, unit_price) FROM app_owner.order_item
WHERE order_id <= 5 ORDER BY order_id, line_no;
SELECT SUM(app_owner.f_line_total(qty, unit_price)) FROM app_owner.order_item WHERE order_id <= 1000;
