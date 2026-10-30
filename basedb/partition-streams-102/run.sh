#!/usr/bin/env bash
# Builds the partition-expression fixture with the same DDL on 10.2 and on develop, so `cubrid-fastmig
# partition-streams --from 10.2` can compare each transcoded 10.2 stream with the one develop writes (T17 workspace#540).
#   bash run.sh <10.2 install> <develop install> <out dir>
# Both installs are reflink copies (the script writes their conf/cubrid.conf). SA mode only, so no port is taken.
# The two DBs stay under <out dir>/db (x102ps, xdevps, about 300 MB together) for the check and a migration; the caller
# deletes them (`cubrid deletedb` with the matching install, then the directories by literal path).
set -euo pipefail
old="${1:?10.2 install}"; new="${2:?develop install}"; out="${3:?out dir}"
here="$(cd "$(dirname "$0")" && pwd)"
tooling="${TOOLING:-/home/cubrid/dev/workspace}"
[ ! -e "$out/db" ] || { echo "$out/db exists; remove it by literal path first" >&2; exit 1; }
mkdir -p "$out/db" "$out/10.2" "$out/develop"
out="$(cd "$out" && pwd)"
touch "$out/db/databases.txt"

use_102() {   # 10.2 links libncurses.so.5, which Rocky 8 lacks (basedb/build.sh)
  export CUBRID="$old" CUBRID_DATABASES="$out/db" PATH="$old/bin:$PATH"
  export LD_LIBRARY_PATH="$old/lib:$tooling/.git_ignored_dir/legacy-libs/ncurses5/lib64"
  printf '[service]\nservice=server\n[common]\n' > "$old/conf/cubrid.conf"
}
use_develop() {
  export CUBRID="$new" CUBRID_DATABASES="$out/db" PATH="$new/bin:$PATH" LD_LIBRARY_PATH="$new/lib"
  printf '[service]\nservice=server\n[common]\n' > "$new/conf/cubrid.conf"
}
build() {     # $1 db, $2 log dir; csql errors are results (V3 may be refused), read the .out
  mkdir -p "$out/db/$1"
  ( cd "$out/db/$1" && cubrid createdb --db-volume-size=64M --log-volume-size=64M \
      --file-path="$out/db/$1" --log-path="$out/db/$1" "$1" en_US.utf8 ) > "$2/createdb.out" 2>&1
  ( cd "$2" && csql -u dba -S "$1" -i "$here/10_partition_fx_dba.sql" > fx.out 2>&1 ) || true
}

( use_102; build x102ps "$out/10.2" )
( use_develop; build xdevps "$out/develop" )
echo "DONE: $out/db/x102ps/x102ps $out/db/xdevps/xdevps"
