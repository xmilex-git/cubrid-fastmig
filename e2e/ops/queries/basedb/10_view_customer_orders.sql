-- The aggregating view (LEFT JOIN, GROUP BY): its top rows and totals.
SELECT id, name, n_orders, amount FROM app_owner.v_customer_orders ORDER BY amount DESC, id LIMIT 20;
SELECT COUNT(*), SUM(n_orders), SUM(amount), SUM(CASE WHEN n_orders = 0 THEN 1 ELSE 0 END) FROM app_owner.v_customer_orders;
