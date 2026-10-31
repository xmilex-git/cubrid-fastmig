# The official HA pair of the parity check (G11 (1), workspace#590): the target pair built the way the manual upgrades
# an HA pair (cubrid-manual upgrade.rst "HA 환경에서 DB 마이그레이션", steps C1-C9): the master's DB through
# unloaddb -> loaddb on the target line, backupdb there, restoredb of that backup on the slave, HA conf, start.
# Sourced by e2e/ops/ha/run.sh after pair.sh; defines functions only. Uses run.sh's OUT, DB, PRISTINE, H, nd, tgt_env.
#
# The unload is the baseline's own (basedb/README.md `<label>/unload/<variant>/<db>`), as T36's official side loads it.
# shortcut: createdb/loadjava/loaddb mirror T36's prepare_official (e2e/ops/run.sh, workspace#588) in a node; merge the
# two into one sourced file once both have landed.

# official_pair <unload prefix> <label>: the official target DB on both nodes, registered in each <host>/tgt-dbs.
official_pair() {
  local ref=$1 label=$2 i d vol0 logsz
  local m; m=$(nd 0)
  # the unload and the source's LOB files where the nodes see them; LOB files go to each node's private mount
  # (/fm590-private, pair.sh), so the master's and the slave's copies have one absolute path, as on two hosts
  mkdir -p "$OUT/unload" || return 1
  cp "${ref}"_* "$OUT/unload/" || return 1
  local uref=$OUT/unload/$DB lob=/fm590-private/lob-src
  for i in 0 1; do
    d=$(nd "$i"); mkdir -p "$d/tgt" "$d/private" || return 1
    [ ! -d "$PRISTINE/lob" ] || cp -a --reflink=always "$PRISTINE/lob" "$d/private/lob-src" || return 1
  done
  case "$label" in
    11.4 | develop) sed "s#\(\^E'[BC][0-9]*\)|file:\([^/]\)#\1|file:$lob/\2#g" "${uref}_objects" >"$OUT/unload/objects" ;;
    *) echo "official pair: label $label needs the 10.2..11.3 LOB base rewrite of T36" >&2; return 1 ;;
  esac || return 1
  vol0=$(($(stat -c %s "$PRISTINE/$DB") / 1048576))
  logsz=$(($(stat -c %s "$PRISTINE/${DB}_lgat") / 1048576))
  local opts="-S -u dba -s '${uref}_schema' -d '$OUT/unload/objects'"
  [ ! -s "${uref}_indexes" ] || opts+=" -i '${uref}_indexes'"
  [ ! -s "${uref}_trigger" ] || opts+=" --trigger-file '${uref}_trigger'"
  # C1-C6 on the master: createdb with the source's volume 0 and log sizes, the Java class, loaddb, backupdb
  node_sh 0 $(tgt_env 0) "cubrid createdb --db-volume-size=${vol0}M --log-volume-size=${logsz}M \
    --file-path='$m/tgt' --log-path='$m/tgt' $DB en_US.utf8" || return 1
  if [ -f "$PRISTINE/java/FmHello.class" ]; then
    cp "$PRISTINE/java/FmHello.class" "$OUT/unload/" || return 1   # the pristine is not mounted in the nodes
    node_sh 0 $(tgt_env 0) "loadjava $DB '$OUT/unload/FmHello.class'" || return 1
  fi
  node_sh 0 $(tgt_env 0) "cd '$OUT/unload' && cubrid loaddb $opts $DB" || return 1
  # loaddb reports a failed row or statement in its log and still exits 0
  ! grep -qE '[1-9][0-9]* object\(s\) failed|ERROR|[Ee]rror:' "$OUT/unload/${DB}_loaddb.log" || return 1
  mkdir -p "$m/backup" || return 1
  node_sh 0 $(tgt_env 0) "cubrid backupdb -S -D '$m/backup' $DB" || return 1
  # C7-C8 on the slave: the master's databases.txt line with the slave's paths, the Java directory, restoredb -u
  # (volumes to the databases.txt paths, not the master's: both nodes see the whole run dir)
  local s; s=$(nd 1)
  printf '%s\t%s\tlocalhost\t%s\tfile:%s/lob\n' "$DB" "$s/tgt" "$s/tgt" "$s/tgt" >"$s/tgt-dbs/databases.txt"
  [ ! -d "$m/tgt/java" ] || cp -a "$m/tgt/java" "$s/tgt/" || return 1
  node_sh 1 $(tgt_env 1) "cubrid restoredb -u -B '$m/backup' $DB"
}
