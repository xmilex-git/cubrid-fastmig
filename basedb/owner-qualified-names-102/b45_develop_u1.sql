-- develop, as u1, after GRANT EXECUTE: SP view and trg1_upd
SELECT * FROM v1_sp ORDER BY 1;
INSERT INTO t1 VALUES (2, 'z');
SELECT * FROM u2.t2 ORDER BY 1;
SELECT * FROM v1_join ORDER BY 1;
