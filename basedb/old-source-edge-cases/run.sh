#!/usr/bin/env bash
# One fixture of the old-source edge cases through both paths, each checked on develop (workspace#554):
#   fastmig:  plan -> migrate --verify -> <f>4* checks -> post-migration-fixes.sql -> <f>4* checks again
#   official: source unloaddb -> develop loaddb -u dba (10.2: --no-user-specified-name) -> <f>4* checks
#   bash run.sh <fixture: m|s|t|d> <label: 10.2|11.2|11.3> <source install> <develop install> <cubrid-fastmig bin> <out dir>
# The develop install should be an optdebug build (the checks then double as the assert smoke). Every step runs in
# SA mode, so no port is taken and no conf is written. Each DB is deleted as soon as its step is done; the out dir
# keeps the unload, plan, logs and check outputs.
set -uo pipefail
fx="${1:?fixture m|s|t|d}"; label="${2:?label}"; src="${3:?source install}"; dev="${4:?develop install}"
bin="$(realpath "${5:?cubrid-fastmig bin}")"; out="${6:?out dir}"
here="$(cd "$(dirname "$0")" && pwd)"
tooling="${TOOLING:-/home/cubrid/dev/workspace}"
tag="${label//./}"
[ ! -e "$out/$fx-$tag" ] || { echo "$out/$fx-$tag exists; remove it by literal path first" >&2; exit 1; }
mkdir -p "$out/$fx-$tag"/{source,unload,official,fastmig,db-src,db-off,db-fm}
out="$(cd "$out/$fx-$tag" && pwd)"
sdb="e$tag$fx" odb="o$tag$fx"

use() {       # $1 install, $2 databases dir; 10.2 and 11.2 link libncurses.so.5, which Rocky 8 lacks (basedb/build.sh)
  export CUBRID="$1" CUBRID_DATABASES="$2" PATH="$1/bin:$PATH"
  export LD_LIBRARY_PATH="$1/lib:$tooling/.git_ignored_dir/legacy-libs/ncurses5/lib64"
  touch "$2/databases.txt"
}
create() {    # $1 db, $2 databases dir, $3 log dir
  mkdir -p "$2/$1"
  ( cd "$2/$1" && cubrid createdb --db-volume-size=32M --log-volume-size=32M \
      --file-path="$2/$1" --log-path="$2/$1" "$1" en_US.utf8 ) > "$3/createdb.out" 2>&1
}
as_user() {   # $1 db, $2 sql file, $3 log dir, [$4 output suffix]; the owner is the file name's last word (dba, u1)
  local f; f=$(basename "$2" .sql)
  ( cd "$3" && csql -u "${f##*_}" -S "$1" -i "$2" > "$f${4:-}.out" 2>&1 ) || true   # errors are results here
}
checks() {    # $1 db, $2 log dir, $3 output suffix
  for f in "$here/$fx"4*.sql; do [ -e "$f" ] && as_user "$1" "$f" "$2" "$3"; done
}

# source: build the fixture
use "$src" "$out/db-src"
create "$sdb" "$out/db-src" "$out/source"
csql -u dba -S "$sdb" -c "create user u1; create user u2" > "$out/source/users.out" 2>&1
for f in "$here/$fx"[0-3]*.sql; do as_user "$sdb" "$f" "$out/source"; done

# fastmig path: the source is this run's own DB, so its _vinf already names its own directory
use "$dev" "$out/db-src"
"$bin" plan "$sdb" --dest "$out/fastmig/plan-dest" --target-databases-txt "$out/db-fm/databases.txt" \
  > "$out/fastmig/plan.out" 2>&1
echo "plan rc=$?" >> "$out/fastmig/plan.out"
if tail -1 "$out/fastmig/plan.out" | grep -q 'rc=0'; then
  touch "$out/db-fm/databases.txt"
  "$bin" migrate "$sdb" --dest "$out/db-fm/$sdb" --target-databases-txt "$out/db-fm/databases.txt" --verify \
    --workers 4 --progress none > "$out/fastmig/migrate.out" 2>&1
  echo "migrate rc=$?" >> "$out/fastmig/migrate.out"
  use "$dev" "$out/db-fm"
  if grep -q "^$sdb[[:space:]]" "$out/db-fm/databases.txt"; then
    tgt="$out/db-fm/$sdb"
    for f in recompile-plcsql recompile-views deferred-indexes update-statistics; do
      [ ! -f "$tgt/fastmig/$f.sql" ] || ( cd "$out/fastmig" && csql -S -u dba -i "$tgt/fastmig/$f.sql" "$sdb" > "$f.out" 2>&1 )
    done
    checks "$sdb" "$out/fastmig" ""
    if [ -f "$tgt/fastmig/post-migration-fixes.sql" ]; then
      cp "$tgt/fastmig/post-migration-fixes.sql" "$out/fastmig/"
      ( cd "$out/fastmig" && csql -S -u dba -i "$tgt/fastmig/post-migration-fixes.sql" "$sdb" > fixes.out 2>&1 )
      checks "$sdb" "$out/fastmig" "-after-fixes"
    fi
    cp "$tgt/fastmig/report.json" "$out/fastmig/" 2>/dev/null
    cubrid deletedb "$sdb"
  fi
fi

# the engine's unload after the fastmig path: an unloaddb that crashes leaves the source unclean, which plan refuses
use "$src" "$out/db-src"
( cd "$out/unload" && cubrid unloaddb -S -u dba "$sdb" ) > "$out/unload/unloaddb.out" 2>&1
echo "unloaddb rc=$?" >> "$out/unload/unloaddb.out"

# official path
use "$dev" "$out/db-off"
create "$odb" "$out/db-off" "$out/official"
un="$out/unload/$sdb"
opts=(-S -u dba -s "${un}_schema" -d "${un}_objects")
[ ! -s "${un}_indexes" ] || opts+=(-i "${un}_indexes")
[ ! -s "${un}_trigger" ] || opts+=(--trigger-file "${un}_trigger")
[ "$label" != 10.2 ] || opts+=(--no-user-specified-name)
( cd "$out/official" && cubrid loaddb "${opts[@]}" "$odb" ) > "$out/official/loaddb.out" 2>&1
echo "loaddb rc=$?" >> "$out/official/loaddb.out"
checks "$odb" "$out/official" ""
cubrid deletedb "$odb" && rmdir "$out/db-off/$odb/lob" "$out/db-off/$odb"

use "$src" "$out/db-src"
cubrid deletedb "$sdb" && rmdir "$out/db-src/$sdb/lob" "$out/db-src/$sdb" 2>/dev/null
echo "DONE $fx $label: $out"
grep -h 'rc=' "$out/unload/unloaddb.out" "$out/official/loaddb.out" "$out/fastmig/plan.out" "$out/fastmig/migrate.out" 2>/dev/null
