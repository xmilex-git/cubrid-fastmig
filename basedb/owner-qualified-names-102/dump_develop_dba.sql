-- run as dba on develop: owner-qualified names and stored texts after the load
SELECT '#class', unique_name, class_name, owner.name, class_type FROM _db_class WHERE is_system_class = 0 ORDER BY 2;
SELECT '#query_spec', class_of.unique_name, spec FROM _db_query_spec WHERE class_of.is_system_class = 0 ORDER BY 2;
SELECT '#trigger', unique_name, name, owner.name, condition, action_definition FROM _db_trigger ORDER BY 2;
SELECT '#partition', class_of.unique_name, pname, pexpr FROM _db_partition WHERE pexpr IS NOT NULL ORDER BY 2;
SELECT '#serial', unique_name, name, owner.name, class_name, attr_name FROM _db_serial ORDER BY 2;
SELECT '#sp', unique_name, sp_name, owner.name, target_class, target_method FROM _db_stored_procedure ORDER BY 2;
