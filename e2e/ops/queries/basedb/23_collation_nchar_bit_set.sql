-- NCHAR sort order, BIT and SET values.
SELECT name_n, grade, HEX(flags), tags FROM app_owner.customer WHERE id <= 12 ORDER BY name_n;
SELECT grade, COUNT(*) FROM app_owner.customer GROUP BY grade ORDER BY grade;
SELECT HEX(flags), COUNT(*), SUM(CASE WHEN tags SUPERSETEQ {'vip'} THEN 1 ELSE 0 END) FROM app_owner.customer GROUP BY HEX(flags) ORDER BY 1;
