-- What the AFTER INSERT trigger wrote: audit rows per action and their link to the orders behind them.
SELECT tbl, act, COUNT(*), MIN(ref_id), MAX(ref_id) FROM app_owner.audit_log GROUP BY tbl, act ORDER BY tbl, act;
SELECT a.ref_id, o.customer_id, o.status, o.total FROM app_owner.audit_log a LEFT JOIN app_owner.orders o ON o.id = a.ref_id
ORDER BY a.ref_id;
