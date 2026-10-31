-- HASH partition pruning on user_id, and the rows of each partition.
SELECT user_id, token, last_seen FROM app_owner.session_hash WHERE user_id = 77 ORDER BY token;
SELECT 'p0', COUNT(*) FROM app_owner.session_hash__p__p0;
SELECT 'p1', COUNT(*) FROM app_owner.session_hash__p__p1;
SELECT 'p2', COUNT(*) FROM app_owner.session_hash__p__p2;
SELECT 'p3', COUNT(*) FROM app_owner.session_hash__p__p3;
