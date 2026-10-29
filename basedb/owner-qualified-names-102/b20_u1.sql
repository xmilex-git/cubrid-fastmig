-- fixture B, run as u1: views and triggers whose text names u2's table, serial and SP and dba's table
CREATE TABLE t1 (a INT, b VARCHAR(40));
CREATE TABLE t1_auto_increment_with_a_rather_long_table_name_for_the_ai_serial_x (
  id_column_with_a_long_name_for_the_auto_increment_serial INT AUTO_INCREMENT PRIMARY KEY, v INT);
INSERT INTO t1_auto_increment_with_a_rather_long_table_name_for_the_ai_serial_x (v) VALUES (1), (2);
CREATE VIEW v1_t2 AS SELECT a, b FROM t2 WHERE a > 0;
CREATE VIEW v1_join AS SELECT t1.a, t2.b, td.v FROM t1, t2, td WHERE t1.a = t2.a AND td.k = t1.a;
CREATE VIEW v1_sp AS SELECT a, sp2(b) AS h FROM t2;
CREATE VIEW v1_sub AS SELECT a FROM t1 WHERE a IN (SELECT a FROM t2);
CREATE VIEW v1_view AS SELECT a, b FROM v1_t2;
CREATE VIEW v1_part AS SELECT p.id, p.d, t1.b FROM tdp p, t1 WHERE p.id = t1.a;
CREATE TRIGGER trg1_ai AFTER INSERT ON t1
  EXECUTE INSERT INTO t2_log (msg) VALUES ('t1 ' || CAST(s2.NEXT_VALUE AS VARCHAR(20)));
CREATE TRIGGER trg1_upd AFTER INSERT ON t1 IF obj.a IN (SELECT a FROM t2) AND obj.a <= (SELECT COUNT(*) FROM td)
  EXECUTE UPDATE t2 SET b = sp2(t2.b) WHERE t2.a = obj.a;
