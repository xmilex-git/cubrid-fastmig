-- Diagnostic rows next to checks.sql: every customer row, round rows (code R%) without their AUTO_INCREMENT id, in a
-- fixed order. check() writes it to <side>/check/<step>.rows, which parity does not compare; when the customer digest
-- of checks.sql differs between the sides, diff the two .rows files to see the row and column (workspace#589: the
-- digest differed after the DML rounds in 2 of the T37 run-01 rows and did not reproduce).
SELECT CASE WHEN code LIKE 'R%' THEN NULL ELSE id END, code, name, HEX(name_n), HEX(grade), region, credit, score, ratio,
  born, created, tz_at, ltz_at, HEX(flags), profile, note, tags
  FROM app_owner.customer ORDER BY code, 1;
