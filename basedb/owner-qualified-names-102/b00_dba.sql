-- fixture B = A without partitioned tables owned by a non-DBA user (develop loaddb --no-user-specified-name
-- stops at their ALTER CLASS .. PARTITION BY, workspace#516); the partitioned table here is DBA's
CREATE USER u1;
CREATE USER u2;
CREATE TABLE td (k INT PRIMARY KEY, v VARCHAR(20));
INSERT INTO td VALUES (1, 'd1'), (2, 'd2');
GRANT SELECT ON td TO u1;
CREATE TABLE tdp (id INT, d DATE) PARTITION BY RANGE (YEAR(d)) (PARTITION p0 VALUES LESS THAN (2024), PARTITION p1 VALUES LESS THAN MAXVALUE);
INSERT INTO tdp VALUES (1, DATE '2023-05-01'), (2, DATE '2025-05-01');
GRANT SELECT ON tdp TO u1;
