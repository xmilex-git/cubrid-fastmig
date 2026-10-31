-- Three-table join, ORDER BY an aggregate with LIMIT (ties broken by the key).
SELECT p.id, p.sku, SUM(i.qty * i.unit_price) AS revenue, COUNT(DISTINCT i.order_id)
FROM app_owner.order_item i JOIN app_owner.product p ON p.id = i.product_id JOIN app_owner.orders o ON o.id = i.order_id
WHERE o.status <> 'C'
GROUP BY p.id, p.sku
ORDER BY revenue DESC, p.id
LIMIT 20;
