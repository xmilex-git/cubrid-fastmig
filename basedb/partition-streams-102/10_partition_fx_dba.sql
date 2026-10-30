-- run as dba on 10.2 and on develop (same DDL): partition expressions in the three 10.2 -> develop opcode ranges
-- (R13 workspace#523 § 4.2; T17 workspace#540). DBA-owned, so the official unload/load path could load them too.
CREATE TABLE seq (n INT);
INSERT INTO seq VALUES (1);
INSERT INTO seq SELECT n + 1 FROM seq;
INSERT INTO seq SELECT n + 2 FROM seq;
INSERT INTO seq SELECT n + 4 FROM seq;
INSERT INTO seq SELECT n + 8 FROM seq;
INSERT INTO seq SELECT n + 16 FROM seq;
INSERT INTO seq SELECT n + 32 FROM seq;
INSERT INTO seq SELECT n + 64 FROM seq;
INSERT INTO seq SELECT n + 128 FROM seq;
INSERT INTO seq SELECT n + 256 FROM seq;
INSERT INTO seq SELECT n + 512 FROM seq;

-- +1: T_EXTRACT (10.2 114, which 11.4 numbers T_CASE: no pred offset), T_FLOOR
CREATE TABLE p_extract (id INT, d DATE) PARTITION BY HASH (EXTRACT(YEAR FROM d)) PARTITIONS 4;
INSERT INTO p_extract SELECT n, ADDDATE(DATE '1990-01-01', n * 37) FROM seq;

CREATE TABLE p_floor (id INT, i INT) PARTITION BY RANGE (FLOOR(i / 10))
  (PARTITION p0 VALUES LESS THAN (10), PARTITION p1 VALUES LESS THAN (50), PARTITION p2 VALUES LESS THAN MAXVALUE);
INSERT INTO p_floor SELECT n, n - 200 FROM seq;

-- +5: T_ASCII under HASH
CREATE TABLE p_ascii_hash (id INT, c VARCHAR(10)) PARTITION BY HASH (ASCII(c)) PARTITIONS 4;
INSERT INTO p_ascii_hash SELECT n, CHR(48 + MOD(n, 75)) || n FROM seq;

-- +7: T_NEW_TIME, T_FROM_TZ, T_CONV_TZ (fixed-offset zones)
CREATE TABLE p_new_time (id INT, dt DATETIME) PARTITION BY RANGE (HOUR(NEW_TIME(dt, '+00:00', '+09:00')))
  (PARTITION p0 VALUES LESS THAN (12), PARTITION p1 VALUES LESS THAN MAXVALUE);
INSERT INTO p_new_time SELECT n, ADDDATE(DATETIME '2024-01-01 00:00:00', 0) + CAST(n AS BIGINT) * 3607000 FROM seq;

CREATE TABLE p_from_tz (id INT, dt DATETIME) PARTITION BY HASH (TO_CHAR(FROM_TZ(dt, '+00:00'))) PARTITIONS 4;
INSERT INTO p_from_tz SELECT n, DATETIME '2024-01-01 00:00:00' + CAST(n AS BIGINT) * 3607000 FROM seq;

CREATE TABLE p_conv_tz (id INT, dt DATETIMETZ) PARTITION BY RANGE (HOUR(CAST(dt AS DATETIME)))
  (PARTITION p0 VALUES LESS THAN (12), PARTITION p1 VALUES LESS THAN MAXVALUE);
INSERT INTO p_conv_tz SELECT n, DATETIMETZ '2024-01-01 00:00:00 +09:00' + CAST(n AS BIGINT) * 3607000 FROM seq;

-- CHAR results: 10.2 compiles CONCAT over CHAR to a built-in CHAR, develop to VARCHAR; CONCAT('x', c) ends with
-- the CHAR(4) padding, so trailing-space rules of hash and compare are exercised (workspace#540)
CREATE TABLE p_char_hash (id INT, c CHAR(4)) PARTITION BY HASH (CONCAT('x', c)) PARTITIONS 4;
INSERT INTO p_char_hash SELECT n, ELT(MOD(n, 6) + 1, 'ab', 'ab  ', '', 'a', 'abcd', NULL) FROM seq;
INSERT INTO p_char_hash SELECT n + 2000, SUBSTR(HEX(n), 1, MOD(n, 4) + 1) FROM seq;

CREATE TABLE p_char_list (id INT, c CHAR(4)) PARTITION BY LIST (CONCAT('x', c))
  (PARTITION p0 VALUES IN ('xab  ', 'x    '), PARTITION p1 VALUES IN ('xa   ', 'xabcd'));
INSERT INTO p_char_list SELECT n, ELT(MOD(n, 4) + 1, 'ab', 'ab  ', '', 'a') FROM seq;
INSERT INTO p_char_list VALUES (5000, 'abcd');

-- V6: a nilary operator next to the column (folded or kept); develop refuses this DDL ("Invalid partition definition")
CREATE TABLE p_curdate (id INT, d DATE) PARTITION BY HASH (DATEDIFF(d, CURRENT_DATE)) PARTITIONS 2;

-- V3: a function the 10.2 check does not look at, nested in an allowed one (F_BENCHMARK 963 in 10.2)
CREATE TABLE p_benchmark (id INT, c VARCHAR(10)) PARTITION BY HASH (CONCAT(c, BENCHMARK(1, 1))) PARTITIONS 2;

SELECT class_name, partition_name, partition_class_name FROM db_partition ORDER BY 1, 2;
