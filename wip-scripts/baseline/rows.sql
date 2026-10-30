SELECT 'customer', COUNT(*) FROM [app_owner].[customer];
SELECT 'orders', COUNT(*) FROM [app_owner].[orders];
SELECT 'order_item', COUNT(*) FROM [app_owner].[order_item];
SELECT 'event_log', COUNT(*) FROM [app_owner].[event_log];
SELECT 'audit_log', COUNT(*) FROM [app_owner].[audit_log];
SELECT 'product', COUNT(*) FROM [app_owner].[product];
SELECT 'g5_fx', COUNT(*) FROM [app_owner].[g5_fx];
SELECT 'indexes', COUNT(*) FROM db_index WHERE owner_name = 'APP_OWNER';
SELECT 'triggers', COUNT(*) FROM db_trigger;
