#!/usr/bin/env bash
# HA check of migrated databases (workspace#590, decision G12 workspace#587 (4)): build a source HA pair, sync it, stop
# it, move master and slave each through cubrid-fastmig, configure HA on the target, then replicate, fail over and fail
# back. The tool does not move an HA pair as one; this shows that two separately migrated nodes form a working pair.
#
# Input: bash e2e/ops/ha/run.sh <bin> <pristine dir> <out dir>
#   <bin>           the cubrid-fastmig release binary
#   <pristine dir>  <BASEDB_ROOT>/<label>/pristine/<variant> (basedb/README.md), label 11.4 or develop
#   <out dir>       a new directory under the workspace scratch; both nodes' installs and DBs live here (see README)
# env: FASTMIG_HA_SOURCE_INSTALL (the source-line install, 11.4.6 or develop), FASTMIG_HA_TARGET_INSTALL (develop
#   optdebug, G12 (6)); both are reflink-copied per node, the originals are not touched.
#   FASTMIG_HA_DML (space-separated SQL files of one DML round, run on the current master of the target pair; T36
#   e2e/ops dml/*.sql), FASTMIG_HA_CHECKS (read-only SQL file run on both target nodes after each phase; T36
#   checks.sql), FASTMIG_HA_ROUNDS (default 1), FASTMIG_HA_WORKERS (migrate worker limit, default 4),
#   FASTMIG_HA_SAME_COPY_LOG_PATH=1 (the target pair copies logs where the source pair did, README).
# Output: <out>/steps.tsv ("step seconds rc"), <out>/log/<step>.log, <out>/check/<phase>-<host>.out and
#   <out>/check/<phase>.diff (master against slave), <out>/meta.txt. Exit 0 when every step exited as expected.
# The caller wraps the run in `taskset -c 0-15` (map #472 CPU rule) and removes <out> by its literal path afterwards:
# this script deletes nothing.
set -uo pipefail

BIN=$(realpath "${1:?cubrid-fastmig binary}")
PRISTINE=$(realpath "${2:?pristine dir}")
OUT=${3:?out dir}
SRC_INST=$(realpath "${FASTMIG_HA_SOURCE_INSTALL:?FASTMIG_HA_SOURCE_INSTALL required}")
TGT_INST=$(realpath "${FASTMIG_HA_TARGET_INSTALL:?FASTMIG_HA_TARGET_INSTALL required}")
ROUNDS=${FASTMIG_HA_ROUNDS:-1}
W=${FASTMIG_HA_WORKERS:-4}
# migrated: the source pair moved node by node through cubrid-fastmig; official: the pair the manual's HA upgrade builds
SIDE=${FASTMIG_HA_SIDE:-migrated}
case "$SIDE" in migrated | official) ;; *) echo "FASTMIG_HA_SIDE must be migrated or official" >&2; exit 2 ;; esac
HA=$(dirname "$(realpath "$0")")
BASEDB=$(realpath "$HA/../../../basedb")
. "$HA/pair.sh"
. "$HA/ha_conf.sh"
. "$HA/official_pair.sh"

[ ! -e "$OUT" ] || { echo "$OUT exists" >&2; exit 2; }
mkdir -p "$OUT/log" "$OUT/check"
OUT=$(realpath "$OUT")
vinf=$(ls "$PRISTINE"/*_vinf 2>/dev/null | head -1)
[ -n "$vinf" ] || { echo "no *_vinf in $PRISTINE" >&2; exit 2; }
DB=$(basename "$vinf" _vinf)
RUN_ID=$(date +%Y%m%dT%H%M%S)-$$
{ echo "side=$SIDE db=$DB pristine=$PRISTINE rounds=$ROUNDS workers=$W bin=$BIN"
  echo "bin_sha256=$(sha256sum "$BIN" | cut -d' ' -f1) source_install=$SRC_INST target_install=$TGT_INST"
  echo "source_rel=$(LD_LIBRARY_PATH="$SRC_INST/lib" "$SRC_INST/bin/cubrid_rel" | grep -o 'CUBRID [^ ]* ([^)]*)') target_rel=$(LD_LIBRARY_PATH="$TGT_INST/lib" "$TGT_INST/bin/cubrid_rel" | grep -o 'CUBRID [^ ]* ([^)]*)')"
  echo "pre: $(date -Is) load=$(cut -d' ' -f1-3 /proc/loadavg) home=[$(df -h /home | awk 'NR == 2 { print $4 " avail" }')]"
} >"$OUT/meta.txt"
START_EPOCH=$(date +%s)

FAIL=0
# step <name> <expected rc> <cmd...>: runs and times one step, log in log/<name>.log, a row in steps.tsv
step() {
  local name=$1 want=$2 s e rc; shift 2
  s=$(date +%s.%N)
  "$@" >"$OUT/log/$name.log" 2>&1
  rc=$?
  e=$(date +%s.%N)
  printf '%s\t%.3f\t%s\n' "$name" "$(awk -v a="$s" -v b="$e" 'BEGIN { print b - a }')" "$rc" >>"$OUT/steps.tsv"
  [ "$rc" = "$want" ] || { FAIL=1; echo "step $name: exit $rc (expected $want)" >&2; }
  return "$rc"
}

# per node i: <out>/<host>/{src,tgt}-install (own conf), src (source DB = CUBRID_DATABASES), tgt (target DB), tgt-dbs
H=("${HA_HOSTS[@]}")
nd() { printf '%s/%s' "$OUT" "${H[$1]}"; }
src_env() { printf '%s %s' "$(nd "$1")/src-install" "$(nd "$1")/src"; }
tgt_env() { printf '%s %s' "$(nd "$1")/tgt-install" "$(nd "$1")/tgt-dbs"; }

# hb <i> <src|tgt> <start|stop|status>: cubrid heartbeat on node i (bounded; stdout to the step log, never a pipe)
hb() { local i=$1 e; e=$("${2}_env" "$i"); node_sh "$i" ${e} "timeout -k 10 300 cubrid heartbeat $3"; }

# wait_mode <i> <src|tgt> <active|standby>: polls `cubrid changemode` until node i's server reports that HA mode
wait_mode() {
  local i=$1 e n; e=$("${2}_env" "$i")
  for n in $(seq 60); do
    node_sh "$i" ${e} "cubrid changemode $DB@localhost" | grep -q "current HA running mode is $3" && return 0
    sleep 5
  done
  node_sh "$i" ${e} "cubrid changemode $DB@localhost; cubrid heartbeat status"
  return 1
}

# sql <i> <src|tgt> <file> [csql options]: csql (CS mode, as dba, plain output -t like T36) of one file on node i; exit 1
# when the output holds an ERROR (csql -i goes on after a failed statement and exits 0, workspace#555)
sql() {
  local i=$1 e out rc; e=$("${2}_env" "$i")
  out=$(node_sh "$i" ${e} "csql -u dba -t ${*:4} -i '$3' $DB@localhost" 2>&1); rc=$?
  printf '%s\n' "$out"
  [ "$rc" = 0 ] && ! grep -qE '^[[:space:]]*ERROR' <<<"$out"
}

# sync_mark <master i> <slave i> <src|tgt> <token>: insert a token on the master, wait until the slave applied it.
# fm590_sync has a primary key, so it is replicated (HA replicates only tables with one).
sync_mark() {
  local m=$1 s=$2 v=$3 tok=$4 n e
  printf "INSERT INTO fm590_sync VALUES ('%s');\nCOMMIT;\n" "$tok" >"$OUT/log/sync-$tok.sql"
  sql "$m" "$v" "$OUT/log/sync-$tok.sql" || return 1
  e=$("${v}_env" "$s")
  for n in $(seq 120); do
    node_sh "$s" ${e} "csql -u dba -c \"SELECT token FROM fm590_sync WHERE token = '$tok'\" $DB@localhost" \
      | grep -q "'$tok'" && return 0
    sleep 5
  done
  return 1
}

# checks <phase> <src|tgt>: FASTMIG_HA_CHECKS on both nodes, then master against slave. Tables without a primary key
# are not replicated, so the diff is compared with the official pair's diff (parity), not required to be empty.
checks() {
  local i
  [ -n "${FASTMIG_HA_CHECKS:-}" ] || { echo "FASTMIG_HA_CHECKS unset: no checks"; return 0; }
  # -N: no column headers, as T36 runs checks.sql (217b108)
  for i in 0 1; do sql "$i" "$2" "$FASTMIG_HA_CHECKS" -N >"$OUT/check/$1-${H[i]}.out" || return 1; done
  diff "$OUT/check/$1-${H[0]}.out" "$OUT/check/$1-${H[1]}.out" >"$OUT/check/$1.diff"
  return 0
}

# dml <i> <src|tgt>: FASTMIG_HA_ROUNDS rounds of FASTMIG_HA_DML (space-separated files, run in order) on node i
dml() {
  local r f
  [ -n "${FASTMIG_HA_DML:-}" ] || { echo "FASTMIG_HA_DML unset: no DML"; return 0; }
  for r in $(seq "$ROUNDS"); do for f in $FASTMIG_HA_DML; do echo "round $r $f"; sql "$1" "$2" "$f" || return 1; done; done
}

# wait_pl <i>: the server reports started before its PL server (JVM) accepts calls; checks.sql calls a Java SP, so wait
# up to 60 s for the one every baseline DB has (T36 217b108 `start`)
wait_pl() {
  local e n; e=$(tgt_env "$1")
  for n in $(seq 30); do
    node_sh "$1" ${e} "csql -u dba -t -c \"SELECT dba.fm_hello('ready') FROM db_root\" $DB@localhost" 2>&1 |
      grep -q 'hello ready' && return 0
    sleep 2
  done
  return 1
}

# apply_info <i> <src|tgt>: the db_ha_apply_info rows of node i (migrate empties them on the target, README)
apply_info() {
  local e; e=$("${2}_env" "$1")
  node_sh "$1" ${e} "csql -u dba -c 'SELECT db_name, db_creation_time, copied_log_path, committed_lsa_pageid, committed_lsa_offset, required_lsa_pageid, required_lsa_offset, append_lsa_pageid, eof_lsa_pageid FROM db_ha_apply_info' $DB@localhost"
}

# ---- 1. source pair: stage, start, DML, sync, stop ------------------------------------------------------------------
stage() {
  local i d
  for i in 0 1; do
    d=$(nd "$i"); mkdir -p "$d/tgt-dbs"
    cp -a --reflink=auto "$TGT_INST" "$d/tgt-install" || return 1
    : >"$d/tgt-dbs/databases.txt"
    [ "$SIDE" = migrated ] || continue
    cp -a --reflink=auto "$SRC_INST" "$d/src-install" || return 1
    # both nodes start from the same pristine, as a slave restored from the master's backup would (same db_creation)
    bash "$BASEDB/safe_copy.sh" "$PRISTINE" "$d/src" || return 1
    ha_hosts "$d/src/databases.txt" "${H[0]}" "${H[1]}"
    ha_conf "$d/src-install" "$DB" "${H[0]}" "${H[1]}"
  done
}
# the DML and checks files are copied into the run dir: only it is mounted in the nodes
stage_sql() {
  local f dml=()
  mkdir -p "$OUT/sql"
  for f in ${FASTMIG_HA_DML:-}; do cp "$f" "$OUT/sql/" || return 1; dml+=("$OUT/sql/$(basename "$f")"); done
  FASTMIG_HA_DML="${dml[*]}"
  [ -z "${FASTMIG_HA_CHECKS:-}" ] || { cp "$FASTMIG_HA_CHECKS" "$OUT/sql/" && FASTMIG_HA_CHECKS="$OUT/sql/$(basename "$FASTMIG_HA_CHECKS")"; }
}
step stage 0 stage || exit 1
stage_sql || exit 1
step pair-up 0 pair_up "$RUN_ID" "$OUT" || { pair_down; exit 1; }
trap 'pair_down' EXIT

printf 'CREATE TABLE fm590_sync (token VARCHAR(64) PRIMARY KEY);\nCOMMIT;\n' >"$OUT/log/sync-table.sql"
if [ "$SIDE" = migrated ]; then
# the master stays to_be_active until the slave joins (run-02), so both start before the modes are read
step src-hb-start-master 0 hb 0 src start
step src-hb-start-slave 0 hb 1 src start
step src-mode-master 0 wait_mode 0 src active
step src-mode-slave 0 wait_mode 1 src standby
step src-hb-status 0 hb 0 src status
step src-sync-table 0 sql 0 src "$OUT/log/sync-table.sql"
# The source pair writes only the replicated marker: T36's round.sql and checks.sql are written for the develop
# target and fail on 11.4 (run-03: non-updatable view, numeric coercion), so they run on the target pair only.
step src-sync 0 sync_mark 0 1 src src-end
step src-apply-info 0 apply_info 1 src
# slave first, so the master's last log is copied and applied before it stops; both must end with a normal shutdown
step src-hb-stop-slave 0 hb 1 src stop
step src-hb-stop-master 0 hb 0 src stop
[ "$FAIL" = 0 ] || { echo "source pair failed: see steps.tsv" >&2; exit 1; }

# ---- 2. migrate each node on the host (stopped DBs, same paths as inside the nodes) ---------------------------------
# One node at a time, finalize before the next: at most three DB copies exist at once (both sources and one target).
migrate_node() {
  local i=$1 d; d=$(nd "$i")
  local -a common=(--dest "$d/tgt" --target-version develop --volumes-in-first-dir
    --source-databases-txt "$d/src/databases.txt" --target-databases-txt "$d/tgt-dbs/databases.txt")
  "$BIN" plan "$DB" "${common[@]}" --json >"$OUT/log/plan-${H[i]}.json" || return 1
  "$BIN" migrate "$DB" "${common[@]}" --workers "$W" --verify --progress plain || return 1
  # the SQL migrate leaves for the user (G5), SA in the node under the target install
  local f out; for f in deferred-indexes recompile-plcsql update-statistics; do
    [ -f "$d/tgt/fastmig/$f.sql" ] || continue
    out=$(node_sh "$i" $(tgt_env "$i") "csql -S -u dba -i '$d/tgt/fastmig/$f.sql' $DB" 2>&1) || return 1
    printf '%s\n' "$out"
    ! grep -qE '^[[:space:]]*ERROR' <<<"$out" || return 1
  done
  "$BIN" finalize "$d/tgt"
}
step migrate-master 0 migrate_node 0
step migrate-slave 0 migrate_node 1
# G4: db_creation is kept from the source, so a pair that shared it still shares it; fastmig/source-fingerprint holds it
step db-creation-same 0 bash -c 'diff <(grep "^log" "$1/fastmig/source-fingerprint" | cut -f4) \
  <(grep "^log" "$2/fastmig/source-fingerprint" | cut -f4)' _ "$(nd 0)/tgt" "$(nd 1)/tgt"
[ "$FAIL" = 0 ] || { echo "migration failed: see steps.tsv" >&2; exit 1; }
else
  # the unload the official side loads: the baseline's (basedb/README.md), as T36 does
  LABEL=$(basename "$(dirname "$(dirname "$PRISTINE")")")
  REF=${FASTMIG_HA_SOURCE_UNLOAD:-$(dirname "$(dirname "$PRISTINE")")/unload/$(basename "$PRISTINE")/$DB}
  step official-pair 0 official_pair "$REF" "$LABEL"
  [ "$FAIL" = 0 ] || { echo "official pair failed: see steps.tsv" >&2; exit 1; }
fi

# ---- 3. target pair: configure, start, replicate ---------------------------------------------------------------------
target_conf() {
  local i d
  for i in 0 1; do
    d=$(nd "$i")
    ha_hosts "$d/tgt-dbs/databases.txt" "${H[0]}" "${H[1]}"
    ha_conf "$d/tgt-install" "$DB" "${H[0]}" "${H[1]}"
    [ "$i" = 0 ] || ha_lob_base "$d/tgt-dbs/databases.txt" "$(awk -v db="$DB" '$1 == db { print $5 }' "$(nd 0)/tgt-dbs/databases.txt")"
    [ "$SIDE" = migrated ] && [ "${FASTMIG_HA_SAME_COPY_LOG_PATH:-0}" = 1 ] || continue
    # an operator who keeps CUBRID_DATABASES: the target copies the peer's log to the path the source pair used, so the
    # db_ha_apply_info row migrate carried over matches (db_name, copied_log_path). The source pair's copied logs are
    # moved aside, so the copied log there is the new master's only.
    echo "ha_copy_log_base=$d/src" >>"$d/tgt-install/conf/cubrid_ha.conf"
    mv "$d/src/${DB}_${H[1 - i]}" "$d/src/${DB}_${H[1 - i]}.source-pair" || return 1
  done
}
step tgt-conf 0 target_conf
# the master stays to_be_active until the slave joins (run-02), so both start before the modes are read
step tgt-hb-start-master 0 hb 0 tgt start
step tgt-hb-start-slave 0 hb 1 tgt start
step tgt-mode-master 0 wait_mode 0 tgt active
step tgt-mode-slave 0 wait_mode 1 tgt standby
step tgt-pl-master 0 wait_pl 0
step tgt-pl-slave 0 wait_pl 1
step tgt-hb-status 0 hb 0 tgt status
# the official pair starts without the marker table: created on the master, replicated as DDL
[ "$SIDE" = migrated ] || step tgt-sync-table 0 sql 0 tgt "$OUT/log/sync-table.sql"
step tgt-apply-info-start 0 apply_info 1 tgt
step tgt-sync-start 0 sync_mark 0 1 tgt tgt-start
step tgt-dml 0 dml 0 tgt
step tgt-sync 0 sync_mark 0 1 tgt tgt-dml
step tgt-checks 0 checks target tgt

# ---- 4. failover to the slave, DML there, fail back -----------------------------------------------------------------
step fo-hb-stop-master 0 hb 0 tgt stop
step fo-mode-new-master 0 wait_mode 1 tgt active
step fo-dml 0 dml 1 tgt
step fo-hb-start-old-master 0 hb 0 tgt start
step fo-mode-old-master 0 wait_mode 0 tgt standby
step fo-pl-old-master 0 wait_pl 0
step fo-sync 0 sync_mark 1 0 tgt failover
step fo-checks 0 checks failover tgt

# ---- 5. evidence: server and HA logs, asserts, cores of this run ----------------------------------------------------
evidence() {
  local i d
  for i in 0 1; do
    d=$(nd "$i")
    hb "$i" tgt status
    # only log files this run wrote: the install copies carry older runs' logs
    find "$d/src-install/log" "$d/tgt-install/log" -type f -newermt "@$START_EPOCH" -exec grep -il assert {} + | sed "s|^|assert-in: |"
    find "$d/tgt-install/log/server" -type f -name '*.err' -newermt "@$START_EPOCH" -exec cat {} + | grep -c 'ERROR' \
      | sed "s|^|${H[i]} error lines |"
  done
  # cores are matched to this run's processes by the reader (pid in the name, cubrid-diagnose P13)
  find /data/core/ -maxdepth 1 -name 'core.cub*' -newermt "@$START_EPOCH" -printf 'core: %p\n'
}
step evidence 0 evidence
step tgt-hb-stop-slave 0 hb 1 tgt stop
step tgt-hb-stop-master 0 hb 0 tgt stop
echo "post: $(date -Is) load=$(cut -d' ' -f1-3 /proc/loadavg) home=[$(df -h /home | awk 'NR == 2 { print $4 " avail" }')]" >>"$OUT/meta.txt"
echo "done; remove the DBs by literal path: rm -rf $OUT" >&2
exit "$FAIL"
