-- user: app_reader
-- What app_reader sees of the catalog: the user classes it may read and its own grants (grantor not shown: the
-- official path records another grantor for a grant made by a third user, docs/intended-differences.md).
SELECT owner_name, class_name, class_type FROM db_class WHERE is_system_class = 'NO' ORDER BY owner_name, class_name;
SELECT grantee_name, object_name, auth_type, is_grantable FROM db_auth WHERE grantee_name IN ('APP_READER', 'APP_GROUP')
ORDER BY grantee_name, object_name, auth_type;
