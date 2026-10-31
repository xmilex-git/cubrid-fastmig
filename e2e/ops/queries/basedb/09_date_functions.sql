-- Date and time functions over DATE, DATETIME, TIMESTAMPTZ and DATETIMELTZ columns.
SELECT id, born, ADDDATE(born, 30), DATEDIFF(DATE '2026-01-01', born), EXTRACT(YEAR FROM born), LAST_DAY(born),
       TO_CHAR(created, 'YYYY-MM-DD HH24:MI:SS'), TO_CHAR(tz_at, 'YYYY-MM-DD HH24:MI:SS TZR'), ltz_at,
       DAYOFWEEK(born), MONTHS_BETWEEN(DATE '2026-01-01', born)
FROM app_owner.customer
WHERE id IN (1, 2, 25, 100, 999, 5000)
ORDER BY id;
