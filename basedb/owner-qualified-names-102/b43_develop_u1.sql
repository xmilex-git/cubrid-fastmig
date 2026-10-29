-- develop, as u1: trg1_upd alone (sp2 without EXECUTE grant)
INSERT INTO t1 VALUES (2, 'z');
SELECT * FROM u2.t2 ORDER BY 1;
