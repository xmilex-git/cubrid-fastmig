-- JSON functions over the JSON columns.
SELECT JSON_EXTRACT(profile, '$.tier') AS tier, COUNT(*) FROM app_owner.customer WHERE profile IS NOT NULL
GROUP BY JSON_EXTRACT(profile, '$.tier') ORDER BY tier;
SELECT id, spec, JSON_TYPE(spec), JSON_LENGTH(spec), JSON_EXTRACT(spec, '$.tags[1]'), JSON_CONTAINS_PATH(spec, 'one', '$.w'),
       JSON_SET(spec, '$.w', 0), JSON_KEYS(spec)
FROM app_owner.product WHERE id IN (1, 2, 3, 500, 1000) ORDER BY id;
