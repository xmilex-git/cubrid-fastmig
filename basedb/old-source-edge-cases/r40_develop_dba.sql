-- old-source-edge-cases, fixture r: the develop checks as DBA (workspace#554).
select class_name, attr_name, data_type, domain_class_name from db_attribute where class_name in ('t_cls', 't_auths') order by 1, 2;
select id, c is null from t_cls;
