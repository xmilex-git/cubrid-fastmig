-- run as app_owner
-- override (10.2): a filtered index with the same key as an existing index on a partitioned table is rejected ("Index ... already defined for class ..."); i_f5 gets a second key column
CREATE TABLE seq_n (n INT PRIMARY KEY);

CREATE TABLE customer (
  id INT AUTO_INCREMENT PRIMARY KEY,
  code CHAR(8) NOT NULL UNIQUE,
  name VARCHAR(100) COLLATE utf8_en_ci COMMENT 'case-insensitive name',
  name_n NCHAR VARYING(50),
  grade NCHAR(2),
  region ENUM('N', 'S', 'E', 'W'),
  credit NUMERIC(12,2),
  score DOUBLE,
  ratio FLOAT,
  born DATE,
  created DATETIME,
  tz_at TIMESTAMPTZ,
  ltz_at DATETIMELTZ,
  flags BIT(8),
  tags SET(VARCHAR(10)),
  profile JSON,
  note STRING
) COMMENT 'customers';
CREATE INDEX idx_customer_name ON customer (name);
CREATE INDEX idx_customer_region_credit ON customer (region, credit DESC) COMMENT 'composite, desc key';
CREATE INDEX idx_customer_born_desc ON customer (born DESC);

CREATE TABLE product (
  id INT PRIMARY KEY,
  sku VARCHAR(20) NOT NULL UNIQUE,
  title VARCHAR(200),
  price NUMERIC(10,2),
  weight REAL,
  active SMALLINT,
  spec JSON,
  image BLOB,
  manual CLOB
);

CREATE TABLE orders (
  id BIGINT PRIMARY KEY,
  customer_id INT NOT NULL,
  ordered_on DATE,
  status CHAR(1),
  total NUMERIC(14,2),
  memo VARCHAR(4000),
  CONSTRAINT fk_orders_customer FOREIGN KEY (customer_id) REFERENCES customer (id) ON DELETE CASCADE
);
CREATE INDEX idx_orders_date_status ON orders (ordered_on, status);

CREATE TABLE order_item (
  order_id BIGINT NOT NULL,
  line_no SMALLINT NOT NULL,
  product_id INT NOT NULL,
  qty INT,
  unit_price NUMERIC(10,2),
  PRIMARY KEY (order_id, line_no),
  CONSTRAINT fk_item_order FOREIGN KEY (order_id) REFERENCES orders (id) ON DELETE CASCADE,
  CONSTRAINT fk_item_product FOREIGN KEY (product_id) REFERENCES product (id)
);
CREATE INDEX idx_item_product ON order_item (product_id);

CREATE TABLE invoice (
  inv_no INT PRIMARY KEY,
  order_id BIGINT,
  amount NUMERIC(14,2)
);

CREATE TABLE audit_log (
  id INT AUTO_INCREMENT PRIMARY KEY,
  tbl VARCHAR(30),
  ref_id BIGINT,
  act VARCHAR(10)
);

CREATE TABLE event_log (
  id BIGINT NOT NULL,
  ev_date DATE NOT NULL,
  kind VARCHAR(20),
  payload VARCHAR(600),
  PRIMARY KEY (id, ev_date)
) PARTITION BY RANGE (ev_date) (
  PARTITION p2023 VALUES LESS THAN ('2024-01-01'),
  PARTITION p2024 VALUES LESS THAN ('2025-01-01'),
  PARTITION pmax VALUES LESS THAN MAXVALUE
);
CREATE INDEX idx_event_kind_date ON event_log (kind, ev_date DESC);

CREATE TABLE session_hash (
  user_id INT NOT NULL,
  token CHAR(32) NOT NULL,
  last_seen DATETIME,
  UNIQUE (user_id, token)
) PARTITION BY HASH (user_id) PARTITIONS 4;

CREATE TABLE region_list (
  id INT NOT NULL,
  code VARCHAR(2) NOT NULL,
  label VARCHAR(40)
) PARTITION BY LIST (code) (
  PARTITION p_ns VALUES IN ('N', 'S'),
  PARTITION p_ew VALUES IN ('E', 'W')
);
CREATE INDEX idx_region_list_id ON region_list (id);

-- rows larger than a page: overflow (BIGONE) records
CREATE TABLE doc_big (
  id INT PRIMARY KEY,
  body STRING
);

-- G5 input: function and filter indexes, kept apart from the rest
CREATE TABLE g5_expr (
  id INT PRIMARY KEY,
  email VARCHAR(100),
  amount INT NOT NULL,
  deleted SMALLINT
);
CREATE INDEX idx_g5_lower_email ON g5_expr (LOWER(email));
CREATE INDEX idx_g5_amount_live ON g5_expr (amount) WHERE deleted = 0;

-- G5 fixture (#486): the tool prints the DDL of these instead of building them
CREATE TABLE g5_fx (
  id INT PRIMARY KEY,
  name VARCHAR(50),
  a INT,
  b VARCHAR(50),
  c INT,
  d VARCHAR(20),
  code VARCHAR(20)
);
CREATE INDEX i_f1 ON g5_fx (UPPER(name)) COMMENT 'function index';
CREATE INDEX i_f2 ON g5_fx (a, LOWER(b));
-- i_f3 UNIQUE (LOWER(code)) removed: 11.4.6 rejects a unique function index ("invalid create index")
CREATE INDEX i_f4 ON g5_fx (c) WHERE c > 10 AND d IS NOT NULL;
CREATE INDEX i_f5 ON region_list (id, label) WHERE id > 50;
