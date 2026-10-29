-- run as app_owner after 40_post.sql (both builds): partition expressions for R7 #487 / T3 #489
-- operators numbered after T_EVALUATE_VARIABLE in 11.4 (ASCII, HEX, CONV, INET_ATON)
CREATE TABLE part_ascii (id INT, c VARCHAR(10)) PARTITION BY RANGE (ASCII(c))
  (PARTITION p0 VALUES LESS THAN (77), PARTITION p1 VALUES LESS THAN MAXVALUE);
INSERT INTO part_ascii SELECT n, CHR(65 + MOD(n, 26)) || n FROM seq_n WHERE n <= 1000;

CREATE TABLE part_hex (id INT, c VARCHAR(10)) PARTITION BY LIST (HEX(c))
  (PARTITION p0 VALUES IN ('41'), PARTITION p1 VALUES IN ('42'));
INSERT INTO part_hex SELECT n, ELT(MOD(n, 2) + 1, 'A', 'B') FROM seq_n WHERE n <= 500;

CREATE TABLE part_conv (id INT, c VARCHAR(10)) PARTITION BY HASH (CONV(c, 16, 10)) PARTITIONS 4;
INSERT INTO part_conv SELECT n, HEX(n) FROM seq_n WHERE n <= 1000;

CREATE TABLE part_inet (id INT, ip VARCHAR(15)) PARTITION BY RANGE (INET_ATON(ip))
  (PARTITION p0 VALUES LESS THAN (167772160), PARTITION p1 VALUES LESS THAN MAXVALUE);
INSERT INTO part_inet SELECT n, ELT(MOD(n, 2) + 1, '9.0.0.', '11.0.0.') || MOD(n, 250) FROM seq_n WHERE n <= 1000;

-- CHAR and NUMERIC constants inside the partition expression
CREATE TABLE part_char_const (id INT, c CHAR(4)) PARTITION BY LIST (CONCAT(c, 'x'))
  (PARTITION p0 VALUES IN ('aaaax'), PARTITION p1 VALUES IN ('bbbbx'));
INSERT INTO part_char_const SELECT n, ELT(MOD(n, 2) + 1, 'aaaa', 'bbbb') FROM seq_n WHERE n <= 500;

CREATE TABLE part_num_const (id INT, v INT) PARTITION BY RANGE (CAST(v * 2.50 AS INT))
  (PARTITION p0 VALUES LESS THAN (1000), PARTITION p1 VALUES LESS THAN MAXVALUE);
INSERT INTO part_num_const SELECT n, n FROM seq_n WHERE n <= 1000;

-- 11.4.6 rejects a NUMERIC partition key ("Unsupported partition column type"), so the NUMERIC
-- column rides in a HASH-partitioned table keyed on INT
CREATE TABLE part_num_hash (k INT NOT NULL, amt NUMERIC(12,2), PRIMARY KEY (k)) PARTITION BY HASH (k) PARTITIONS 4;
INSERT INTO part_num_hash SELECT n, MOD(n * 37, 100000) / 100.0 FROM seq_n WHERE n <= 4000;
