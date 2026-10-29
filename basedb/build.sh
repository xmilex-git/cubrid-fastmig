#!/usr/bin/env bash
# Build one variant of the fastmig reference DB on one install.
#   bash build.sh <label: 11.4|develop> <install> <port> <variant: clean|dirty>
# Host paths come from the environment (defaults are the dev host's):
#   BASEDB_ROOT (/home/cubrid/fastmig-basedb), TOOLING (/home/cubrid/dev/workspace),
#   BASEDB_JAVA_HOME (/usr/lib/jvm/java-1.8.0-openjdk)
# Rows multiplier (GLOSSARY): BASEDB_ROWS_MULTIPLIER (1 = T1 as is; k > 1 runs 25_rows_multiplier.sql),
#   with BASEDB_CAP_MB (5120) raised to fit the bigger DB
# Layout under BASE=$BASEDB_ROOT/<label>/:
#   databases.txt, <variant>/ (registered db), pristine/<variant>/ (copy taken right after the
#   final shutdown), unload/<variant>/ (unloaddb output), evidence/<variant>/ (logs, counts, hashes)
set -euo pipefail
label="${1:?label}"; inst="${2:?install}"; port="${3:?port}"; variant="${4:?clean|dirty}"
scripts="$(cd "$(dirname "$0")" && pwd)"
root="${BASEDB_ROOT:-/home/cubrid/fastmig-basedb}"
tooling="${TOOLING:-/home/cubrid/dev/workspace}"
ctl="$tooling/.claude/skills/cubrid-server-control/scripts/cubrid-server-ctl.sh"
base=$root/$label
case "$label" in 11.4) tag=114 ;; develop) tag=dev ;; *) echo "bad label" >&2; exit 2 ;; esac
db="fm${tag}_${variant}"
dbdir=$base/$variant; pristine=$base/pristine/$variant; unload=$base/unload/$variant; ev=$base/evidence/$variant
cap_mb="${BASEDB_CAP_MB:-5120}"   # du counts the reflinked pristine copy a second time
rows_multiplier="${BASEDB_ROWS_MULTIPLIER:-1}"
[[ "$rows_multiplier" =~ ^[1-9][0-9]*$ ]] || { echo "BASEDB_ROWS_MULTIPLIER must be a positive integer" >&2; exit 2; }
conf_extra=""   # a multiplied load writes GBs of log; archives it no longer needs are not kept
[ "$rows_multiplier" -eq 1 ] || conf_extra="log_max_archives=0"

export CUBRID="$inst" CUBRID_DATABASES="$base" PATH="$inst/bin:$PATH" LD_LIBRARY_PATH="$inst/lib"
export JAVA_HOME="${BASEDB_JAVA_HOME:-/usr/lib/jvm/java-1.8.0-openjdk}"
[ "$(command -v cubrid)" = "$inst/bin/cubrid" ] || { echo "cubrid resolves to $(command -v cubrid)" >&2; exit 1; }
for p in "$dbdir" "$pristine" "$unload" "$ev"; do   # cp -a onto an existing $pristine would nest the copy inside it
  [ ! -e "$p" ] || { echo "$p exists; remove it by literal path first" >&2; exit 1; }
done
mkdir -p "$base" "$dbdir" "$ev" "$base/pristine" "$base/unload"
touch "$base/databases.txt"

write_conf() {   # $1 = extra line or empty
  local f="$ev/cubrid.conf"
  printf '[service]\nservice=server\n[common]\ncubrid_port_id=%s\ndata_buffer_size=512M\nstored_procedure=yes\n%s\n' "$port" "$1" > "$f"
  cp -f "$f" "$inst/conf/cubrid.conf"
}
csql_as() {      # $1 user, $2 file, $3 mode flag (-C|-S)
  csql -u "$1" "$3" "$db" -i "$2" > "$ev/$(basename "$2").$1.out" 2>&1 \
    || { echo "csql $1 $2 failed:"; tail -20 "$ev/$(basename "$2").$1.out"; exit 1; }
  if grep -qE '^ERROR' "$ev/$(basename "$2").$1.out"; then   # "In line N" also heads every successful result block
    echo "csql $1 $2 reported errors:"; grep -E -B3 -A3 '^ERROR' "$ev/$(basename "$2").$1.out" | head -40; exit 1
  fi
}
size_check() {
  local mb; mb=$(du -sm "$root" | cut -f1)
  echo "size: ${mb}M (cap ${cap_mb}M)"
  [ "$mb" -le "$cap_mb" ] || { echo "size cap exceeded" >&2; exit 1; }
}

write_conf "$conf_extra"
( cd "$dbdir" && cubrid createdb --db-volume-size=20M --log-volume-size=64M \
    --file-path="$dbdir" --log-path="$dbdir" "$db" en_US.utf8 ) > "$ev/createdb.out" 2>&1
cubrid addvoldb -S --db-volume-size=64M "$db" > "$ev/addvoldb.out" 2>&1

( cd "$scripts" && javac -source 8 -target 8 -d "$ev" FmHello.java )
loadjava "$db" "$ev/FmHello.class" > "$ev/loadjava.out" 2>&1

bash "$ctl" start "$db" | tee "$ev/start.out" | grep 'RESULT: OK'
csql_as dba "$scripts/00_users.sql" -C
csql_as app_owner "$scripts/10_schema.sql" -C
csql_as app_owner "$scripts/20_data.sql" -C
if [ "$rows_multiplier" -gt 1 ]; then
  sed "s/@ROWS_MULTIPLIER@/$rows_multiplier/g" "$scripts/25_rows_multiplier.sql" > "$ev/25_rows_multiplier.sql"
  csql_as app_owner "$ev/25_rows_multiplier.sql" -C
fi
csql_as app_owner "$scripts/30_objects.sql" -C
csql_as dba "$scripts/31_dba_objects.sql" -C
csql_as app_owner "$scripts/40_post.sql" -C
[ "$label" != 11.4 ] || csql_as app_owner "$scripts/45_orphan_lob.sql" -C   # 11.4 DROP TABLE leaves LOB files (G6 #488)
csql_as app_owner "$scripts/46_partition_fx.sql" -C
bash "$ctl" stop "$db" | tee "$ev/stop1.out" | grep 'RESULT: OK'

# leave no vacuum work behind the load
cubrid vacuumdb -S "$db" > "$ev/vacuumdb.out" 2>&1

if [ "$variant" = dirty ]; then
  write_conf "vacuum_disable=yes"
  bash "$ctl" start "$db" | tee "$ev/start2.out" | grep 'RESULT: OK'
  cubrid paramdump -C "$db" > "$ev/paramdump.out" 2>&1 || true
  csql_as app_owner "$scripts/50_dirty.sql" -C
  bash "$ctl" stop "$db" | tee "$ev/stop2.out" | grep 'RESULT: OK'
  write_conf "$conf_extra"
fi

# pristine copy: the state P1/#485 start from; every SA tool below may write the log or volumes
cp -a --reflink=always "$dbdir" "$pristine"
( cd "$pristine" && find . -type f -print0 | sort -z | xargs -0 sha256sum ) > "$ev/pristine.sha256"

# log header straight from the pristine active log (LOG_HDRPAGE 16 B + LOG_HEADER offsets, r5-min-files)
python3 -I - "$pristine/${db}_lgat" > "$ev/loghdr.txt" <<'PY'
import struct, sys
b = open(sys.argv[1], 'rb').read(4096)
def lsa(off):
    v, = struct.unpack_from('<q', b, off)
    pid = v & ((1 << 48) - 1)
    if pid >> 47: pid -= 1 << 48
    o = (v >> 48) & 0xffff
    if o >> 15: o -= 1 << 16
    return f"{pid}|{o}"
print("magic", b[16:16 + 16])
print("append_lsa", lsa(16 + 112))
print("chkpt_lsa", lsa(16 + 120))
PY

# pending-vacuum evidence, before any other SA boot (each boot runs pending vacuum): a full SA vacuum
# changes the data volumes (_x001, _x002) only when work was pending; volume 0 and the log change on every boot
cubrid vacuumdb -S "$db" > "$ev/vacuum_probe.out" 2>&1
for f in "${db}_x001" "${db}_x002"; do
  a=$(sha256sum < "$pristine/$f"); b=$(sha256sum < "$dbdir/$f")
  [ "$a" = "$b" ] && echo "$f unchanged by vacuum" || echo "$f CHANGED by vacuum"
done > "$ev/vacuum_pending.txt"
printf '1\n-1\n1\n-1\ny\n' | cubrid diagdb -d 8 -o "$ev/diagdb_log.txt" "$db" > "$ev/diagdb.out" 2>&1 || true   # -d 8 asks its page range on stdin
grep -a -m1 -E 'Current_append' "$ev/diagdb_log.txt" >> "$ev/loghdr.txt" || true
csql_as dba "$scripts/counts.sql" -S

mkdir -p "$unload"
( cd "$unload" && cubrid unloaddb -S -u dba "$db" ) > "$ev/unloaddb.out" 2>&1
( cd "$unload" && find . -type f -print0 | sort -z | xargs -0 sha256sum ) > "$ev/unload.sha256"

# put the registered db back to the pristine bytes
rsync -a --delete "$pristine/" "$dbdir/"
( cd "$dbdir" && find . -type f -print0 | sort -z | xargs -0 sha256sum ) | diff - "$ev/pristine.sha256" \
  && echo "registered db == pristine"
find "$pristine/lob" -type f | wc -l > "$ev/lob_files.txt"   # 2 per product row with LOBs (40); 11.4 adds 10 orphans
du -sm "$dbdir" "$pristine" "$unload" > "$ev/sizes.txt"
size_check
echo "DONE $label $variant $db"
