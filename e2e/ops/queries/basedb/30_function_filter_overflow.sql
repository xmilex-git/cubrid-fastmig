-- Function and filter index predicates (indexes the migrated side builds after the move), and overflow records.
SELECT id, email, amount FROM app_owner.g5_expr WHERE LOWER(email) = 'user77@example.com';
SELECT COUNT(*), SUM(amount) FROM app_owner.g5_expr WHERE amount BETWEEN 100 AND 300 AND deleted = 0;
SELECT id, name, a, b FROM app_owner.g5_fx WHERE UPPER(name) IN ('NAME10', 'NAME200') ORDER BY id;
SELECT COUNT(*), SUM(c) FROM app_owner.g5_fx WHERE c > 10 AND d IS NOT NULL;
SELECT COUNT(*) FROM app_owner.g5_fx WHERE a = 3 AND LOWER(b) = 'bee3';
SELECT id, LENGTH(body), SUBSTRING(body, 19995, 10), MD5(body) FROM app_owner.doc_big ORDER BY id;
