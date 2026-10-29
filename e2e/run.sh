#!/usr/bin/env bash
# One end-to-end run of a baseline DB through cubrid-fastmig, every step timed (workspace#484).
#
# Input: bash e2e/run.sh <bin> <pristine dir> <target version> <out dir>
#   <bin>             the cubrid-fastmig release binary
#   <pristine dir>    <BASEDB_ROOT>/<label>/pristine/<variant>: a pristine copy of a baseline DB (basedb/README.md)
#   <target version>  --target-version of plan and migrate (develop; 11.4 is refused by the plan)
#   <out dir>         a new directory; it holds the source copy, the target, logs and steps.tsv
# env: FASTMIG_E2E_INSTALL (the target-line CUBRID install, the caller's own copy: its conf/cubrid.conf is
#   overwritten), FASTMIG_E2E_PORT (claimed cubrid_port_id), FASTMIG_E2E_SERVER_CTL (workspace cubrid-server-control
#   wrapper), FASTMIG_E2E_WORKERS (worker limit, default 4), FASTMIG_E2E_CPUS (taskset CPU list for migrate and verify,
#   default: none), FASTMIG_E2E_CHECKDB=1 (engine checkdb -S on the target, #484 (5): small 11.4 -> develop only),
#   FASTMIG_E2E_BREAKER (the #484 (6) probe binary: breaks the finalized target, expects verify exit 1),
#   FASTMIG_E2E_SOURCE_UNLOAD (unloaddb prefix of the source; default the baseline's <label>/unload/<variant>/<db>),
#   FASTMIG_E2E_LOB_BASE (lob-base-path the source copy is registered with; default <out>/source/lob. A 10.2..11.3
#   baseline's locators are absolute under the base it was built in, so give that base to run its LOB path; finalize
#   is then not run, it would delete that base's files, workspace#529).
# Output: <out>/steps.tsv ("step seconds rc" per step), one log per step under <out>/log/, the migrate phases from
#   report.json in <out>/phases.tsv. Exit 0 when every step that ran exited as expected, else 1.
# Steps: reflink copy -> plan -> migrate -> verify target/source -> deferred SQL -> target unloaddb -> set comparison
#   -> engine smoke (CS server, SELECTs, index scan plans) -> [checkdb] -> finalize -> [broken target] -> target deletedb.
#   A refusing plan ends the run after the copy (exit 0 when the refusal is the expected outcome is the reader's call).
# Source: workspace#484 (checks (1)-(10)), task of the map supervisor 2026-10-29 (step order); G1 workspace#481,
#   G5 workspace#486 (deferred indexes before the set comparison), ADR 0007 (verifier + set comparison).
set -uo pipefail

BIN=$(realpath "${1:?cubrid-fastmig binary}")
PRISTINE=$(realpath "${2:?pristine dir}")
TV=${3:?target version}
OUT=${4:?out dir}
INST=${FASTMIG_E2E_INSTALL:?FASTMIG_E2E_INSTALL required}
PORT=${FASTMIG_E2E_PORT:?FASTMIG_E2E_PORT required}
CTL=${FASTMIG_E2E_SERVER_CTL:?FASTMIG_E2E_SERVER_CTL required}
W=${FASTMIG_E2E_WORKERS:-4}
E2E=$(dirname "$(realpath "$0")")

[ ! -e "$OUT" ] || { echo "$OUT exists" >&2; exit 2; }
mkdir -p "$OUT/log" "$OUT/src-dbs" "$OUT/dbs" "$OUT/unload"
OUT=$(realpath "$OUT")
cd "$OUT" || exit 2   # csql and unloaddb leave csql.err and logs in the cwd
vinf=$(ls "$PRISTINE"/*_vinf 2>/dev/null | head -1)
[ -n "$vinf" ] || { echo "no *_vinf in $PRISTINE" >&2; exit 2; }
DB=$(basename "$vinf" _vinf)
SRC=$OUT/source TGT=$OUT/target
REF=${FASTMIG_E2E_SOURCE_UNLOAD:-$(dirname "$(dirname "$PRISTINE")")/unload/$(basename "$PRISTINE")/$DB}
T=()
[ -z "${FASTMIG_E2E_CPUS:-}" ] || T=(taskset -c "$FASTMIG_E2E_CPUS")

# the target install's environment; CUBRID_TMP is short (unix socket paths) and private to these runs
export CUBRID=$INST CUBRID_DATABASES=$OUT/dbs CUBRID_TMP=$HOME/.fastmig-e2e-tmp
export PATH=$INST/bin:$PATH LD_LIBRARY_PATH=$INST/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
mkdir -p "$CUBRID_TMP"
printf '[service]\nservice=server\n[common]\ncubrid_port_id=%s\ndata_buffer_size=512M\nstored_procedure=yes\n' \
  "$PORT" >"$INST/conf/cubrid.conf"

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

{ echo "db=$DB pristine=$PRISTINE target_version=$TV workers=$W cpus=${FASTMIG_E2E_CPUS:-} bin=$BIN"
  echo "bin_sha256=$(sha256sum "$BIN" | cut -d' ' -f1) install=$INST"
  echo "pre: $(date -Is) load=$(cut -d' ' -f1-3 /proc/loadavg) home=[$(df -h /home | awk 'NR == 2 { print $4 " avail" }')]"
  [ -z "${FASTMIG_E2E_CPUS:-}" ] || bash "$E2E/quiet_gate.sh" "$FASTMIG_E2E_CPUS"
} >"$OUT/meta.txt"

# 1. reflink copy of the pristine; its volume headers still name the registered DB, so no engine utility ever
#    opens it (basedb/README.md): fastmig reads it with --volumes-in-first-dir
step copy 0 cp -a --reflink=always "$PRISTINE" "$SRC" || exit 1
printf '%s\t%s\tlocalhost\t%s\tfile:%s\n' "$DB" "$SRC" "$SRC" "${FASTMIG_E2E_LOB_BASE:-$SRC/lob}" >"$OUT/src-dbs/databases.txt"
: >"$OUT/dbs/databases.txt"
# a lob-base outside the copy belongs to the registered baseline: its file list and contents must not change (workspace#529)
lob_digest() { (cd "$1" && find . -type f -print0 | sort -z | xargs -0 -r sha256sum); }
[ -z "${FASTMIG_E2E_LOB_BASE:-}" ] || step lob-base-before 0 lob_digest "$FASTMIG_E2E_LOB_BASE"
find "$SRC" -maxdepth 1 -type f ! -name '*_lgar*' -exec cat {} + >/dev/null   # warm, as the baseline runs
common=(--dest "$TGT" --target-version "$TV" --volumes-in-first-dir
  --source-databases-txt "$OUT/src-dbs/databases.txt" --target-databases-txt "$OUT/dbs/databases.txt")

# 2. plan: exit 1 = refused, the run ends here
step plan 0 "$BIN" plan "$DB" "${common[@]}" --json
cp "$OUT/log/plan.log" "$OUT/plan.json"
[ "$(awk '$1 == "plan" { print $3 }' "$OUT/steps.tsv")" = 0 ] || { echo "plan refused: see log/plan.log" >&2; exit 1; }

# 3. migrate --verify (verify is opt-in, ADR 0015: verifies at its end and registers only a target with 0 migration defects)
step migrate 0 "${T[@]}" "$BIN" migrate "$DB" "${common[@]}" --workers "$W" --verify --progress plain
[ -f "$TGT/fastmig/report.json" ] && cp "$TGT/fastmig/report.json" "$OUT/report.json"

# 4. verify the target against the source, and the source alone
step verify-target 0 "${T[@]}" "$BIN" verify "$TGT/$DB" --source "$SRC/$DB" --workers "$W" \
  --report "$OUT/verify-target.jsonl"
step verify-source 0 "${T[@]}" "$BIN" verify "$SRC/$DB" --role source --workers "$W" --report "$OUT/verify-source.jsonl"
registered=0
grep -q "^$DB[[:space:]]" "$OUT/dbs/databases.txt" && registered=1

if [ $registered = 1 ]; then
  # 5. the SQL migrate leaves for the user: deferred indexes before the set comparison (G5)
  for f in deferred-indexes recompile-plcsql update-statistics; do
    [ ! -f "$TGT/fastmig/$f.sql" ] || step "sql-$f" 0 csql -S -u dba -i "$TGT/fastmig/$f.sql" "$DB"
  done
  # 6. target unloaddb, set comparison against the source's engine unload
  step target-unload 0 bash -c 'cd "$1" && cubrid unloaddb -S -O "$1" "$2"' _ "$OUT/unload" "$DB"
  # the two engine lines come from the plan, so the pair's unloaddb print differences are normalized (workspace#502)
  pair=$(sed -n 's/.*"version_pair": {"source": "\([^"]*\)", "target": "\([^"]*\)"}.*/\1 \2/p' "$OUT/plan.json")
  step set-comparison 0 "$BIN" set-comparison "$REF" "$OUT/unload/$DB" --diff "$OUT/set-comparison.diff" \
    --source-version "${pair% *}" --target-version "${pair#* }"
  # 7. engine smoke: CS server on the target, SELECTs and their plans
  step server-start 0 bash "$CTL" start "$DB"
  step smoke-sql 0 csql -C -u dba -i "$E2E/smoke.sql" "$DB"
  # LOB contents read through the engine: rows, BLOB equal, CLOB equal = the source's BLOB locator count
  step lob-sql 0 csql -C -u dba -i "$E2E/lob.sql" "$DB"
  nlob=$(awk '/^%class \[app_owner\]\.\[product\]/ { p = 1; next } /^%class/ { p = 0 } p' "${REF}_objects" | grep -c "'B[0-9]*|file:")
  step lob-contents 0 grep -q "lob-rows $nlob $nlob $nlob end" "$OUT/log/lob-sql.log"
  step server-stop 0 bash "$CTL" stop "$DB"
  cubrid service stop >"$OUT/log/service-stop.log" 2>&1
  # every referenced LOB file of the manifest (last column: its target path) is still there after the engine booted
  # and shut down the target
  # (develop removes top-level ces_* dirs of the lob-base at boot as temporary, CBRD-23700)
  [ ! -s "$TGT/fastmig/lob-manifest" ] || step lob-after-boot 0 awk -F'\t' -v d="$TGT/lob" \
    '{ if (system("test -f \"" d "/" $NF "\"")) { print "missing " $NF; n++ } } END { print NR - n " of " NR " present"; exit n > 0 }' \
    "$TGT/fastmig/lob-manifest"
  # every smoke query has an index scan in its plan and none errored
  step smoke-plans 0 bash -c '! grep -q "^ERROR" "$1" && [ "$(grep -c "^ *Index scan(" "$1")" -ge "$2" ]' _ \
    "$OUT/log/smoke-sql.log" "$(grep -c '^SELECT' "$E2E/smoke.sql")"
  # 8. engine checkdb on the small 11.4 -> develop target only (#484 (5))
  [ "${FASTMIG_E2E_CHECKDB:-0}" != 1 ] || step checkdb 0 cubrid checkdb -S "$DB"
fi

# 9. finalize: deletes the source copy's volumes, logs, LOB files and Java dirs; refused unless verify passed.
#    Guard first: every path the source fingerprint names lies in this run's source copy.
fp=$TGT/fastmig/source-fingerprint
if [ $registered = 1 ] && [ -n "${FASTMIG_E2E_LOB_BASE:-}" ]; then
  echo "finalize skipped (lob-base outside the copy): finalize would delete the files of $FASTMIG_E2E_LOB_BASE" >"$OUT/log/finalize.log"
  printf 'finalize\t0.000\tskipped\n' >>"$OUT/steps.tsv"
elif [ $registered = 1 ] && [ -f "$fp" ] && ! awk -F'\t' -v s="$SRC/" '
    $1 == "volume" { if (index($3, s) != 1) bad = 1 }
    $1 == "log" || $1 == "vol-path" || $1 == "log-path" || $1 == "lob-base" { if (index($2 "/", s) != 1) bad = 1 }
    END { exit !bad }' "$fp"; then
  step finalize 0 "$BIN" finalize "$TGT"
  cp "$TGT/fastmig/finalize.json" "$OUT/" 2>/dev/null
elif [ $registered = 1 ]; then
  echo "finalize skipped: the source fingerprint names a path outside $SRC" >&2; FAIL=1
fi

# 10. fail-before (#484 (6), (8)): break the target cumulatively; verify must exit 1 after each break
if [ $registered = 1 ] && [ -n "${FASTMIG_E2E_BREAKER:-}" ]; then
  B=$FASTMIG_E2E_BREAKER
  brk() { local name=$1; shift; step "break-$name" 0 "$@" && step "verify-broken-$name" 1 "$BIN" verify "$TGT/$DB" \
    --workers "$W" --report "$OUT/verify-broken-$name.jsonl"; }
  brk leaf-next "$B" leaf-next "$TGT/$DB" "$TGT/${DB}_lgat" app_owner.orders pk_orders_id
  brk drop-entry "$B" drop-entry "$TGT/$DB" "$TGT/${DB}_lgat" app_owner.customer idx_customer_name
  brk drop-row "$B" drop-row "$TGT/$DB" "$TGT/${DB}_lgat" app_owner.customer
  lob=$(find "$TGT/lob" -type f 2>/dev/null | head -1)
  echo "lob-missing break: ${lob:-no LOB file under $TGT/lob}" >>"$OUT/meta.txt"
  [ -z "$lob" ] || brk lob-missing rm -f "$lob"
fi

# 11. delete the target (registered: engine deletedb; not registered: left for the caller's literal rm -rf)
if [ $registered = 1 ]; then
  step target-deletedb 0 cubrid deletedb "$DB"
fi
if [ -n "${FASTMIG_E2E_LOB_BASE:-}" ]; then
  cp "$OUT/log/lob-base-before.log" "$OUT/lob-base-before.sha256"
  step lob-base-unchanged 0 diff "$OUT/lob-base-before.sha256" <(lob_digest "$FASTMIG_E2E_LOB_BASE")
fi
# migrate's own phase timings: the "seconds" object of report.json, one "phase seconds" row each
grep -o '"seconds": {[^}]*}' "$OUT/report.json" 2>/dev/null | grep -o '"[a-z_]*": [0-9][0-9.]*' | tr -d '"' |
  awk -F': ' '{ printf "%s\t%s\n", $1, $2 }' >"$OUT/phases.tsv"
echo "post: $(date -Is) load=$(cut -d' ' -f1-3 /proc/loadavg) out=$(du -sm "$OUT" | cut -f1) MB" >>"$OUT/meta.txt"
echo "left on disk (remove by literal path once read): $OUT"
exit $FAIL
