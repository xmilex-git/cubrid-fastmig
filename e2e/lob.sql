-- LOB contents of a migrated baseline DB (e2e/run.sh step 7): basedb/20_data.sql gives every product row with a LOB
-- the same BLOB and CLOB, so one line counts the rows whose LOB files the engine reads back unchanged. run.sh
-- expects all three counts to equal the number of BLOB locators of the source unload.
SELECT CONCAT('lob-rows ', COUNT(*),
  ' ', SUM(CASE WHEN BLOB_TO_BIT(image) = X'89504E470D0A1A0A0000000D49484452' THEN 1 ELSE 0 END),
  ' ', SUM(CASE WHEN CLOB_TO_CHAR(manual) = REPEAT('manual ', 500) THEN 1 ELSE 0 END), ' end')
FROM app_owner.product WHERE image IS NOT NULL;
