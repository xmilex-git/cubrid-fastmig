#!/usr/bin/env bash
# Builds the TPC-H source DB of the application query parity (SF1 scale, volumes <= 2 GB) with one engine, in the
# baseline layout the query driver and e2e/ops/sides.sh read: <root>/<label>/{sf1, pristine/sf1, unload/sf1, evidence/sf1}.
#
# Input: bash e2e/ops/queries/tpch-db/build.sh <label: 11.4|develop> <install> <port> <server ctl> <root>
#   <install>: the caller's own reflink copy of a release install (conf/cubrid.conf is overwritten); <port>: claimed;
#   <server ctl>: the workspace cubrid-server-control wrapper. env TPCH_SF10_LOAD: the SF10 load files
#   (default /home/cubrid/dev/TPC-H/scale10/load_data). Run under `taskset -c 0-15`.
# Output: the DB tpch<tag>_sf1 (tag 114 | dev), its pristine copy, its unload (the official side's input), evidence/:
#   timings, row counts, volume sizes. <root>/sample/ keeps the sampled load files for the next label.
#   Exit 0 built, 1 a step failed or the volumes exceed 2048 MB, 2 bad arguments or an existing output.
# Source: T40 workspace#595 (sample rule approved by the map supervisor 2026-10-31); the SF10 load procedure
#   /home/cubrid/databases/tpch/load_tpch.sh (schema without keys -> data -> keys, CS loaddb).
set -uo pipefail

label=${1:?label: 11.4 or develop}
inst=$(realpath "${2:?install}")
port=${3:?port}
ctl=${4:?server ctl}
root=$(realpath -m "${5:?root}")
load=${TPCH_SF10_LOAD:-/home/cubrid/dev/TPC-H/scale10/load_data}
here=$(dirname "$(realpath "$0")")
case "$label" in 11.4) tag=114 ;; develop) tag=dev ;; *) echo "label must be 11.4 or develop" >&2; exit 2 ;; esac

# The sample (README.md § TPC-H DB): SF10 has 2,000,000 parts and 15,000,000 orders over keys 1..60,000,000.
# PARTKEY_MAX keeps 15 % of part/partsupp, ORDERKEY_MAX 1/10 of orders (SF1's 1.5 M); lineitem keeps the rows inside
# both, so every lineitem has its order and its partsupp row (supplier, customer, nation, region are kept whole).
# Sized by the first build: 1.17 GB of load files made 2688 MB of volumes (2.29 x, keys included), workspace#595.
PARTKEY_MAX=300000
ORDERKEY_MAX=6000000
CAP_MB=2048   # T40: volumes of the TPC-H DB <= 2 GB

db=tpch${tag}_sf1
base=$root/$label
dbdir=$base/sf1 pristine=$base/pristine/sf1 unload=$base/unload/sf1 ev=$base/evidence/sf1 sample=$root/sample
for p in "$dbdir" "$pristine" "$unload" "$ev"; do
  [ ! -e "$p" ] || { echo "$p exists; remove it by literal path first" >&2; exit 2; }
done
mkdir -p "$dbdir" "$ev" "$base/pristine" "$unload"
touch "$base/databases.txt"
export CUBRID=$inst CUBRID_DATABASES=$base PATH=$inst/bin:$PATH LD_LIBRARY_PATH=$inst/lib
[ "$(command -v cubrid)" = "$inst/bin/cubrid" ] || { echo "cubrid resolves to $(command -v cubrid)" >&2; exit 1; }
cd "$ev" || exit 1   # csql and loaddb leave their .err files in the working directory

t0=$(date +%s)
phase() { echo "$1 rc=$2 at $(( $(date +%s) - t0 ))s" | tee -a "$ev/timings.txt"; [ "$2" = 0 ] || exit 1; }
# a failed step exits with the server possibly up; stop it and this install's master so neither outlives the script
stop_master() {
  local p
  for p in $(ss -ltnp "sport = :$port" | sed -n 's/.*pid=\([0-9]*\).*/\1/p' | sort -u); do
    tr '\0' '\n' <"/proc/$p/environ" 2>/dev/null | grep -qx "CUBRID=$inst" && kill -TERM "$p"
  done
  timeout 30 bash -c "while [ -n \"\$(ss -ltn 'sport = :$port' | tail -n +2)\" ]; do sleep 1; done" || echo "port $port still listening" >&2
}
trap 'rc=$?; [ $rc -eq 0 ] || bash "$ctl" stop "$db" >"$ev/stop.on-error.out" 2>&1; stop_master' EXIT

# 1. the sampled load files, once per root (the loaddb object files have no %class header: loaded with -t <table>)
if [ ! -f "$sample/done" ]; then
  mkdir -p "$sample"
  for t in region nation supplier customer; do cp --reflink=auto "$load/$t.load" "$sample/$t.load" || exit 1; done
  awk -v k="$PARTKEY_MAX" '$1 <= k' "$load/part.load" >"$sample/part.load" &&
    awk -v k="$PARTKEY_MAX" '$1 <= k' "$load/partsupp.load" >"$sample/partsupp.load" &&
    awk -v o="$ORDERKEY_MAX" '$1 <= o' "$load/orders.load" >"$sample/orders.load" &&
    awk -v o="$ORDERKEY_MAX" -v k="$PARTKEY_MAX" '$1 <= o && $2 <= k' "$load/lineitem.load" >"$sample/lineitem.load" || exit 1
  (cd "$sample" && wc -lc ./*.load) >"$sample/sizes.txt" && touch "$sample/done"
fi
phase sample 0
cp "$sample/sizes.txt" "$ev/sample-sizes.txt"

# 2. the DB: one 1 GB volume, the server adds 256 MB volumes; archives are not kept (the load writes GBs of log)
printf '[service]\nservice=server\n[common]\ncubrid_port_id=%s\ndata_buffer_size=2G\nstored_procedure=no\ndb_volume_size=256M\nlog_max_archives=0\nauto_restart_server=no\n' \
  "$port" >"$inst/conf/cubrid.conf"
(cd "$dbdir" && cubrid createdb --db-volume-size=1G --log-volume-size=256M --file-path="$dbdir" --log-path="$dbdir" \
  "$db" en_US.utf8) >"$ev/createdb.out" 2>&1
phase createdb $?
bash "$ctl" start "$db" >"$ev/start.out" 2>&1; grep -q 'RESULT: OK' "$ev/start.out"
phase server-start $?

# 3. schema without keys -> data per table -> keys (the FOREIGN KEYs check the sample's key consistency)
cubrid loaddb -C -u dba -s "$here/schema.sql" "$db" >"$ev/schema.out" 2>&1
phase schema $?
for t in region nation supplier customer part partsupp orders lineitem; do
  cubrid loaddb -C -u dba -d "$sample/$t.load" -t "$t" --no-statistics "$db" >"$ev/data-$t.out" 2>&1 &&
    ! grep -qE '[1-9][0-9]* object\(s\) failed|ERROR|[Ee]rror:' "$ev/data-$t.out"
  phase "data-$t" $?
done
cubrid loaddb -C -u dba -i "$here/indexes.sql" "$db" >"$ev/indexes.out" 2>&1 && ! grep -qE 'ERROR|[Ee]rror:' "$ev/indexes.out"
phase indexes $?
csql -C -u dba -c "UPDATE STATISTICS ON ALL CLASSES WITH FULLSCAN; COMMIT;" "$db" >"$ev/statistics.out" 2>&1
phase statistics $?
csql -C -u dba -t -c "SELECT 'region', COUNT(*) FROM region; SELECT 'nation', COUNT(*) FROM nation;
  SELECT 'supplier', COUNT(*) FROM supplier; SELECT 'customer', COUNT(*) FROM customer; SELECT 'part', COUNT(*) FROM part;
  SELECT 'partsupp', COUNT(*) FROM partsupp; SELECT 'orders', COUNT(*) FROM orders; SELECT 'lineitem', COUNT(*) FROM lineitem;" \
  "$db" >"$ev/rowcounts.txt" 2>&1
phase rowcounts $?

# 4. the unload the official side loads (CS: the DB is not touched), then a clean stop and an SA vacuum
(cd "$unload" && cubrid unloaddb -C -u dba "$db") >"$ev/unloaddb.out" 2>&1
phase unloaddb $?
bash "$ctl" stop "$db" >"$ev/stop.out" 2>&1; grep -q 'RESULT: OK' "$ev/stop.out"
phase server-stop $?
stop_master
cubrid vacuumdb -S "$db" >"$ev/vacuumdb.out" 2>&1
phase vacuumdb $?

# 5. the pristine copy and the size cap (volume files: <db> and <db>_x<NNN>)
cp -a --reflink=always "$dbdir" "$pristine"
phase pristine $?
ls -l "$pristine" >"$ev/pristine-ls.txt"
du -sm "$dbdir" "$pristine" "$unload" >"$ev/sizes.txt"
mb=$(( $(find "$pristine" -maxdepth 1 \( -name "$db" -o -name "${db}_x[0-9]*" \) -printf '%s\n' | awk '{ s += $1 } END { print s + 0 }') / 1048576 ))
echo "volumes ${mb} MB (cap ${CAP_MB} MB)" | tee -a "$ev/timings.txt"
[ "$mb" -le "$CAP_MB" ] || { echo "volume cap exceeded" >&2; exit 1; }
echo "built $db: $pristine, unload $unload/$db"
