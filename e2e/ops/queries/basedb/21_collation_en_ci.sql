-- utf8_en_ci column: case-insensitive equality and LIKE, and the sort order it gives.
SELECT COUNT(*) FROM app_owner.customer WHERE name = 'CUST 77';
SELECT COUNT(*) FROM app_owner.customer WHERE name LIKE 'cUsT 12%';
SELECT name, id FROM app_owner.customer WHERE id <= 30 OR id > 10000 ORDER BY name, id;
