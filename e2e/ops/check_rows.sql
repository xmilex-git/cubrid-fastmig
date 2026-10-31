-- Diagnostic rows next to checks.sql, written only with FASTMIG_OPS_ROWS_DUMP=1: the T1 customer rows (ids below
-- 1000001, where basedb/25_rows_multiplier.sql starts, so the 4.2 GB DB dumps no more than T1) and the round rows
-- (code R%) without their AUTO_INCREMENT id, in a fixed order. check() writes it to <side>/check/<step>.rows, which
-- parity does not compare; when the customer digest of checks.sql differs between the sides, diff the two .rows files
-- to see the row and column (workspace#589: the digest differed after the DML rounds in 2 of the T37 run-01 rows and
-- did not reproduce).
SELECT CASE WHEN code LIKE 'R%' THEN NULL ELSE id END, code, name, HEX(name_n), HEX(grade), region, credit, score, ratio,
  born, created, tz_at, ltz_at, HEX(flags), profile, note, tags
  FROM app_owner.customer WHERE code LIKE 'R%' OR id < 1000001 ORDER BY code, 1;
