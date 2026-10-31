# HA configuration of one node's CUBRID install (workspace#590). Sourced by e2e/ops/ha/run.sh; defines functions only.
# The conf follows the manual's HA quick setup (cubrid-manual ko/ha.rst "데이터베이스 생성 및 서버 설정"): ha_mode=on,
# log_max_archives/force_remove_log_archives so the slave's copied logs are not removed under it, the same
# ha_port_id/ha_node_list/ha_db_list on every node, and databases.txt db-host = the node list.

# ha_conf <install> <db> <host a> <host b>: overwrites the install's cubrid.conf and cubrid_ha.conf (the caller's own
# reflink copy of the install, never a shared one). Ports are the defaults: each node has its own network namespace.
ha_conf() {
  local inst=$1 db=$2 a=$3 b=$4
  printf '%s\n' '[service]' 'service=server' '[common]' 'data_buffer_size=512M' 'stored_procedure=yes' \
    'log_max_archives=100' 'force_remove_log_archives=no' 'ha_mode=on' >"$inst/conf/cubrid.conf"
  printf '%s\n' '[common]' 'ha_port_id=59901' "ha_node_list=fmha@$a:$b" "ha_db_list=$db" \
    'ha_copy_sync_mode=sync:sync' 'ha_apply_max_mem_size=500' >"$inst/conf/cubrid_ha.conf"
}

# ha_hosts <databases.txt> <host a> <host b>: sets db-host (column 3) of every database line (createdb writes a
# "#" header line, left alone) to "a:b", as the manual's HA setup;
# the other columns stay as safe_copy.sh, migrate or createdb wrote them
ha_hosts() {
  awk -v h="$2:$3" 'BEGIN { OFS = "\t" } !/^#/ { $3 = h } { print }' "$1" >"$1.ha" && mv "$1.ha" "$1"
}

# ha_lob_base <databases.txt> <lob-base-path>: sets lob-base-path (column 5) of every database line. CUBRID HA
# replicates LOB locators, not LOB files, and refuses LOB work on a slave or after failover when the storage is
# local (cubrid-manual ha.rst "LOB 칼럼 메타 데이터(Locator)는 복제되고"; run-08: a slave's checks.sql read hit
# "External file ... was not found"), so the target pair shares one LOB storage: the slave takes the master's
# lob-base-path. The source pair keeps one per node: it writes no LOB.
ha_lob_base() {
  awk -v b="$2" 'BEGIN { OFS = "\t" } !/^#/ { $5 = b } { print }' "$1" >"$1.lob" && mv "$1.lob" "$1"
}
