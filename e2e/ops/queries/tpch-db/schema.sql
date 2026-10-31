CREATE CLASS [dba].[region] REUSE_OID, COLLATE utf8_bin;

CREATE CLASS [dba].[nation] REUSE_OID, COLLATE utf8_bin;

CREATE CLASS [dba].[part] REUSE_OID, COLLATE utf8_bin;

CREATE CLASS [dba].[partsupp] REUSE_OID, COLLATE utf8_bin;

CREATE CLASS [dba].[lineitem] REUSE_OID, COLLATE utf8_bin;

CREATE CLASS [dba].[supplier] REUSE_OID, COLLATE utf8_bin;

CREATE CLASS [dba].[customer] REUSE_OID, COLLATE utf8_bin;

CREATE CLASS [dba].[orders] REUSE_OID, COLLATE utf8_bin;

ALTER CLASS [dba].[region] ADD ATTRIBUTE
       [r_regionkey] integer NOT NULL,
       [r_name] character(25) COLLATE utf8_bin NOT NULL,
       [r_comment] character varying(152) COLLATE utf8_bin;

ALTER CLASS [dba].[nation] ADD ATTRIBUTE
       [n_nationkey] integer NOT NULL,
       [n_name] character(25) COLLATE utf8_bin NOT NULL,
       [n_regionkey] integer NOT NULL,
       [n_comment] character varying(152) COLLATE utf8_bin;

ALTER CLASS [dba].[part] ADD ATTRIBUTE
       [p_partkey] integer NOT NULL,
       [p_name] character varying(55) COLLATE utf8_bin NOT NULL,
       [p_mfgr] character(25) COLLATE utf8_bin NOT NULL,
       [p_brand] character(10) COLLATE utf8_bin NOT NULL,
       [p_type] character varying(25) COLLATE utf8_bin NOT NULL,
       [p_size] integer NOT NULL,
       [p_container] character(10) COLLATE utf8_bin NOT NULL,
       [p_retailprice] numeric(15,2) NOT NULL,
       [p_comment] character varying(23) COLLATE utf8_bin NOT NULL;

ALTER CLASS [dba].[partsupp] ADD ATTRIBUTE
       [ps_partkey] integer NOT NULL,
       [ps_suppkey] integer NOT NULL,
       [ps_availqty] integer NOT NULL,
       [ps_supplycost] numeric(15,2) NOT NULL,
       [ps_comment] character varying(199) COLLATE utf8_bin NOT NULL;

ALTER CLASS [dba].[lineitem] ADD ATTRIBUTE
       [l_orderkey] integer NOT NULL,
       [l_partkey] integer NOT NULL,
       [l_suppkey] integer NOT NULL,
       [l_linenumber] integer NOT NULL,
       [l_quantity] numeric(15,2) NOT NULL,
       [l_extendedprice] numeric(15,2) NOT NULL,
       [l_discount] numeric(15,2) NOT NULL,
       [l_tax] numeric(15,2) NOT NULL,
       [l_returnflag] character(1) COLLATE utf8_bin NOT NULL,
       [l_linestatus] character(1) COLLATE utf8_bin NOT NULL,
       [l_shipdate] date NOT NULL,
       [l_commitdate] date NOT NULL,
       [l_receiptdate] date NOT NULL,
       [l_shipinstruct] character(25) COLLATE utf8_bin NOT NULL,
       [l_shipmode] character(10) COLLATE utf8_bin NOT NULL,
       [l_comment] character varying(44) COLLATE utf8_bin NOT NULL;

ALTER CLASS [dba].[supplier] ADD ATTRIBUTE
       [s_suppkey] integer NOT NULL,
       [s_name] character(25) COLLATE utf8_bin NOT NULL,
       [s_address] character varying(40) COLLATE utf8_bin NOT NULL,
       [s_nationkey] integer NOT NULL,
       [s_phone] character(15) COLLATE utf8_bin NOT NULL,
       [s_acctbal] numeric(15,2) NOT NULL,
       [s_comment] character varying(101) COLLATE utf8_bin NOT NULL;

ALTER CLASS [dba].[customer] ADD ATTRIBUTE
       [c_custkey] integer NOT NULL,
       [c_name] character varying(25) COLLATE utf8_bin NOT NULL,
       [c_address] character varying(40) COLLATE utf8_bin NOT NULL,
       [c_nationkey] integer NOT NULL,
       [c_phone] character(15) COLLATE utf8_bin NOT NULL,
       [c_acctbal] numeric(15,2) NOT NULL,
       [c_mktsegment] character(10) COLLATE utf8_bin NOT NULL,
       [c_comment] character varying(117) COLLATE utf8_bin NOT NULL;

ALTER CLASS [dba].[orders] ADD ATTRIBUTE
       [o_orderkey] integer NOT NULL,
       [o_custkey] integer NOT NULL,
       [o_orderstatus] character(1) COLLATE utf8_bin NOT NULL,
       [o_totalprice] numeric(15,2) NOT NULL,
       [o_orderdate] date NOT NULL,
       [o_orderpriority] character(15) COLLATE utf8_bin NOT NULL,
       [o_clerk] character(15) COLLATE utf8_bin NOT NULL,
       [o_shippriority] integer NOT NULL,
       [o_comment] character varying(79) COLLATE utf8_bin NOT NULL;

COMMIT WORK;
