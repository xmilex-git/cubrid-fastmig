#!/usr/bin/env bash
# Build one variant of the fastmig reference DB on one install.
#   bash build.sh <label: 10.2|11.0|11.2|11.3|11.4|develop> <install> <port> <variant: clean|dirty>
# A file <label>/<name>.sql next to this script replaces <name>.sql for that label (releases before
#   11.4 reject some statements of the common set; each override says what it drops or rewrites, workspace#512)
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
old=0   # 1 = release line before 11.4
case "$label" in
  11.4) tag=114 ;; develop) tag=dev ;;
  10.2) tag=102; old=1 ;; 11.0) tag=110; old=1 ;; 11.2) tag=112; old=1 ;; 11.3) tag=113; old=1 ;;
  *) echo "bad label" >&2; exit 2 ;;
esac
db="fm${tag}_${variant}"
dbdir=$base/$variant; pristine=$base/pristine/$variant; unload=$base/unload/$variant; ev=$base/evidence/$variant
cap_mb="${BASEDB_CAP_MB:-5120}"   # per label; du counts the reflinked pristine copy a second time
rows_multiplier="${BASEDB_ROWS_MULTIPLIER:-1}"
[[ "$rows_multiplier" =~ ^[1-9][0-9]*$ ]] || { echo "BASEDB_ROWS_MULTIPLIER must be a positive integer" >&2; exit 2; }
conf_extra=""   # a multiplied load writes GBs of log; archives it no longer needs are not kept
[ "$rows_multiplier" -eq 1 ] || conf_extra="log_max_archives=0"

export CUBRID="$inst" CUBRID_DATABASES="$base" PATH="$inst/bin:$PATH" LD_LIBRARY_PATH="$inst/lib"
# 10.2, 11.0 and 11.2 link libncurses.so.5, which Rocky 8 lacks (workspace cubrid-diagnose P3: nix build of the el8 RPM)
case "$label" in 10.2|11.0|11.2) export LD_LIBRARY_PATH="$inst/lib:$tooling/.git_ignored_dir/legacy-libs/ncurses5/lib64" ;; esac
export JAVA_HOME="${BASEDB_JAVA_HOME:-/usr/lib/jvm/java-1.8.0-openjdk}"
[ "$(command -v cubrid)" = "$inst/bin/cubrid" ] || { echo "cubrid resolves to $(command -v cubrid)" >&2; exit 1; }
for p in "$dbdir" "$pristine" "$unload" "$ev"; do   # cp -a onto an existing $pristine would nest the copy inside it
  [ ! -e "$p" ] || { echo "$p exists; remove it by literal path first" >&2; exit 1; }
done
# a failed step (csql error, size cap) exits with the server still up; stop it so no server outlives the script
trap 'rc=$?; [ $rc -eq 0 ] || bash "$ctl" stop "$db" > "$ev/stop.on-error.out" 2>&1 || true' EXIT
mkdir -p "$base" "$dbdir" "$ev" "$base/pristine" "$base/unload"
touch "$base/databases.txt"

sql() {   # $1 = file name; the label's override when there is one
  if [ -f "$scripts/$label/$1" ]; then echo "$scripts/$label/$1"; else echo "$scripts/$1"; fi
}
write_conf() {   # $1 = extra line or empty
  local f="$ev/cubrid.conf" sp=stored_procedure   # the Java SP switch is java_stored_procedure before 11.4
  [ "$old" = 0 ] || sp=java_stored_procedure
  printf '[service]\nservice=server\n[common]\ncubrid_port_id=%s\ndata_buffer_size=512M\n%s=yes\n%s\n' "$port" "$sp" "$1" > "$f"
  cp -f "$f" "$inst/conf/cubrid.conf"
}
dirty_conf() {   # keeps vacuum from running while 50_dirty.sql runs
  # 10.2 and 11.0 die on the first write with vacuum_disable=yes (SIGFPE in vacuum_get_log_blockid: log_block_npages 0);
  # a 20 s vacuum master interval leaves the same pending vacuum, and the stop waits at most one interval
  if [ "$label" = 10.2 ] || [ "$label" = 11.0 ]; then echo vacuum_master_interval_in_msecs=20000; else echo vacuum_disable=yes; fi
}
stop_master() {   # `server stop` leaves cub_master listening; the next label's server must not register with this one
  local p
  for p in $(ss -ltnp "sport = :$port" | sed -n 's/.*pid=\([0-9]*\).*/\1/p' | sort -u); do
    if tr '\0' '\n' < "/proc/$p/environ" | awk -v want="CUBRID=$inst" '$0 == want { f = 1 } END { exit !f }'; then kill -TERM "$p"; fi
  done
  timeout 30 bash -c "while [ -n \"\$(ss -ltn 'sport = :$port' | tail -n +2)\" ]; do sleep 1; done" || echo "port $port still listening" >&2
}
javasp_ctl() {   # $1 = start|stop; 11.0 does not start the Java SP server with the DB server (11.2+ does, 10.2 has the JVM inside cub_server)
  [ "$label" = 11.0 ] || return 0
  cubrid javasp "$1" "$db" > "$ev/javasp_$1.out" 2>&1 < /dev/null   # a file, not a pipe: the daemon inherits stdout
}
csql_as() {      # $1 user, $2 file, $3 mode flag (-C|-S)
  csql -u "$1" "$3" "$db" -i "$2" > "$ev/$(basename "$2").$1.out" 2>&1 \
    || { echo "csql $1 $2 failed:"; tail -20 "$ev/$(basename "$2").$1.out"; exit 1; }
  if grep -qE '^ERROR' "$ev/$(basename "$2").$1.out"; then   # "In line N" also heads every successful result block
    echo "csql $1 $2 reported errors:"; grep -E -B3 -A3 '^ERROR' "$ev/$(basename "$2").$1.out" | head -40; exit 1
  fi
}
size_check() {
  local mb; mb=$(du -sm "$base" | cut -f1)
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
javasp_ctl start
csql_as dba "$(sql 00_users.sql)" -C
csql_as app_owner "$(sql 10_schema.sql)" -C
csql_as app_owner "$(sql 20_data.sql)" -C
if [ "$rows_multiplier" -gt 1 ]; then
  sed "s/@ROWS_MULTIPLIER@/$rows_multiplier/g" "$scripts/25_rows_multiplier.sql" > "$ev/25_rows_multiplier.sql"
  csql_as app_owner "$ev/25_rows_multiplier.sql" -C
fi
csql_as app_owner "$(sql 30_objects.sql)" -C
csql_as dba "$(sql 31_dba_objects.sql)" -C
csql_as app_owner "$(sql 40_post.sql)" -C
# DROP TABLE leaves LOB files on 11.4 (G6 #488) and on 10.2/11.0/11.2/11.3 (40 -> 50 files); develop deletes them
[ "$label" = develop ] || csql_as app_owner "$(sql 45_orphan_lob.sql)" -C
csql_as app_owner "$(sql 46_partition_fx.sql)" -C
javasp_ctl stop
bash "$ctl" stop "$db" | tee "$ev/stop1.out" | grep 'RESULT: OK'

# leave no vacuum work behind the load
cubrid vacuumdb -S "$db" > "$ev/vacuumdb.out" 2>&1

if [ "$variant" = dirty ]; then
  write_conf "$(dirty_conf)"
  bash "$ctl" start "$db" | tee "$ev/start2.out" | grep 'RESULT: OK'
  cubrid paramdump -C "$db" > "$ev/paramdump.out" 2>&1 || true
  csql_as app_owner "$(sql 50_dirty.sql)" -C
  bash "$ctl" stop "$db" | tee "$ev/stop2.out" | grep 'RESULT: OK'
  write_conf "$conf_extra"
fi
[ "$old" = 0 ] || stop_master

# pristine copy: the state P1/#485 start from; every SA tool below may write the log or volumes
cp -a --reflink=always "$dbdir" "$pristine"
( cd "$pristine" && find . -type f -print0 | sort -z | xargs -0 sha256sum ) > "$ev/pristine.sha256"

# log header straight from the pristine active log (LOG_HDRPAGE 16 B + LOG_HEADER offsets, r5-min-files)
# append_lsa sits at +112 from 11.4 on; before it LOG_HEADER has no vol_creation (8 B), so +104; chkpt_lsa follows at +8
lsa_off=112; [ "$old" = 0 ] || lsa_off=104
python3 -I - "$pristine/${db}_lgat" "$lsa_off" > "$ev/loghdr.txt" <<'PY'
import struct, sys
b = open(sys.argv[1], 'rb').read(4096)
off = int(sys.argv[2])
def lsa(off):
    v, = struct.unpack_from('<q', b, off)
    pid = v & ((1 << 48) - 1)
    if pid >> 47: pid -= 1 << 48
    o = (v >> 48) & 0xffff
    if o >> 15: o -= 1 << 16
    return f"{pid}|{o}"
print("magic", b[16:16 + 16])
print("append_lsa", lsa(16 + off))
print("chkpt_lsa", lsa(16 + off + 8))
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
csql_as dba "$(sql counts.sql)" -S

mkdir -p "$unload"
( cd "$unload" && cubrid unloaddb -S -u dba "$db" ) > "$ev/unloaddb.out" 2>&1
( cd "$unload" && find . -type f -print0 | sort -z | xargs -0 sha256sum ) > "$ev/unload.sha256"

# put the registered db back to the pristine bytes
rsync -a --delete "$pristine/" "$dbdir/"
( cd "$dbdir" && find . -type f -print0 | sort -z | xargs -0 sha256sum ) | diff - "$ev/pristine.sha256" \
  && echo "registered db == pristine"
find "$pristine/lob" -type f | wc -l > "$ev/lob_files.txt"   # 2 per product row with LOBs (40); every label but develop adds 10 orphans
du -sm "$dbdir" "$pristine" "$unload" > "$ev/sizes.txt"
size_check
echo "DONE $label $variant $db"
