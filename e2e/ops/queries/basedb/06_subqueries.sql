-- NOT EXISTS, correlated scalar subquery and IN subquery.
SELECT 'no_orders', COUNT(*) FROM app_owner.customer c
  WHERE NOT EXISTS (SELECT 1 FROM app_owner.orders o WHERE o.customer_id = c.id);
SELECT 'above_avg', COUNT(*) FROM app_owner.customer c
  WHERE (SELECT COUNT(*) FROM app_owner.orders o WHERE o.customer_id = c.id)
      > (SELECT COUNT(*) / COUNT(DISTINCT customer_id) FROM app_owner.orders);
SELECT 'big_item_orders', COUNT(*), SUM(total) FROM app_owner.orders
  WHERE id IN (SELECT order_id FROM app_owner.order_item WHERE qty * unit_price > 400);
