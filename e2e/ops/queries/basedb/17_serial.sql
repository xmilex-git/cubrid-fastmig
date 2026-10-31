-- The serial's definition and the values it generated before the move (the current value is not compared: the
-- official path starts a cached serial ahead, docs/intended-differences.md).
SELECT name, increment_val, min_val, max_val, cyclic, cached_num FROM db_serial WHERE name = 's_invoice';
SELECT COUNT(*), MIN(inv_no), MAX(inv_no), SUM(CASE WHEN MOD(inv_no - 1000, 5) = 0 THEN 1 ELSE 0 END) FROM app_owner.invoice;
SELECT i.inv_no, i.order_id, i.amount, o.total FROM app_owner.invoice i JOIN app_owner.orders o ON o.id = i.order_id
WHERE i.order_id <= 10 ORDER BY i.inv_no;
