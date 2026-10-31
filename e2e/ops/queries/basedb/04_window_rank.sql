-- Analytic RANK/ROW_NUMBER per partition: the three customers with the highest credit in each region.
SELECT region, rnk, rn, id, credit
FROM (SELECT region, id, credit,
             RANK() OVER (PARTITION BY region ORDER BY credit DESC) AS rnk,
             ROW_NUMBER() OVER (PARTITION BY region ORDER BY credit DESC, id) AS rn
      FROM app_owner.customer WHERE region IS NOT NULL) t
WHERE rn <= 3
ORDER BY region, rn;
