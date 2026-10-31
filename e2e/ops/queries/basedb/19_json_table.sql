-- JSON_TABLE over product.spec, joined back and aggregated.
SELECT jt.tag, COUNT(*) FROM app_owner.product p,
  JSON_TABLE(p.spec, '$.tags[*]' COLUMNS (tag VARCHAR(10) PATH '$')) AS jt
WHERE p.id <= 200
GROUP BY jt.tag ORDER BY jt.tag;
