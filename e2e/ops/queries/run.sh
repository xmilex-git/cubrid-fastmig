#!/usr/bin/env bash
# Application query parity: one baseline DB taken to the target version by cubrid-fastmig (side "migrated") and by the
# official path (side "official": source unloaddb -> target loaddb), the same read-only query set run on each side, and
# each query's output compared byte for byte between the sides. Interface, query files and output: README.md here.
#
# Input: bash e2e/ops/queries/run.sh <bin> <pristine dir> <target version> <query set: tpch|basedb> <out dir>
# env: FASTMIG_OPS_INSTALL, FASTMIG_OPS_PORT, FASTMIG_OPS_SERVER_CTL (required); FASTMIG_OPS_WORKERS,
#   FASTMIG_OPS_SOURCE_UNLOAD, FASTMIG_OPS_LOB_BASE (as e2e/ops/run.sh, whose side functions sides.sh holds)
# Output: <out>/steps.tsv, <out>/<side>/{result,plan,log}/, <out>/parity/<query>.diff, <out>/queries.tsv,
#   <out>/server-log-summary.tsv. Exit 0 pass, 1 fail, 2 bad arguments.
# Source: T40 workspace#595 (query set, byte comparison, plans recorded only); G12 workspace#587 (1: parity, 6: optdebug).
set -uo pipefail

QDIR=$(dirname "$(realpath "$0")")
. "$QDIR/../sides.sh"
SET=${4:?query set: tpch or basedb}
[ -d "$QDIR/$SET" ] || { echo "no query set $QDIR/$SET" >&2; exit 2; }
ops_setup "$1" "$2" "$3" "${5:?out dir}"

# the queries this source has: a file's "-- min-source: <label>" line names the first source version with its objects
rank() { case $1 in 10.2) echo 0 ;; 11.2) echo 1 ;; 11.3) echo 2 ;; 11.4) echo 3 ;; develop) echo 4 ;; *) echo 9 ;; esac; }
QUERIES=()
for f in "$QDIR/$SET"/*.sql; do
  m=$(sed -n 's/^-- min-source: *//p' "$f" | head -1)
  [ -z "$m" ] || [ "$(rank "$LABEL")" -ge "$(rank "$m")" ] || continue
  QUERIES+=("$f")
done
[ "${#QUERIES[@]}" -gt 0 ] || { echo "no query of $SET for $LABEL" >&2; exit 2; }

# csql options of a query file: its "-- user: <name> [<password>]" line, dba when there is none
query_user() {
  local user pw
  read -r user pw < <(sed -n 's/^-- user: *//p' "$1" | head -1)
  QOPTS=(-C -u "${user:-dba}" -N -t)
  [ -z "${pw:-}" ] || QOPTS+=(-p "$pw")
}
# run_query <file>: its output into <side>/result/<name>.out; fails on a csql exit or an ERROR in the output
run_query() {
  local name out rc
  name=$(basename "$1" .sql)
  query_user "$1"
  out=$(csql "${QOPTS[@]}" -i "$1" "$DB" 2>&1)
  rc=$?
  printf '%s\n' "$out" >"$OUT/$SIDE/result/$name.out"
  [ "$rc" = 0 ] && ! /usr/bin/grep -qE '^[[:space:]]*ERROR' <<<"$out"
}
# run_plan <file>: the simple plan without executing (optimization level 258 = 256 simple plan + 2 do not execute, the
# plan `;plan simple` prints), into <side>/plan/<name>.out. Recorded only: a plan after an index rebuild may differ (T40)
run_plan() {
  local name
  name=$(basename "$1" .sql)
  query_user "$1"
  { echo 'SET OPTIMIZATION LEVEL 258;'; cat "$1"; } >"$OUT/$SIDE/plan/$name.sql"
  csql "${QOPTS[@]}" -i "$OUT/$SIDE/plan/$name.sql" "$DB" >"$OUT/$SIDE/plan/$name.out" 2>&1
  return 0
}

# run_side <side>: prepare, every query and its plan, then the side's DB deleted
run_side() {
  side_begin "$1"
  mkdir -p "$OUT/$SIDE/result" "$OUT/$SIDE/plan"
  step prepare 0 "prepare_$SIDE" || return 1
  step server-start 0 start || return 1
  local f
  for f in "${QUERIES[@]}"; do
    step "q-$(basename "$f" .sql)" 0 run_query "$f"
    step "plan-$(basename "$f" .sql)" 0 run_plan "$f"
  done
  step server-stop 0 stop
  step deletedb 0 cubrid deletedb -d "$DB"
  side_end
}

{ echo "db=$DB label=$LABEL pristine=$PRISTINE target_version=$TV set=$SET queries=${#QUERIES[@]} workers=$W"
  echo "bin_sha256=$(sha256sum "$BIN" | cut -d' ' -f1) install=$INST source_unload=$REF"
  echo "pre: $(date -Is) load=$(cut -d' ' -f1-3 /proc/loadavg) home=[$(df -h /home | awk 'NR == 2 { print $4 " avail" }')]"
} >"$OUT/meta.txt"
printf 'side\tstep\tseconds\trc\n' >"$OUT/steps.tsv"

for s in migrated official; do
  run_side "$s" || { FAIL=1; echo "$s: prepare or first start failed; see $OUT/$s/log" >&2
    [ "$UP" = 0 ] || stop >/dev/null 2>&1; cubrid deletedb -d "$DB" >/dev/null 2>&1; }
  stop_master >"$OUT/$s/log/master-stop.log" 2>&1
done

# parity: every query's output, migrated against official, byte for byte; queries.tsv = name, lines on each side, same
printf 'query\tlines_migrated\tlines_official\tsame\n' >"$OUT/queries.tsv"
for f in "${QUERIES[@]}"; do
  n=$(basename "$f" .sql)
  a=$OUT/migrated/result/$n.out b=$OUT/official/result/$n.out
  cmp -s "$a" "$b"
  rc=$?
  [ "$rc" = 0 ] || diff "$a" "$b" >"$OUT/parity/$n.diff" 2>&1
  printf 'parity\t%s\t0\t%s\n' "$n" "$rc" >>"$OUT/steps.tsv"
  printf '%s\t%s\t%s\t%s\n' "$n" "$(wc -l <"$a" 2>/dev/null)" "$(wc -l <"$b" 2>/dev/null)" "$([ "$rc" = 0 ] && echo yes || echo NO)" >>"$OUT/queries.tsv"
  [ "$rc" = 0 ] || { FAIL=1; echo "parity $n: differs, see parity/$n.diff" >&2; }
done

echo "post: $(date -Is) load=$(cut -d' ' -f1-3 /proc/loadavg) out=$(du -sm "$OUT" | cut -f1) MB fail=$FAIL" >>"$OUT/meta.txt"
echo "left on disk (remove by literal path once read): $OUT"
exit $FAIL
