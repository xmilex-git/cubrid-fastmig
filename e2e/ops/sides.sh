# The two sides of a parity run, shared by the operations scenario runner (run.sh), the application query driver
# (queries/run.sh) and the long run (longrun/side_loop.sh): argument and environment setup, timed steps, the server of a
# side, the preparation of each side (side "migrated": cubrid-fastmig; side "official": source unloaddb -> target
# loaddb), the scenario pieces (checks, DML rounds, kill -9, archives) and the fault counts. Sourced; defines functions (and OPS when unset).
#
# ops_setup <bin> <pristine dir> <target version> <out dir> sets the globals every function here reads: BIN PRISTINE TV
# OUT INST PORT CTL W DB LABEL REF and the install's environment, writes the install's cubrid.conf and changes into <out>.
# side_begin/side_end bracket the steps of one side; side_faults counts its faults so far. Interface and env:
# e2e/ops/README.md.
# Source: G12 workspace#587, T36 workspace#588 (runner), T40 workspace#595 (split out for the query driver), T39
# workspace#594 (scenario pieces and side_faults for the long run).

# OPS is the e2e/ops directory (checks.sql, dml/, official_102_schema.awk); a caller that does not set it gets this file's own
OPS=${OPS:-$(dirname "$(realpath "${BASH_SOURCE[0]}")")}

# ops_setup <bin> <pristine> <target version> <out>: exits 2 on bad arguments or an existing <out>
ops_setup() {
  BIN=$(realpath "${1:?cubrid-fastmig binary}")
  PRISTINE=$(realpath "${2:?pristine dir}")
  TV=${3:?target version}
  OUT=${4:?out dir}
  INST=$(realpath "${FASTMIG_OPS_INSTALL:?FASTMIG_OPS_INSTALL required}")
  PORT=${FASTMIG_OPS_PORT:?FASTMIG_OPS_PORT required}
  CTL=${FASTMIG_OPS_SERVER_CTL:?FASTMIG_OPS_SERVER_CTL required}
  W=${FASTMIG_OPS_WORKERS:-4}
  [ ! -e "$OUT" ] || { echo "$OUT exists" >&2; exit 2; }
  local vinf
  vinf=$(ls "$PRISTINE"/*_vinf 2>/dev/null | head -1)
  [ -n "$vinf" ] || { echo "no *_vinf in $PRISTINE" >&2; exit 2; }
  DB=$(basename "$vinf" _vinf)
  # <root>/<label>/pristine/<variant>: the label is the source version and picks the scenario SQL
  LABEL=$(basename "$(dirname "$(dirname "$PRISTINE")")")
  case "$LABEL" in 10.2 | 11.2 | 11.3 | 11.4 | develop) ;; *) echo "label $LABEL is not a source version" >&2; exit 2 ;; esac
  REF=${FASTMIG_OPS_SOURCE_UNLOAD:-$(dirname "$(dirname "$PRISTINE")")/unload/$(basename "$PRISTINE")/$DB}
  [ -s "${REF}_schema" ] || { echo "no unload ${REF}_schema" >&2; exit 2; }
  mkdir -p "$OUT/parity"
  OUT=$(realpath "$OUT")
  cd "$OUT" || exit 2

  # CUBRID_TMP per port: the PL server socket is $CUBRID_TMP/sp_<db>.sock (pl_comm.c), and two runs of the same DB on
  # other ports would take each other's socket (workspace#589 lanes in parallel)
  export CUBRID=$INST CUBRID_TMP=$HOME/.fastmig-ops-tmp/$PORT
  export PATH=$INST/bin:$PATH LD_LIBRARY_PATH=$INST/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
  mkdir -p "$CUBRID_TMP"
  # auto_restart_server=no: the master would revive the server the kill9-recovery step kills and hold the DB before the
  # runner's own start
  printf '[service]\nservice=server\n[common]\ncubrid_port_id=%s\ndata_buffer_size=512M\nstored_procedure=yes\nauto_restart_server=no\n' \
    "$PORT" >"$INST/conf/cubrid.conf"

  FAIL=0
  SIDE=""   # the side the steps run on: migrated | official
  UP=0      # 1 while this side's server runs (checks then use csql -C, else -S)
}

# step <name> <expected rc> <cmd...>: runs and times one step of $SIDE, log in <side>/log/<name>.log
step() {
  local name=$1 want=$2 s e rc; shift 2
  s=$(date +%s.%N)
  "$@" >"$OUT/$SIDE/log/$name.log" 2>&1
  rc=$?
  e=$(date +%s.%N)
  printf '%s\t%s\t%.3f\t%s\n' "$SIDE" "$name" "$(awk -v a="$s" -v b="$e" 'BEGIN { print b - a }')" "$rc" >>"$OUT/steps.tsv"
  [ "$rc" = "$want" ] || { FAIL=1; echo "$SIDE step $name: exit $rc (expected $want)" >&2; }
  return "$rc"
}

# csql -i of one SQL file as dba: exit 1 when the output holds an ERROR (csql -i goes on and exits 0, workspace#555)
sql_file() {
  local mode=$1 file=$2 out rc; shift 2   # the rest: extra csql options
  out=$(csql "$mode" "$@" -u dba -t -i "$file" "$DB" 2>&1)
  rc=$?
  printf '%s\n' "$out"
  [ "$rc" = 0 ] || return "$rc"
  ! /usr/bin/grep -qE '^[[:space:]]*ERROR' <<<"$out"
}

server_pid() {   # the cub_server of $DB whose environment is this side's CUBRID_DATABASES
  local p
  for p in $(pgrep -x cub_server); do
    tr '\0' '\n' <"/proc/$p/environ" 2>/dev/null | grep -qx "CUBRID_DATABASES=$CUBRID_DATABASES" && echo "$p"
  done
}
# the server reports started before its PL server (JVM) accepts calls; an SP call in that gap fails with "Can't connect
# PL server", so start waits up to 60 s for the Java SP every baseline DB has (a source without its class, the TPC-H DB of
# queries/, has no SP to wait for)
start() {
  bash "$CTL" start "$DB" || return 1
  UP=1
  [ -f "$PRISTINE/java/FmHello.class" ] || return 0
  local i
  # first the PL server's socket in LISTEN (pl_comm.c: $CUBRID_TMP/sp_<db>.sock; the file alone may be one a kill -9
  # left behind), then the SP call itself
  for ((i = 0; i < 30; i++)); do
    ss -xl | grep -qF "$CUBRID_TMP/sp_$DB.sock" && break
    sleep 2
  done
  for ((i = 0; i < 30; i++)); do
    csql -C -u dba -t -c "SELECT dba.fm_hello('ready') FROM db_root" "$DB" 2>&1 | grep -q 'hello ready' && return 0
    sleep 2
  done
  echo "PL server not ready after 60 s"; return 1
}
stop() { bash "$CTL" stop "$DB" && UP=0; }
# after a side: SIGTERM to the cub_master on the claimed port and to any cub_pl left by the kill -9, matched by this
# install's CUBRID= in their environment (never `cubrid service stop` on the shared host: cubrid-server-control P4)
stop_master() {
  local p
  for p in $(ss -ltnp "sport = :$PORT" | sed -n 's/.*pid=\([0-9]*\).*/\1/p' | sort -u) $(pgrep -x cub_pl); do
    tr '\0' '\n' <"/proc/$p/environ" 2>/dev/null | grep -qx "CUBRID=$INST" && { echo "SIGTERM $p $(cat /proc/$p/comm)"; kill -TERM "$p"; }
  done
  timeout 30 bash -c "while [ -n \"\$(ss -ltn 'sport = :$PORT' | tail -n +2)\" ]; do sleep 1; done" || echo "port $PORT still listening"
}

# round_files: ROUND_FILES, the DML round of this source: synonyms from 11.2, PL/CSQL from 11.4 (basedb per-label
# overrides). The scenario functions below read OPS (e2e/ops) from the caller.
round_files() {
  ROUND_FILES=("$OPS/dml/round.sql")
  case "$LABEL" in 11.2 | 11.3 | 11.4 | develop) ROUND_FILES+=("$OPS/dml/round_synonym.sql") ;; esac
  case "$LABEL" in 11.4 | develop) ROUND_FILES+=("$OPS/dml/round_plcsql.sql") ;; esac
}

# checks.sql after a scenario step into <side>/check/<step>.out; fails on an error or an index that differs from its heap.
# Always in CS mode (the Java SP of checks.sql runs in the server's PL process): a stopped server is started around it.
check() {
  local name=$1 f=$OUT/$SIDE/check/$1.out was=$UP rc checks=$OPS/checks.sql
  # 11.4 names the trigger view db_trig; db_trigger there is the system class, without trigger_name (workspace#589)
  if [ "$TV" = 11.4 ]; then
    checks=$OUT/checks-11.4.sql
    sed 's/ FROM db_trigger;$/ FROM db_trig;/' "$OPS/checks.sql" >"$checks" || return 1
  fi
  [ "$was" = 1 ] || start || return 1
  sql_file -C "$checks" -N >"$f" && ! grep -q "INDEX!=HEAP" "$f"
  rc=$?
  # diagnostic only, not compared: the customer rows behind the digest (check_rows.sql)
  csql -C -u dba -N -i "$OPS/check_rows.sql" "$DB" >"${f%.out}.rows" 2>&1
  [ "$was" = 1 ] || stop || return 1
  return $rc
}
scenario_step() {   # scenario_step <name> <cmd...>: the step, then its checks as step check-<name>
  local name=$1; shift
  step "$name" 0 "$@"
  step "check-$name" 0 check "$name"
}

kill9_server() {   # kill -9 of this side's server and of the cub_pl it leaves; the next boot runs crash recovery
  local p; p=$(server_pid)
  [ -n "$p" ] || { echo "no cub_server of $DB"; return 1; }
  echo "kill -9 $p"; kill -9 "$p"
  timeout 60 bash -c "while kill -0 $p 2>/dev/null; do sleep 1; done" || return 1
  # what else of this side still runs (the server's cub_pl outlives it): listed, then killed as a crash would leave it
  local q
  for q in $(pgrep -x cub_pl); do
    tr '\0' '\n' <"/proc/$q/environ" 2>/dev/null | grep -qx "CUBRID_DATABASES=$CUBRID_DATABASES" || continue
    echo "left after the kill: $q $(cat /proc/$q/comm), kill -9"; kill -9 "$q"
    timeout 60 bash -c "while kill -0 $q 2>/dev/null; do sleep 1; done" || return 1
  done
  UP=0
}
kill9_restart() { kill9_server && start; }   # the restart runs crash recovery in the server
dml_rounds() {   # dml_rounds <n>: n DML rounds, every file of this source
  local i f
  for ((i = 1; i <= $1; i++)); do
    for f in "${ROUND_FILES[@]}"; do echo "round $i $(basename "$f")"; sql_file -C "$f" || return 1; done
  done
}
last_archive() {   # number of the newest log archive of this side, -1 when none
  ls "$SIDE_DB" 2>/dev/null | sed -n "s/^${DB}_lgar\([0-9][0-9]*\)\$/\1/p" | sort -n | tail -1 | awk '{ n = $1 + 0 } END { print NR ? n : -1 }'
}

# prepare_migrated: reflink copy -> migrate --verify -> the SQL migrate leaves for the user (as e2e/run.sh)
prepare_migrated() {
  local src=$OUT/migrated/source
  cp -a --reflink=always "$PRISTINE" "$src" || return 1
  printf '%s\t%s\tlocalhost\t%s\tfile:%s\n' "$DB" "$src" "$src" "${FASTMIG_OPS_LOB_BASE:-$src/lob}" >"$OUT/migrated/src-dbs/databases.txt"
  "$BIN" migrate "$DB" --dest "$SIDE_DB" --target-version "$TV" --volumes-in-first-dir \
    --source-databases-txt "$OUT/migrated/src-dbs/databases.txt" --target-databases-txt "$CUBRID_DATABASES/databases.txt" \
    --workers "$W" --verify --progress plain || return 1
  grep -q "^$DB[[:space:]]" "$CUBRID_DATABASES/databases.txt" || { echo "target not registered"; return 1; }
  local f
  for f in recompile-plcsql recompile-views deferred-indexes update-statistics; do
    [ ! -f "$SIDE_DB/fastmig/$f.sql" ] || { echo "== $f"; sql_file -S "$SIDE_DB/fastmig/$f.sql" || return 1; }
  done
  # finalize deletes the source copy, so only the two sides' DBs remain; a lob-base outside the copy belongs to the
  # registered baseline and finalize would delete its files (workspace#529): the copy is then left for the caller
  if [ -n "${FASTMIG_OPS_LOB_BASE:-}" ]; then
    echo "finalize skipped (lob-base outside the copy); remove $src by its literal path"; return 0
  fi
  # guard as e2e/run.sh: every path the source fingerprint names lies in this run's source copy
  awk -F'\t' -v s="$src/" '
      $1 == "volume" { if (index($3, s) != 1) bad = 1 }
      $1 == "log" || $1 == "vol-path" || $1 == "log-path" || $1 == "lob-base" { if (index($2 "/", s) != 1) bad = 1 }
      END { exit bad }' "$SIDE_DB/fastmig/source-fingerprint" ||
    { echo "finalize refused: the source fingerprint names a path outside $src"; return 1; }
  "$BIN" finalize "$SIDE_DB"
}

# prepare_official: createdb with the source's volume 0 and log sizes, loaddb of the source unload, the Java class and
# the LOB files where the unload's locators point
prepare_official() {
  local vol0 logsz opts objects=${REF}_objects base
  vol0=$(( $(stat -c %s "$PRISTINE/$DB") / 1048576 ))
  logsz=$(( $(stat -c %s "$PRISTINE/${DB}_lgat") / 1048576 ))
  (cd "$SIDE_DB" && cubrid createdb --db-volume-size="${vol0}M" --log-volume-size="${logsz}M" \
    --file-path="$SIDE_DB" --log-path="$SIDE_DB" "$DB" en_US.utf8) || return 1
  # the source's LOB files: a reflink copy outside this side's lob-base, every locator of the unload pointed at it by an
  # absolute path (only the path prefix changes; the unload itself is only read). 11.4 and develop unloads name the
  # files relative to the lob-base, but a copy under develop's lob-base is lost: develop removes its top-level ces_*
  # directories at every boot (CBRD-23700), loaddb's SA boot included, and keeps the relative locator. 10.2..11.3
  # unloads name them under the registered baseline, whose files a LOB UPDATE of the scenario would delete.
  local lob_src=$OUT/official/lob-src
  if [ -d "$PRISTINE/lob" ]; then cp -a --reflink=always "$PRISTINE/lob" "$lob_src" || return 1; else mkdir -p "$lob_src"; fi
  objects=$OUT/official/objects
  case "$LABEL" in
    11.4 | develop) sed "s#\(\^E'[BC][0-9]*\)|file:\([^/]\)#\1|file:$lob_src/\2#g" "${REF}_objects" >"$objects" || return 1 ;;
    *) base=${FASTMIG_OPS_LOB_BASE:?a $LABEL source needs FASTMIG_OPS_LOB_BASE}
       sed "s#|file:${base%/}/#|file:$lob_src/#g" "${REF}_objects" >"$objects" || return 1 ;;
  esac
  [ ! -f "$PRISTINE/java/FmHello.class" ] || loadjava "$DB" "$PRISTINE/java/FmHello.class" || return 1
  local schema=${REF}_schema
  if [ "$LABEL" = 10.2 ]; then
    # a 10.2 unload hands a partitioned table to its owner before its PARTITION BY, and develop/11.4 loaddb
    # --no-user-specified-name then fails on it (workspace#516): that change_owner moves after the PARTITION BY
    schema=$OUT/official/schema
    awk -f "$OPS/official_102_schema.awk" "${REF}_schema" "${REF}_schema" >"$schema" || return 1
  fi
  opts=(-S -u dba -s "$schema" -d "$objects")
  [ ! -s "${REF}_indexes" ] || opts+=(-i "${REF}_indexes")
  [ ! -s "${REF}_trigger" ] || opts+=(--trigger-file "${REF}_trigger")
  [ "$LABEL" != 10.2 ] || opts+=(--no-user-specified-name)
  (cd "$OUT/official" && cubrid loaddb "${opts[@]}" "$DB") || return 1
  # loaddb reports a failed row or statement on its output and still exits 0
  ! grep -qE '[1-9][0-9]* object\(s\) failed|ERROR|[Ee]rror:' "$OUT/official/${DB}_loaddb.log"
}


# side_begin <side>: this side's directories, databases.txt and the marks side_end reads
side_begin() {
  SIDE=$1; UP=0
  SIDE_DB=$OUT/$SIDE/db
  export CUBRID_DATABASES=$OUT/$SIDE/dbs
  mkdir -p "$OUT/$SIDE/log" "$OUT/$SIDE/check" "$OUT/$SIDE/dbs" "$OUT/$SIDE/src-dbs" "$OUT/$SIDE/backup" "$SIDE_DB"
  : >"$CUBRID_DATABASES/databases.txt"
  touch "$OUT/$SIDE/start-marker"
  # the install's logs only grow (cubrid_utility.log keeps every run): their sizes now, so the side keeps what it adds
  find "$INST/log" -type f -printf '%s\t%p\n' >"$OUT/$SIDE/log-sizes"
}

# side_faults: what this side added to the install's logs since side_begin, copied to <side>/server-log/ (refreshed on
# every call), and prints "error_lines assert_lines cores pl_drops session_expired". Server error log lines; asserts in
# those logs and in the steps' own output (a server that asserts while it starts prints it to the start output); cores
# of any process of this side: a core under core_pattern written while the side ran whose memory holds a
# CUBRID_DATABASES under the side's directory (its own and a restored DB's; servers and SA utilities alike), listed in
# <side>/cores.txt. The long run calls it every cycle. session_expired: the server's "-1066 Session expired" error
# lines (session.c), not counted as error lines: an engine defect of develop and 11.4.6 alike, one line per csql
# session that ran a join with a GROUP BY sort, on both sides and on a source never migrated (workspace#594 smoke, T40
# workspace#595).
side_faults() {
  local errs asserts pl_drops expired cores=0 c cdir f old
  mkdir -p "$OUT/$SIDE/server-log"
  for f in $(find "$INST/log" -type f -newer "$OUT/$SIDE/start-marker" 2>/dev/null); do
    old=$(awk -F'\t' -v p="$f" '$2 == p { print $1 }' "$OUT/$SIDE/log-sizes")
    tail -c +$(( ${old:-0} + 1 )) "$f" >"$OUT/$SIDE/server-log/$(basename "$f")"
  done
  expired=$(cat /dev/null "$OUT/$SIDE"/server-log/* | grep -c "ERROR \*\*\* file .*/session\.c, line [0-9]* ERROR CODE = -1066,")
  errs=$(( $(cat /dev/null "$OUT/$SIDE"/server-log/* | grep -c "ERROR \*\*\*") - expired ))
  asserts=$(cat /dev/null "$OUT/$SIDE"/server-log/* "$OUT/$SIDE"/log/*.log | grep -c 'Assertion .* failed')
  # a PL server that drops mid-run (README § Known flakes) fails the run through its error lines; counted apart
  pl_drops=$(cat /dev/null "$OUT/$SIDE"/server-log/*.err | grep -c "^Networking with PL server failed")
  cdir=$(dirname "$(cat /proc/sys/kernel/core_pattern)")
  : >"$OUT/$SIDE/cores.txt"
  # -H: core_pattern's directory may be a symlink (/data/core -> /bench/ssd/core on the dev host)
  for c in $(find -H "$cdir" -maxdepth 1 -name 'core.*' -newer "$OUT/$SIDE/start-marker" 2>/dev/null); do
    grep -aqF "CUBRID_DATABASES=$OUT/$SIDE/" "$c" && { cores=$((cores + 1)); echo "$SIDE $c" >>"$OUT/$SIDE/cores.txt"; }
  done
  echo "$errs $asserts $cores $pl_drops $expired"
}

# side_end: side_faults once more, its cores appended to <out>/cores.txt and one row of <out>/server-log-summary.tsv
side_end() {
  local errs asserts cores pl_drops expired
  read -r errs asserts cores pl_drops expired < <(side_faults)
  cat "$OUT/$SIDE/cores.txt" >>"$OUT/cores.txt"
  [ -s "$OUT/server-log-summary.tsv" ] || printf 'side\terror_lines\tassert_lines\tcores\tpl_drops\tsession_expired\n' >"$OUT/server-log-summary.tsv"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$SIDE" "$errs" "$asserts" "$cores" "$pl_drops" "$expired" >>"$OUT/server-log-summary.tsv"
  [ "$errs$asserts$cores" = 000 ] || { FAIL=1; echo "$SIDE: server error lines $errs (PL server drops $pl_drops), asserts $asserts, cores $cores" >&2; }
}
