-- run as dba on 10.2: names and stored texts as the source holds them
SELECT '#class', class_name, owner.name, class_type FROM _db_class WHERE is_system_class = 0 ORDER BY 2;
SELECT '#query_spec', class_of.class_name, spec FROM _db_query_spec WHERE class_of.is_system_class = 0 ORDER BY 2;
SELECT '#trigger', name, owner.name, condition, action_definition FROM db_trigger ORDER BY 2;
SELECT '#trig_target', trigger_name, target_class_name FROM db_trig ORDER BY 2;
SELECT '#partition', class_of.class_name, pname, pexpr FROM _db_partition WHERE pexpr IS NOT NULL ORDER BY 2;
SELECT '#serial', name, owner.name, class_name, att_name FROM db_serial ORDER BY 2;
SELECT '#sp', sp_name, owner.name, target FROM _db_stored_procedure ORDER BY 2;
