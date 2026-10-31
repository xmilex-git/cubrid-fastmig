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

round_files

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
printf 'side\terror_lines\tassert_lines\tcores\tpl_drops\tsession_expired\n' >"$OUT/server-log-summary.tsv"
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
