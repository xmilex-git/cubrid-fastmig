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

# ha_hosts <databases.txt> <host a> <host b>: sets db-host (column 3) of every line to "a:b", as the manual's HA setup;
# the other columns stay as safe_copy.sh or migrate wrote them, so each node keeps its own LOB base copy
ha_hosts() {
  awk -v h="$2:$3" 'BEGIN { OFS = "\t" } { $3 = h; print }' "$1" >"$1.ha" && mv "$1.ha" "$1"
}
