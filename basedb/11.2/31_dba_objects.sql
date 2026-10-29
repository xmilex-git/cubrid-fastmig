-- run as dba, after loadjava of FmHello.class
-- override (11.2, 11.3): no GRANT EXECUTE ON PROCEDURE before 11.4
CREATE TABLE app_settings (k VARCHAR(50) PRIMARY KEY, v VARCHAR(200)) COMMENT 'dba-owned table';
INSERT INTO app_settings (k, v) SELECT 'key' || n, 'value ' || n FROM app_owner.seq_n WHERE n <= 10;

CREATE TABLE ko_text (id INT PRIMARY KEY, txt VARCHAR(100)) COLLATE utf8_ko_cs;
INSERT INTO ko_text (id, txt) SELECT n, '가나다' || n FROM app_owner.seq_n WHERE n <= 200;
CREATE INDEX idx_ko_text_txt ON ko_text (txt);

CREATE SYNONYM syn_orders FOR app_owner.orders;

CREATE FUNCTION fm_hello (s STRING) RETURN STRING
  AS LANGUAGE JAVA NAME 'FmHello.hello(java.lang.String) return java.lang.String';
