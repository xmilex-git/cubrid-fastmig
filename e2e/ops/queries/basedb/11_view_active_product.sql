-- The WITH CHECK OPTION view joined back to its table.
SELECT v.id, v.sku, v.price, p.title FROM app_owner.v_active_product v JOIN app_owner.product p ON p.id = v.id
WHERE v.id <= 40 ORDER BY v.id;
SELECT COUNT(*), SUM(price), MIN(sku), MAX(sku) FROM app_owner.v_active_product;
