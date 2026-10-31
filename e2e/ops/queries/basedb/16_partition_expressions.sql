-- Partitions keyed on expressions (ASCII, HEX, CONV, INET_ATON, CONCAT, CAST): pruned predicates on the expression.
SELECT 'ascii', COUNT(*), MIN(id), MAX(id) FROM app_owner.part_ascii WHERE ASCII(c) < 77;
SELECT 'hex', COUNT(*), MIN(id), MAX(id) FROM app_owner.part_hex WHERE HEX(c) = '42';
SELECT 'conv', id, c FROM app_owner.part_conv WHERE CONV(c, 16, 10) IN ('255', '256', '999') ORDER BY id;
SELECT 'inet', COUNT(*), MIN(ip), MAX(ip) FROM app_owner.part_inet WHERE INET_ATON(ip) < 167772160;
SELECT 'char_const', COUNT(*) FROM app_owner.part_char_const WHERE CONCAT(c, 'x') = 'bbbbx';
SELECT 'num_const', COUNT(*), SUM(v) FROM app_owner.part_num_const WHERE CAST(v * 2.50 AS INT) >= 1000;
SELECT 'num_hash', COUNT(*), SUM(amt) FROM app_owner.part_num_hash WHERE k BETWEEN 100 AND 300;
