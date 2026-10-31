-- LIST partition pruning on code, with the filter index predicate (id > 50).
SELECT id, code, label FROM app_owner.region_list WHERE code IN ('E', 'W') AND id > 50 ORDER BY id;
SELECT code, COUNT(*) FROM app_owner.region_list WHERE code = 'S' GROUP BY code;
