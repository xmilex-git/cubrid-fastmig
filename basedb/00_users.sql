-- run as dba
CREATE USER app_owner COMMENT 'owner of the app schema';
CREATE USER app_group;
CREATE USER app_reader GROUPS app_group COMMENT 'read-only user';
