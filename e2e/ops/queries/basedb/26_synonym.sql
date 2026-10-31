-- min-source: 11.2
-- Queries through the two synonyms (app_owner's, and dba's for another owner's table), joined.
SELECT c.region, COUNT(*), SUM(o.total) FROM app_owner.syn_cust c JOIN dba.syn_orders o ON o.customer_id = c.id
WHERE o.ordered_on >= DATE '2025-01-01' GROUP BY c.region ORDER BY c.region;
SELECT id, code, name FROM app_owner.syn_cust WHERE id IN (3, 33, 333, 10003) ORDER BY id;
