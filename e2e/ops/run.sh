#!/usr/bin/env bash
# Operations scenario runner: one baseline DB taken to the target version by cubrid-fastmig (side "migrated") and by
# the official path (side "official": source unloaddb -> target loaddb), the same scenario run on each side, the checks
# after every step compared between the sides (parity). Interface, steps and output: e2e/ops/README.md.
#
# Input: bash e2e/ops/run.sh <bin> <pristine dir> <target version> <out dir> [rounds]
# env: FASTMIG_OPS_INSTALL, FASTMIG_OPS_PORT, FASTMIG_OPS_SERVER_CTL (required); FASTMIG_OPS_ROUNDS,
#   FASTMIG_OPS_MIN_ARCHIVES, FASTMIG_OPS_WORKERS, FASTMIG_OPS_SOURCE_UNLOAD, FASTMIG_OPS_LOB_BASE, FASTMIG_OPS_BREAK
# Output: <out>/steps.tsv, <out>/<side>/{log,check}/, <out>/parity/, <out>/server-log-summary.tsv, <out>/archives.tsv.
#   Exit 0 pass, 1 fail, 2 bad arguments.
# Source: G12 workspace#587 (decisions 1-6), T36 workspace#588; e2e/run.sh (step, sql_file, migrate and deferred SQL);
#   the side functions live in sides.sh (shared with queries/run.sh, workspace#595).
set -uo pipefail

OPS=$(dirname "$(realpath "$0")")
. "$OPS/sides.sh"
ROUNDS=${5:-${FASTMIG_OPS_ROUNDS:-6}}   # 6: README § Rounds
MIN_ARCHIVES=${FASTMIG_OPS_MIN_ARCHIVES:-2}
[[ "$ROUNDS" =~ ^[1-9][0-9]*$ ]] || { echo "rounds must be a positive integer" >&2; exit 2; }
case "${FASTMIG_OPS_BREAK:-}" in "" | official-extra-row) ;; *) echo "unknown FASTMIG_OPS_BREAK" >&2; exit 2 ;; esac
ops_setup "$@"

# the DML round of this source: synonyms from 11.2, PL/CSQL from 11.4 (basedb per-label overrides)
ROUND_FILES=("$OPS/dml/round.sql")
case "$LABEL" in 11.2 | 11.3 | 11.4 | develop) ROUND_FILES+=("$OPS/dml/round_synonym.sql") ;; esac
case "$LABEL" in 11.4 | develop) ROUND_FILES+=("$OPS/dml/round_plcsql.sql") ;; esac

# checks.sql after a scenario step into <side>/check/<step>.out; fails on an error or an index that differs from its heap.
# Always in CS mode (the Java SP of checks.sql runs in the server's PL process): a stopped server is started around it.
check() {
  local name=$1 f=$OUT/$SIDE/check/$1.out was=$UP rc
  [ "$was" = 1 ] || start || return 1
  sql_file -C "$OPS/checks.sql" -N >"$f" && ! grep -q "INDEX!=HEAP" "$f"
  rc=$?
  [ "$was" = 1 ] || stop || return 1
  return $rc
}
scenario_step() {   # scenario_step <name> <cmd...>: the step, then its checks as step check-<name>
  local name=$1; shift
  step "$name" 0 "$@"
  step "check-$name" 0 check "$name"
}


kill9_restart() {   # kill -9 of this side's server, then a start: the restart runs crash recovery
  local p; p=$(server_pid)
  [ -n "$p" ] || { echo "no cub_server of $DB"; return 1; }
  echo "kill -9 $p"; kill -9 "$p"
  timeout 60 bash -c "while kill -0 $p 2>/dev/null; do sleep 1; done" || return 1
  # what else of this side still runs (the server's cub_pl outlives it): listed, then killed as a crash would leave it
  local q
  for q in $(pgrep -x cub_pl); do
    tr '\0' '\n' <"/proc/$q/environ" 2>/dev/null | grep -qx "CUBRID_DATABASES=$CUBRID_DATABASES" || continue
    echo "left after the kill: $q $(cat /proc/$q/comm), kill -9"; kill -9 "$q"
    timeout 60 bash -c "while kill -0 $q 2>/dev/null; do sleep 1; done" || return 1
  done
  UP=0; start
}
dml_rounds() {   # dml_rounds <n>: n DML rounds, every file of this source
  local i f
  for ((i = 1; i <= $1; i++)); do
    for f in "${ROUND_FILES[@]}"; do echo "round $i $(basename "$f")"; sql_file -C "$f" || return 1; done
  done
}
last_archive() {   # number of the newest log archive of this side, -1 when none
  ls "$SIDE_DB" 2>/dev/null | sed -n "s/^${DB}_lgar\([0-9][0-9]*\)\$/\1/p" | sort -n | tail -1 | awk '{ n = $1 + 0 } END { print NR ? n : -1 }'
}


# run_side <side>: prepare, then the scenario steps with their checks; leaves the side's DB deleted
run_side() {
  side_begin "$1"
  step prepare 0 "prepare_$SIDE" || return 1
  step server-start 0 start || return 1
  if [ "$SIDE" = official ] && [ "${FASTMIG_OPS_BREAK:-}" = official-extra-row ]; then
    step break-official-extra-row 0 csql -C -u dba -c "INSERT INTO app_owner.region_list (id, code, label) VALUES (100001, 'N', 'break')" "$DB"
  fi
  step check-baseline 0 check baseline
  local a0; a0=$(last_archive)
  scenario_step dml dml_rounds "$ROUNDS"
  scenario_step statistics sql_file -C "$OPS/statistics.sql"
  scenario_step checkpoint sql_file -C "$OPS/checkpoint.sql" --sysadm
  scenario_step kill9-recovery kill9_restart
  step server-stop 0 stop
  scenario_step vacuumdb cubrid vacuumdb -S "$DB"
  scenario_step addvoldb cubrid addvoldb -S --db-volume-size=32M "$DB"
  scenario_step compactdb cubrid compactdb -S "$DB"
  scenario_step backupdb cubrid backupdb -S -D "$OUT/$SIDE/backup" "$DB"
  step server-start-2 0 start
  scenario_step dml-after-backup dml_rounds 1
  step server-stop-2 0 stop
  printf '%s\t%s\t%s\n' "$SIDE" "$a0" "$(last_archive)" >>"$OUT/archives.tsv"
  scenario_step restoredb cubrid restoredb -B "$OUT/$SIDE/backup" "$DB"
  step deletedb 0 cubrid deletedb -d "$DB"
  side_end
}

{ echo "db=$DB label=$LABEL pristine=$PRISTINE target_version=$TV rounds=$ROUNDS workers=$W break=${FASTMIG_OPS_BREAK:-}"
  echo "bin_sha256=$(sha256sum "$BIN" | cut -d' ' -f1) install=$INST source_unload=$REF"
  echo "pre: $(date -Is) load=$(cut -d' ' -f1-3 /proc/loadavg) home=[$(df -h /home | awk 'NR == 2 { print $4 " avail" }')]"
} >"$OUT/meta.txt"
printf 'side\tstep\tseconds\trc\n' >"$OUT/steps.tsv"
printf 'side\terror_lines\tassert_lines\tcores\tpl_drops\n' >"$OUT/server-log-summary.tsv"
printf 'side\tlast_archive_before\tlast_archive_after\n' >"$OUT/archives.tsv"

for s in migrated official; do
  run_side "$s" || { FAIL=1; echo "$s: prepare or first start failed; see $OUT/$s/log" >&2
    [ "$UP" = 0 ] || stop >/dev/null 2>&1; cubrid deletedb -d "$DB" >/dev/null 2>&1; }
  stop_master >"$OUT/$s/log/master-stop.log" 2>&1
done

# parity: the checks of every step, migrated against official
for f in "$OUT"/migrated/check/*.out; do
  n=$(basename "$f" .out)
  diff "$f" "$OUT/official/check/$n.out" >"$OUT/parity/$n.diff" 2>&1
  rc=$?
  printf 'parity\t%s\t0\t%s\n' "$n" "$rc" >>"$OUT/steps.tsv"
  [ "$rc" = 0 ] || { FAIL=1; echo "parity $n: differs, see parity/$n.diff" >&2; }
done
[ -n "$(ls "$OUT"/migrated/check/*.out 2>/dev/null)" ] || FAIL=1

# the migrated side must have written enough log to archive it at least MIN_ARCHIVES times
created=$(awk -F'\t' '$1 == "migrated" { print $3 - $2 }' "$OUT/archives.tsv")
echo "archives created on migrated: ${created:-none} (min $MIN_ARCHIVES)" >>"$OUT/meta.txt"
[ "${created:--1}" -ge "$MIN_ARCHIVES" ] || { FAIL=1; echo "migrated side created ${created:-no} log archives, fewer than $MIN_ARCHIVES" >&2; }

echo "post: $(date -Is) load=$(cut -d' ' -f1-3 /proc/loadavg) out=$(du -sm "$OUT" | cut -f1) MB fail=$FAIL" >>"$OUT/meta.txt"
echo "left on disk (remove by literal path once read): $OUT"
exit $FAIL
