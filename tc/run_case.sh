#!/usr/bin/env bash
# Runs one ported testcase in its own namespace job and writes its verdict.
#
# usage: run_case.sh <job dir> <testcase dir> <install> <cubrid-fastmig binary> <ctp dir> <server-ctl> <switch 0|1>
#   <testcase dir>  the testcase's directory in a cubrid-testcases-private-ex checkout (its cases/ is run)
#   <install>       the CUBRID install; it is the source and the target (same-version path, workspace#502)
#   <ctp dir>       a cubrid-testtools CTP directory (its shell/init_path/init.sh is sourced through tc/shim)
#   <server-ctl>    the workspace's cubrid-server-control wrapper (cubrid-server-ctl.sh)
#   <switch>        1 = the first unloaddb goes through the switch point; 0 = a baseline run of the testcase as is
# Output in <job dir>: work/ (the testcase run; its <case>.result), switch.tsv and switch/ (switch_point.sh),
# shim.log, job.log, internal-errors.txt, cores.txt, verdict (one line, see verdict()).
# The job runs in user, mount, ipc, pid and net namespaces (as workspace tools/fast-gate/fast-cs-csql.sh): CTP's
# `pkill cub` and the testcase's own server and port use reach nothing outside it; the install is an overlay.
# Nothing is removed with rm here: the testcase deletes its databases, a clean job deletes what is still
# registered with `cubrid deletedb` inside the namespace, and the run dir is removed by its literal path afterwards.
set -u

SELF="$(readlink -f "${BASH_SOURCE[0]}")"
TC_DIR="$(dirname "$SELF")"

if [ "${1:-}" = "--job" ]; then
  J="$2" INST="$3" CASE_SH="$4"
  /usr/sbin/ip link set lo up || exit 90
  h="$(hostname)"
  { printf '127.0.0.1\t%s\n' "$h"; /usr/bin/grep -v -w -F "$h" /etc/hosts; } >"$J/hosts" || exit 90
  mount --bind "$J/hosts" /etc/hosts || exit 90
  mount -t tmpfs -o size=256m,mode=1777 tmpfs /dev/shm || exit 90
  mount -t overlay overlay -o "lowerdir=$INST,upperdir=$J/up-install,workdir=$J/work-install,volatile,userxattr" \
    "$INST" || exit 91
  RP="$(readelf -d "$INST/bin/cub_server" | sed -n 's/.*(RPATH).*\[\(.*\)\]/\1/p')"
  { [ -z "$RP" ] || [ "$RP" = "$INST/lib" ] || [ ! -d "$RP" ]; } || mount --bind "$INST/lib" "$RP" || exit 91
  # the parameters ctp-run pins on every CTP server
  printf '%s\n' data_buffer_size=512M parallelism=4 max_parallel_workers=100 >>"$INST/conf/cubrid.conf" || exit 91
  mount --bind "$J/t" /srv || exit 90
  export HOME=/home CUBRID="$INST" CUBRID_TMP=/srv OS=Linux CUBRID_CHARSET=en_US
  export CUBRID_DATABASES="$J/dbs"
  export PATH="$INST/bin:$PATH" LD_LIBRARY_PATH="$INST/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  export init_path="$J/CTP/shell/init_path"
  cd "$J/work" || exit 90
  sh "$CASE_SH" </dev/null
  rc=$?
  cat "$INST"/log/server/*.err 2>/dev/null | /usr/bin/grep -i 'internal error' >"$J/internal-errors.txt"
  for dbs in "$FASTMIG_TC_SOURCE_DBS" "$FASTMIG_TC_TARGET_DBS"; do
    for db in $(CUBRID_DATABASES="$dbs" "$INST/bin/cubrid" server status 2>/dev/null | sed -n 's/^ *Server \([^ ]*\) .*/\1/p'); do
      CUBRID_DATABASES="$dbs" timeout -k 10 60s "$INST/bin/cubrid" server stop "$db" </dev/null >>"$J/job.log" 2>&1
    done
  done
  timeout -k 5 30s "$INST/bin/cubrid" service stop </dev/null >>"$J/job.log" 2>&1
  # a clean run deletes what is still registered; a failed one keeps its databases for analysis
  result="$J/work/${CASE_SH#./}"; result="${result%.sh}.result"
  if ! /usr/bin/grep -q ' : NOK' "$result" 2>/dev/null \
    && [ "$(awk -F'\t' '$2 != 0 || $3 != 0 || $4 != 0 || $5 != 0 || $6 != 0 || $7 != 0' "$J/switch.tsv" | wc -l)" -eq 0 ]; then
    for dbs in "$FASTMIG_TC_SOURCE_DBS" "$FASTMIG_TC_TARGET_DBS"; do
      for db in $(awk 'NF >= 5 && $1 !~ /^#/ {print $1}' "$dbs/databases.txt" 2>/dev/null); do
        CUBRID_DATABASES="$dbs" "$INST/bin/cubrid" deletedb "$db" </dev/null >>"$J/cleanup.log" 2>&1
      done
    done
  fi
  exit $rc
fi

[ $# -eq 7 ] || { sed -n '3,9p' "$SELF"; exit 2; }
J="$1" CASE_DIR="$2" INST="$(readlink -f "$3")" BIN="$(readlink -f "$4")" CTP="$(readlink -f "$5")"
SERVER_CTL="$(readlink -f "$6")" SWITCH="$7"
# CTP runs cases/<testcase dir name>.sh; a few testcases keep helper scripts beside it
CASE_SH="./$(basename "$CASE_DIR").sh"
[ -f "$CASE_DIR/cases/$CASE_SH" ] || CASE_SH="$(cd "$CASE_DIR/cases" 2>/dev/null && ls ./*.sh 2>/dev/null | head -1)"
[ -n "$CASE_SH" ] || { echo "no cases/*.sh in $CASE_DIR"; exit 2; }
[ -e "$J" ] && { echo "$J exists (never reused)"; exit 2; }
mkdir -p "$J"/{up-install,work-install,t,db-source,db-target,switch} || exit 2
ln -s "$J/db-source" "$J/dbs" || exit 2
cp -a --reflink=auto "$CASE_DIR/cases" "$J/work" || exit 2
cp -a --reflink=auto "$CTP" "$J/CTP" || exit 2
mv "$J/CTP/shell/init_path/init.sh" "$J/CTP/shell/init_path/init_ctp.sh" || exit 2
cp "$TC_DIR/shim/init.sh" "$TC_DIR/shim/switch_point.sh" "$J/CTP/shell/init_path/" || exit 2
: >"$J/switch.tsv"

export FASTMIG_TC_JOB="$J" FASTMIG_TC_BIN="$BIN" FASTMIG_TC_SWITCH="$SWITCH" FASTMIG_TC_SERVER_CTL="$SERVER_CTL"
export FASTMIG_TC_SOURCE_DBS="$J/db-source" FASTMIG_TC_TARGET_DBS="$J/db-target"
touch "$J/start-marker"
t0=$(date +%s)
timeout -k 30 "${FASTMIG_TC_TIMEOUT:-720}"s unshare -Urmipnf --mount-proc --kill-child \
  bash "$SELF" --job "$J" "$INST" "$CASE_SH" >"$J/job.log" 2>&1
rc=$?
elapsed=$(( $(date +%s) - t0 ))
# cores land in the host core_pattern dir; list the engine's ones written during this job (pid match by the lead)
find /data/core -maxdepth 1 -newer "$J/start-marker" \( -name 'core.cub*' -o -name 'core.csql*' \) >"$J/cores.txt" 2>/dev/null

result="$J/work/${CASE_SH#./}"; result="${result%.sh}.result"
ok=$(/usr/bin/grep -c ' : OK' "$result" 2>/dev/null); ok=${ok:-0}
nok=$(/usr/bin/grep -c ' : NOK' "$result" 2>/dev/null); nok=${nok:-0}
switched=$(wc -l <"$J/switch.tsv")
switch_bad=$(awk -F'\t' '$2 != 0 || $3 != 0 || $4 != 0 || $5 != 0 || $6 != 0 || $7 != 0' "$J/switch.tsv" | wc -l)
cores=$(wc -l <"$J/cores.txt")
internal=$(wc -l <"$J/internal-errors.txt" 2>/dev/null || echo 0)
# migration defects by code over every switched database, e.g. oids-not-ascending:10 (- when none)
verify_codes=$(cat "$J"/switch/*/verify.jsonl 2>/dev/null | /usr/bin/grep -o "\"origin\":\"migration\".*\"code\":\"[a-z-]*\"" \
  | sed "s/.*\"code\":\"//; s/\"$//" | sort | uniq -c | awk "{printf \"%s%s:%s\", (NR > 1 ? \",\" : \"\"), \$2, \$1}")
verdict=PASS
[ "$ok" -ge 1 ] && [ "$nok" -eq 0 ] || verdict=FAIL
[ "$SWITCH" = 0 ] || { [ "$switched" -ge 1 ] && [ "$switch_bad" -eq 0 ]; } || verdict=FAIL
[ "$cores" -eq 0 ] && [ "$internal" -eq 0 ] || verdict=FAIL
[ "$rc" -ne 124 ] && [ "$rc" -ne 137 ] || verdict=FAIL

printf '%s\tswitch=%s\t%s\tok=%s nok=%s switched_dbs=%s switch_bad=%s cores=%s internal_errors=%s verify_codes=%s rc=%s elapsed_s=%s\n' \
  "$(basename "$CASE_DIR")" "$SWITCH" "$verdict" "$ok" "$nok" "$switched" "$switch_bad" "$cores" "$internal" \
  "${verify_codes:--}" "$rc" "$elapsed" | tee "$J/verdict"
