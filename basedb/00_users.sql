-- run as dba
CREATE USER app_owner COMMENT 'owner of the app schema';
CREATE USER app_group;
-- a password: develop's unloaddb salts it anew on every unload, the case of set-comparison rule password-hash (workspace#582)
CREATE USER app_reader PASSWORD 'fm_reader' GROUPS app_group COMMENT 'read-only user';
