-- run as u1 (source and target): views read, triggers fire
SELECT * FROM v1_t2 ORDER BY 1;
SELECT * FROM v1_join ORDER BY 1;
SELECT * FROM v1_sp ORDER BY 1;
SELECT * FROM v1_sub ORDER BY 1;
SELECT * FROM v1_view ORDER BY 1;
SELECT * FROM v1_part ORDER BY 1;
INSERT INTO t1 VALUES (1, 'x'), (9, 'y');
SELECT * FROM t2 ORDER BY 1;
SELECT * FROM t2_log ORDER BY 1;
SELECT * FROM v1_join ORDER BY 1;
SELECT * FROM v1_sub ORDER BY 1;
