-- 11.4 only, run as app_owner after 40_post.sql: 11.4 DROP TABLE leaves the LOB files behind (orphans, G6 #488)
CREATE TABLE lob_drop (id INT PRIMARY KEY, b BLOB, c CLOB);
INSERT INTO lob_drop (id, b, c)
SELECT n, BIT_TO_BLOB(X'DEADBEEF'), CHAR_TO_CLOB('orphan clob ' || n) FROM seq_n WHERE n <= 5;
DROP TABLE lob_drop;
