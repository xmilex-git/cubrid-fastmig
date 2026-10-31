-- String functions over VARCHAR, NCHAR and STRING columns (NCHAR only in functions 11.4 accepts: CONCAT_WS rejects it there).
SELECT id, UPPER(name), LOWER(code), LPAD(id, 10, '*'), REPLACE(name, 'Cust', 'C'), CONCAT_WS('/', code, region),
       INSTR(name, ' '), REVERSE(code), CHAR_LENGTH(name_n), CHAR_LENGTH(note), SUBSTRING(note, 1, 5), TRIM(grade)
FROM app_owner.customer
WHERE id IN (1, 2, 25, 100, 999, 5000, 10001, 10002, 10003)
ORDER BY id;
