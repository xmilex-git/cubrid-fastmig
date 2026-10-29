-- fixture A (cross-owner references), run as dba on 10.2
CREATE USER u1;
CREATE USER u2;
CREATE TABLE td (k INT PRIMARY KEY, v VARCHAR(20));
INSERT INTO td VALUES (1, 'd1'), (2, 'd2');
GRANT SELECT ON td TO u1;
