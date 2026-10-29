# The init.sh a ported testcase sources (`. $init_path/init.sh`): CTP's own init.sh plus the overrides that keep
# the run inside its namespace job and send the first unloaddb through the switch point.
#
# Input: env from run_case.sh (FASTMIG_TC_*); CTP's init.sh renamed to init_ctp.sh in this directory.
# Output: CTP's functions (init, exec_sql, compare_result_between_files, write_ok, ...) with
#   - pkill/killall/xkill/xkill_pid: no-ops that log (CTP init/finish run `pkill cub`, init.sh:825 and :1751);
#   - cubrid/cub_admin: `server start|stop|restart` through the cubrid-server-control wrapper, `service stop` as
#     "stop this job's servers, then its master", `unloaddb` through the switch point (switch_point.sh).
# The job's pid and net namespaces already stop a kill from reaching another session (workspace#502); these are
# the second guard. Each override turns the testcase's `set -x` off for good: a function's own trace lines would
# otherwise land in the file the testcase redirects the call to and break its expected answer. The calls are
# logged in $FASTMIG_TC_JOB/shim.log instead.

FASTMIG_TC_SHIM_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$FASTMIG_TC_SHIM_DIR/init_ctp.sh"
. "$FASTMIG_TC_SHIM_DIR/switch_point.sh"

# each line also names the databases.txt dir in use and the databases it holds
fastmig_tc_log() {
  echo "[fastmig-tc] $* [dbs=$(basename "$(readlink "$FASTMIG_TC_JOB/dbs")"): $(awk 'NF >= 5 && $1 !~ /^#/ {printf "%s ", $1}' "$FASTMIG_TC_JOB/dbs/databases.txt" 2>/dev/null)]" >>"$FASTMIG_TC_JOB/shim.log"
}

pkill() { { set +x; } 2>/dev/null; fastmig_tc_log "suppressed: pkill $*"; }
killall() { { set +x; } 2>/dev/null; fastmig_tc_log "suppressed: killall $*"; }
xkill() { { set +x; } 2>/dev/null; fastmig_tc_log "suppressed: xkill $*"; }
xkill_pid() { { set +x; } 2>/dev/null; fastmig_tc_log "suppressed: xkill_pid $*"; }

# server start|stop|restart <db> through the hang-proof wrapper; its summary goes to the TC's stdout like cubrid's
fastmig_tc_server() {
  local verb="$1"; shift
  CUBRID_SERVER_CTL_LOGDIR="$FASTMIG_TC_JOB/server-ctl" bash "$FASTMIG_TC_SERVER_CTL" "$verb" "$@"
}

# every server this job runs, then its master (the namespace holds no other master)
fastmig_tc_service_stop() {
  local db
  for db in $("$CUBRID/bin/cubrid" server status 2>/dev/null | sed -n 's/^ *Server \([^ ]*\) .*/\1/p'); do
    fastmig_tc_server stop "$db" >/dev/null
  done
  "$CUBRID/bin/cubrid" service stop >/dev/null 2>&1
}

cubrid() {
  { set +x; } 2>/dev/null
  fastmig_tc_log "cubrid $*"
  case "$1 ${2:-}" in
    "server start" | "server stop" | "server restart") shift; fastmig_tc_server "$@"; return ;;
    "service stop") fastmig_tc_service_stop; return ;;
  esac
  [ "$1" = unloaddb ] && fastmig_tc_switch_point
  command cubrid "$@"
}

cub_admin() {
  { set +x; } 2>/dev/null
  fastmig_tc_log "cub_admin $*"
  [ "$1" = unloaddb ] && fastmig_tc_switch_point
  command cub_admin "$@"
}

export -f fastmig_tc_log pkill killall xkill xkill_pid fastmig_tc_server fastmig_tc_service_stop cubrid cub_admin
