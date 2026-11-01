#!/usr/bin/env bash
# The command-line run of workspace#605: help and usage exit codes, the names before #605 refused with their
# replacement, the target conf defaults in plan --json, -i with volumes on two filesystems, -r (replace, refusals) and
# -d (delete the source; kept when the check finds a defect), on a small baseline DB.
#
# Input: bash e2e/cli/run.sh <bin> <pristine dir> <other-device dir> <out dir>
#   <bin>               the cubrid-fastmig release binary
#   <pristine dir>      <BASEDB_ROOT>/develop/pristine/<variant> (a develop source: the target line is the install's)
#   <other-device dir>  a new directory on another filesystem than <out> (the -i volume that must be byte-copied)
#   <out dir>           a new directory on a reflink filesystem; source copies, targets, logs and steps.tsv
# env: FASTMIG_E2E_INSTALL (the develop install, the caller's own copy: its conf/cubrid.conf is overwritten),
#   FASTMIG_E2E_PORT (claimed cubrid_port_id), FASTMIG_E2E_SERVER_CTL (workspace cubrid-server-control wrapper),
#   FASTMIG_E2E_SOURCE_UNLOAD (unloaddb prefix of the source; default the baseline's <label>/unload/<variant>/<db>).
# Output: <out>/steps.tsv ("step rc expected" per step), one log per step under <out>/log/. Exit 0 when every step
#   exited as expected.
# Disk: reflink source copies and cloned targets (one volume byte-copied to <other-device dir>); a registered target
#   is removed with cubrid deletedb, what is left stays in <out> and <other-device dir> for the caller's literal rm -rf.
# Source: workspace#605 acceptance criteria; decision G13 workspace#604; the -d defect is injected by stopping
#   migrate (SIGSTOP) when its verifier phase starts, deleting one target LOB file and continuing: the verifier checks
#   LOB files after its heap scan, so the check sees the file missing (crates/verify/src/run.rs verify_database).
set -uo pipefail

BIN=$(realpath "${1:?cubrid-fastmig binary}")
PRISTINE=$(realpath "${2:?pristine dir}")
OTHER=${3:?other-device dir}
OUT=${4:?out dir}
INST=${FASTMIG_E2E_INSTALL:?FASTMIG_E2E_INSTALL required}
PORT=${FASTMIG_E2E_PORT:?FASTMIG_E2E_PORT required}
CTL=${FASTMIG_E2E_SERVER_CTL:?FASTMIG_E2E_SERVER_CTL required}

[ ! -e "$OUT" ] || { echo "$OUT exists" >&2; exit 2; }
[ ! -e "$OTHER" ] || { echo "$OTHER exists" >&2; exit 2; }
mkdir -p "$OUT/log" "$OUT/src-dbs" "$OUT/dbs" "$OTHER"
OUT=$(realpath "$OUT") OTHER=$(realpath "$OTHER")
[ "$(stat -c %d "$OUT")" != "$(stat -c %d "$OTHER")" ] || { echo "$OTHER is on the filesystem of $OUT" >&2; exit 2; }
cd "$OUT" || exit 2
vinf=$(ls "$PRISTINE"/*_vinf 2>/dev/null | head -1)
DB=$(basename "$vinf" _vinf)
REF=${FASTMIG_E2E_SOURCE_UNLOAD:-$(dirname "$(dirname "$PRISTINE")")/unload/$(basename "$PRISTINE")/$DB}
export CUBRID=$INST CUBRID_DATABASES=$OUT/dbs CUBRID_TMP=$HOME/.fastmig-e2e-tmp
export PATH=$INST/bin:$PATH LD_LIBRARY_PATH=$INST/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
mkdir -p "$CUBRID_TMP"
printf '[service]\nservice=server\n[common]\ncubrid_port_id=%s\ndata_buffer_size=512M\nstored_procedure=yes\n' \
  "$PORT" >"$INST/conf/cubrid.conf"
: >"$OUT/dbs/databases.txt"
# the cub_master this install started (the wrapper does not stop it): SIGTERM by pid, never `cubrid service stop`
stop_master() {
  local p
  for p in $(pgrep -x cub_master); do
    tr '\0' '\n' <"/proc/$p/environ" 2>/dev/null | /usr/bin/grep -qx "CUBRID=$INST" && kill -TERM "$p"
  done
}

FAIL=0
# step <name> <expected rc> <cmd...>: one step, its output in log/<name>.log, a row in steps.tsv
step() {
  local name=$1 want=$2 rc; shift 2
  "$@" >"$OUT/log/$name.log" 2>&1
  rc=$?
  printf '%s\t%s\t%s\n' "$name" "$rc" "$want" >>"$OUT/steps.tsv"
  [ "$rc" = "$want" ] || { FAIL=1; echo "step $name: exit $rc (expected $want)" >&2; }
  return 0
}
# expect <name> <pattern> <file>: the file holds the pattern (a row "name 0|1 0")
expect() {
  local rc=0
  /usr/bin/grep -qE -- "$2" "$3" || rc=1
  printf '%s\t%s\t0\n' "$1" "$rc" >>"$OUT/steps.tsv"
  [ "$rc" = 0 ] || { FAIL=1; echo "expect $1: no /$2/ in $3" >&2; }
}
# source <dir>: a reflink copy of the pristine registered as $DB in src-dbs/databases.txt
source_copy() {
  cp -a --reflink=always "$PRISTINE" "$1" &&
    printf '%s\t%s\tlocalhost\t%s\tfile:%s\n' "$DB" "$1" "$1" "$1/lob" >"$OUT/src-dbs/databases.txt"
}
common=(--volumes-in-first-dir --source-databases-txt "$OUT/src-dbs/databases.txt"
  --target-databases-txt "$OUT/dbs/databases.txt" -t 4)
mig=(--progress none)
# pass <name> <target dir>: the registered target holds what the source holds — the deferred SQL, then the engine
# unload against the source's unload (set comparison, 0 differences)
pass() {
  local f
  for f in recompile-plcsql recompile-views deferred-indexes update-statistics; do
    [ ! -f "$2/fastmig/$f.sql" ] || step "$1-sql-$f" 0 csql -S -u dba -i "$2/fastmig/$f.sql" "$DB"
  done
  mkdir -p "$OUT/unload-$1"
  step "$1-unload" 0 bash -c 'cd "$1" && cubrid unloaddb -S -O "$1" "$2"' _ "$OUT/unload-$1" "$DB"
  step "$1-set-comparison" 0 "$BIN" set-comparison "$REF" "$OUT/unload-$1/$DB" --diff "$OUT/log/$1-set.diff" \
    --source-version develop --target-version develop
}

# 1. help, usage errors, the names before #605
for c in plan migrate check finalize set-comparison; do
  step "help-$c" 0 "$BIN" "$c" -h
  expect "help-$c-text" "^cubrid-fastmig $c " "$OUT/log/help-$c.log"
  step "missing-$c" 2 "$BIN" "$c"
  expect "missing-$c-help" "^cubrid-fastmig $c " "$OUT/log/missing-$c.log"
done
step missing-file-path 2 "$BIN" migrate "$DB"
expect missing-file-path-says "error: -F, --file-path is required" "$OUT/log/missing-file-path.log"
step old-command 2 "$BIN" verify x
expect old-command-says "renamed: use check" "$OUT/log/old-command.log"
for o in "--dest x:-F, --file-path" "--log-dir x:-L, --log-path" "--lob-dir x:-B, --lob-base-path" \
  "--workers 4:-t, --thread-count" "--mem 1G:--temp-mem-size" "--temp-disk min:--temp-disk-size" \
  "--verify:--check" "--target-name y:the second positional argument <new-db>"; do
  n=${o%%:*} n=${n%% *}
  # shellcheck disable=SC2086
  step "old${n}" 2 "$BIN" migrate "$DB" -F "$OUT/x" ${o%%:*}
  expect "old${n}-says" "unknown option $n \(renamed: use ${o#*:}" "$OUT/log/old${n}.log"
done
step equals-form 0 "$BIN" plan -h --file-path="$OUT/x"

# 2. target conf defaults in plan --json (develop pages are 16 KiB)
source_copy "$OUT/src" || exit 1
printf '[common]\ndb_volume_size=256M\n[@%s]\ndata_buffer_size=300M\ntemp_file_max_size_in_pages=1000\n' "$DB" >"$OUT/conf-a"
printf '[common]\ntemp_file_max_size_in_pages=0\n' >"$OUT/conf-b"
step conf-plan-a 0 "$BIN" plan "$DB" -F "$OUT/t1" "${common[@]}" --target-conf "$OUT/conf-a" --json
expect conf-plan-a-values '"settings": \{.*"target_version": "develop".*"db_volume_size": 268435456, "temp_mem_size": 314572800, "temp_disk_size": 16384000' "$OUT/log/conf-plan-a.log"
step conf-plan-b 0 "$BIN" plan "$DB" -F "$OUT/t1" "${common[@]}" --target-conf "$OUT/conf-b" --json
expect conf-plan-b-values '"settings": \{.*"db_volume_size": 536870912, .*"temp_disk_size": "min"' "$OUT/log/conf-plan-b.log"
step conf-plan-cli 0 "$BIN" plan "$DB" -F "$OUT/t1" "${common[@]}" --target-conf "$OUT/conf-a" --json \
  --db-volume-size 128M --temp-mem-size=1G --temp-disk-size max
expect conf-plan-cli-wins '"db_volume_size": 134217728, "temp_mem_size": 1073741824, "temp_disk_size": "max"' "$OUT/log/conf-plan-cli.log"

# 3. -i: volume 0 in -F, volume 1 in another directory of the same filesystem (clone), volume 2 on the other one (copy)
mkdir -p "$OUT/vols-same"
printf '# volid source_fullvolname dest_fullvolname\n1 %s %s\n2 %s %s\n' \
  "$OUT/src/${DB}_x001" "$OUT/vols-same/${DB}_x001" "$OUT/src/${DB}_x002" "$OTHER/${DB}_x002" >"$OUT/control"
step split-plan 0 "$BIN" plan "$DB" -F "$OUT/t1" "${common[@]}" -i "$OUT/control" --json
expect split-plan-methods "\"volid\": 0, [^}]*\"copy_method\": \"clone\"\}, \{\"volid\": 1, [^}]*\"target\": \"$OUT/vols-same/[^}]*\"copy_method\": \"clone\"\}, \{\"volid\": 2, [^}]*\"target\": \"$OTHER/[^}]*\"copy_method\": \"copy\"" "$OUT/log/split-plan.log"
step split-ie-exclusive 2 "$BIN" plan "$DB" -F "$OUT/t1" "${common[@]}" -i "$OUT/control" -E "$OTHER"
step split-migrate 0 "$BIN" migrate "$DB" -F "$OUT/t1" "${common[@]}" "${mig[@]}" -i "$OUT/control" --check
pass split "$OUT/t1"

# 4. -r: the same name without it refuses; with it the target in the same -F is replaced; an unlisted file in -F,
#    or its server up, refuses and leaves the old target registered
step replace-needs-r 1 "$BIN" migrate "$DB" -F "$OUT/t2" "${common[@]}" "${mig[@]}"
expect replace-needs-r-says "pass -r, --replace" "$OUT/log/replace-needs-r.log"
step replace-same-dir 0 "$BIN" migrate "$DB" -F "$OUT/t1" "${common[@]}" "${mig[@]}" -r --check
expect replace-removed-old "--replace: removed $DB" "$OUT/log/replace-same-dir.log"
step replace-old-volumes-gone 1 test -e "$OUT/vols-same/${DB}_x001"
step replace-other-gone 1 test -e "$OTHER/${DB}_x002"
pass replace "$OUT/t1"
echo stray >"$OUT/t1/stray.txt"
step replace-stray 1 "$BIN" migrate "$DB" -F "$OUT/t1" "${common[@]}" "${mig[@]}" -r
expect replace-stray-names "would still hold files .*stray.txt" "$OUT/log/replace-stray.log"
step replace-stray-kept 0 /usr/bin/grep -q "^$DB[[:space:]]" "$OUT/dbs/databases.txt"
rm -f "$OUT/t1/stray.txt"
step replace-server-start 0 bash "$CTL" start "$DB"
step replace-server-up 1 "$BIN" migrate "$DB" -F "$OUT/t1" "${common[@]}" "${mig[@]}" -r
expect replace-server-up-says "cannot be replaced while its server is up" "$OUT/log/replace-server-up.log"
step replace-server-stop 0 bash "$CTL" stop "$DB"
stop_master
step target-deletedb-1 0 cubrid deletedb "$DB"

# 5. -d: with 0 migration defects the source copy goes (as finalize) ...
step delete-source 0 "$BIN" migrate "$DB" -F "$OUT/t3" "${common[@]}" "${mig[@]}" -d
expect delete-source-finalized "^finalize: deleted [0-9]+ paths of $DB" "$OUT/log/delete-source.log"
step delete-source-volume-gone 1 test -e "$OUT/src/$DB"
step delete-source-unregistered 1 /usr/bin/grep -q "^$DB[[:space:]]" "$OUT/src-dbs/databases.txt"
step target-deletedb-2 0 cubrid deletedb "$DB"
# ... and with a defect (a target LOB file deleted while migrate is stopped at its verifier phase) the source stays
source_copy "$OUT/src2" || exit 1
inject() {
  "$BIN" migrate "$DB" -F "$OUT/t4" "${common[@]}" --progress plain --progress-interval 0.05 -d >"$OUT/log/inject-progress.log" 2>&1 &
  local pid=$! lob
  timeout 600 bash -c 'until /usr/bin/grep -qE "phase [0-9]+/[0-9]+ verifier\]" "$1" || ! kill -0 "$2" 2>/dev/null; do sleep 0.02; done' _ \
    "$OUT/log/inject-progress.log" "$pid"
  kill -STOP "$pid"
  lob=$(find "$OUT/t4/lob" -type f | head -1)
  echo "deleted: $lob"
  rm -f "$lob"
  kill -CONT "$pid"
  wait "$pid"
}
step delete-source-defect 1 inject
expect delete-source-defect-found "the check found migration defects" "$OUT/log/inject-progress.log"
step delete-source-kept 0 test -e "$OUT/src2/$DB"
step delete-source-still-registered 0 /usr/bin/grep -q "^$DB[[:space:]]" "$OUT/src-dbs/databases.txt"

echo "left on disk (remove by literal path once read): $OUT $OTHER"
exit $FAIL
