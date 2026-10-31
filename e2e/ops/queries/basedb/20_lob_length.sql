-- LOB values: lengths and digests of BLOB and CLOB through their conversion functions.
SELECT id, LENGTH(BLOB_TO_BIT(image)), HEX(BLOB_TO_BIT(image)), CHAR_LENGTH(CLOB_TO_CHAR(manual)), MD5(CLOB_TO_CHAR(manual))
FROM app_owner.product WHERE image IS NOT NULL OR manual IS NOT NULL ORDER BY id;
SELECT COUNT(image), COUNT(manual), COUNT(*) FROM app_owner.product;
