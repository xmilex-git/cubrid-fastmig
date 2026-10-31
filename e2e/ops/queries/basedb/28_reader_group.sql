-- user: app_reader
-- app_reader through its group app_group: SELECT on orders.
SELECT status, COUNT(*), SUM(total) FROM app_owner.orders GROUP BY status ORDER BY status;
