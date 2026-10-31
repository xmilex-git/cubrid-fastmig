-- The Java SP called per row and in a predicate.
SELECT id, dba.fm_hello(code) FROM app_owner.customer WHERE id <= 5 ORDER BY id;
SELECT COUNT(*) FROM app_owner.product WHERE dba.fm_hello(sku) LIKE '%SKU-00001%';
