#!/usr/bin/env bash
# The engine's official path for a 10.2 source with references across owners, on one fixture:
# build the fixture on 10.2 -> unloaddb -> develop `loaddb -u dba --no-user-specified-name` -> dump the
# owner-qualified names and stored texts -> run the fixture's develop checks as the owners (workspace#516).
#   bash run.sh <fixture: a|b|c> <10.2 install> <develop install> <out dir>
# Both installs are reflink copies (the script writes their conf/cubrid.conf). Everything runs in SA mode, so no
# port is taken. Each DB is deleted as soon as its step is done; the out dir keeps the unload, logs and dumps.
set -euo pipefail
fx="${1:?fixture a|b|c}"; old="${2:?10.2 install}"; new="${3:?develop install}"; out="${4:?out dir}"
here="$(cd "$(dirname "$0")" && pwd)"
tooling="${TOOLING:-/home/cubrid/dev/workspace}"
java_home="${BASEDB_JAVA_HOME:-/usr/lib/jvm/java-1.8.0-openjdk}"
[ ! -e "$out/$fx" ] || { echo "$out/$fx exists; remove it by literal path first" >&2; exit 1; }
mkdir -p "$out/$fx/10.2/unload" "$out/$fx/develop" "$out/$fx/db"
out="$(cd "$out/$fx" && pwd)"
javac -source 8 -target 8 -d "$out" "$here/../FmHello.java"

use_102() {   # 10.2 links libncurses.so.5, which Rocky 8 lacks (basedb/build.sh)
  export CUBRID="$old" CUBRID_DATABASES="$out/db" PATH="$old/bin:$PATH" JAVA_HOME="$java_home"
  export LD_LIBRARY_PATH="$old/lib:$tooling/.git_ignored_dir/legacy-libs/ncurses5/lib64"
  printf '[service]\nservice=server\n[common]\njava_stored_procedure=yes\n' > "$old/conf/cubrid.conf"
}
use_develop() {
  export CUBRID="$new" CUBRID_DATABASES="$out/db" PATH="$new/bin:$PATH" LD_LIBRARY_PATH="$new/lib" JAVA_HOME="$java_home"
  printf '[service]\nservice=server\n[common]\nstored_procedure=yes\n' > "$new/conf/cubrid.conf"
}
create() {    # $1 db, $2 log dir
  mkdir -p "$out/db/$1"; touch "$out/db/databases.txt"
  ( cd "$out/db/$1" && cubrid createdb --db-volume-size=64M --log-volume-size=64M \
      --file-path="$out/db/$1" --log-path="$out/db/$1" "$1" en_US.utf8 ) > "$2/createdb.out" 2>&1
  loadjava -y "$1" "$out/FmHello.class" > "$2/loadjava.out" 2>&1
}
drop() {      # $1 db; deletedb leaves java/, lob/ and (after a develop SP call) java_static/ behind
  cubrid deletedb "$1"
  rm -f "$out/db/$1/java/FmHello.class"
  [ ! -d "$out/db/$1/java_static" ] || rmdir "$out/db/$1/java_static"
  rmdir "$out/db/$1/java" "$out/db/$1/lob" "$out/db/$1"
}
as_user() {   # $1 db, $2 sql file, $3 log dir; the owner is the file name's last word (dba, u1, u2)
  local f; f=$(basename "$2" .sql)
  ( cd "$3" && csql -u "${f##*_}" -S "$1" -i "$2" > "$f.out" 2>&1 ) || true   # errors are results here; read the .out
}

use_102
create "x102$fx" "$out/10.2"
for f in "$here/$fx"[0-3]*.sql; do as_user "x102$fx" "$f" "$out/10.2"; done
as_user "x102$fx" "$here/dump_102_dba.sql" "$out/10.2"
( cd "$out/10.2/unload" && cubrid unloaddb -S -u dba "x102$fx" ) > "$out/10.2/unloaddb.out" 2>&1
drop "x102$fx"

use_develop
create "xdev$fx" "$out/develop"
un="$out/10.2/unload/x102$fx"
set +e
( cd "$out/develop" && cubrid loaddb -S -u dba --no-user-specified-name -s "${un}_schema" \
    --trigger-file "${un}_trigger" -d "${un}_objects" "xdev$fx" ) > "$out/develop/loaddb.out" 2>&1
echo "loaddb rc=$?" >> "$out/develop/loaddb.out"
set -e
as_user "xdev$fx" "$here/dump_develop_dba.sql" "$out/develop"
for f in "$here/$fx"4*.sql; do [ -e "$f" ] && as_user "xdev$fx" "$f" "$out/develop"; done
drop "xdev$fx"
echo "DONE $fx: $out"; tail -1 "$out/develop/loaddb.out"
