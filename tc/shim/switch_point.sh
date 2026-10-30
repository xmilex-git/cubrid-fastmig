# The switch point of a ported testcase: where the testcase first calls unloaddb, every database it registered is
# migrated with cubrid-fastmig, checked, and put in the testcase's place; the testcase then goes on (its own
# unloaddb, loaddb, queries and expected answers) against the migrated databases.
#
# Input: env from run_case.sh — FASTMIG_TC_JOB (job dir), FASTMIG_TC_BIN (cubrid-fastmig), FASTMIG_TC_SWITCH (0 = a
# baseline run: no switch), FASTMIG_TC_SOURCE_DBS / FASTMIG_TC_TARGET_DBS (the two databases.txt dirs);
# CUBRID_DATABASES is the symlink $FASTMIG_TC_JOB/dbs, pointing at the source dir until the switch.
# Output: per database one line in $FASTMIG_TC_JOB/switch.tsv (db, then the exit code of source unload, migrate,
# verify, deferred SQL, target unload, set comparison) and the step outputs under $FASTMIG_TC_JOB/switch/<db>/;
# the symlink flipped to the target dir (every process sees it, subshells included); servers that were running
# restarted on the target. A source database is deleted only when all its steps exited 0; a failed one is renamed
# <db>_src and kept for analysis (workspace#502).
# Source: decisions G1 workspace#481 (migrate, verify, SQL files in <dest>/fastmig), G5 workspace#486 (deferred
# indexes are applied before the set comparison), ADR 0007 (verifier + set comparison).

# the databases registered in the source databases.txt, one name per line
fastmig_tc_source_dbs() {
  awk 'NF >= 5 && $1 !~ /^#/ { print $1 }' "$FASTMIG_TC_SOURCE_DBS/databases.txt" 2>/dev/null
}

# one database: source unload, migrate, verify, deferred SQL, target unload, set comparison
fastmig_tc_migrate_one() {
  local db="$1" d="$FASTMIG_TC_JOB/switch/$1"
  local r_su r_mi r_ve r_sq=0 r_tu r_co=-1 f
  mkdir -p "$d/source-unload" "$d/target-unload"
  ( cd "$d" && CUBRID_DATABASES="$FASTMIG_TC_SOURCE_DBS" "$CUBRID/bin/cubrid" unloaddb -S -O "$d/source-unload" "$db" ) \
    >"$d/source-unload.log" 2>&1
  r_su=$?
  # --verify: the verifier runs on target and source, and only a target without migration defects is registered
  # (ADR 0015); its report is <dest>/fastmig/verify.jsonl
  "$FASTMIG_TC_BIN" migrate "$db" --dest "$d/volumes" --workers 4 --progress plain --verify \
    --source-databases-txt "$FASTMIG_TC_SOURCE_DBS/databases.txt" \
    --target-databases-txt "$FASTMIG_TC_TARGET_DBS/databases.txt" \
    >"$d/migrate.log" 2>&1
  r_mi=$?
  # 0 = the report's summary counts 0 migration defects, 1 = some, 2 = no report
  r_ve=2
  if [ -f "$d/volumes/fastmig/verify.jsonl" ]; then
    /usr/bin/grep -q '"kind":"summary","migration":0,' "$d/volumes/fastmig/verify.jsonl" && r_ve=0 || r_ve=1
  fi
  # csql -i goes on after a failed statement and exits 0, so an ERROR in its output fails the step (workspace#555)
  for f in deferred-indexes.sql recompile-plcsql.sql update-statistics.sql; do
    [ -f "$d/volumes/fastmig/$f" ] || continue
    CUBRID_DATABASES="$FASTMIG_TC_TARGET_DBS" "$CUBRID/bin/csql" -S -u dba -i "$d/volumes/fastmig/$f" "$db" \
      >"$d/sql-$f.log" 2>&1 || r_sq=$?
    /usr/bin/grep -qE '^[[:space:]]*ERROR' "$d/sql-$f.log" && r_sq=1
    cat "$d/sql-$f.log" >>"$d/sql.log"
  done
  ( cd "$d" && CUBRID_DATABASES="$FASTMIG_TC_TARGET_DBS" "$CUBRID/bin/cubrid" unloaddb -S -O "$d/target-unload" "$db" ) \
    >"$d/target-unload.log" 2>&1
  r_tu=$?
  # unloaddb exits 1 on a database without user classes: empty on both sides is the same set
  if [ "$r_su" = 1 ] && [ "$r_tu" = 1 ] && /usr/bin/grep -q 'No data to be unloaded' "$d/source-unload.log" \
    && /usr/bin/grep -q 'No data to be unloaded' "$d/target-unload.log"; then
    r_su=0 r_tu=0 r_co=0
  elif [ "$r_su" = 0 ] && [ "$r_tu" = 0 ]; then
    "$FASTMIG_TC_BIN" set-comparison "$d/source-unload/$db" "$d/target-unload/$db" --diff "$d/set-comparison.diff" \
      >"$d/set-comparison.log" 2>&1
    r_co=$?
  fi
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$db" "$r_su" "$r_mi" "$r_ve" "$r_sq" "$r_tu" "$r_co" >>"$FASTMIG_TC_JOB/switch.tsv"
  if [ "$r_su$r_mi$r_ve$r_sq$r_tu$r_co" = 000000 ]; then
    CUBRID_DATABASES="$FASTMIG_TC_SOURCE_DBS" "$CUBRID/bin/cubrid" deletedb "$db" >>"$d/source-delete.log" 2>&1
  else
    # kept for analysis under another name: its files sit where the testcase recreates <db>
    CUBRID_DATABASES="$FASTMIG_TC_SOURCE_DBS" "$CUBRID/bin/cubrid" renamedb "$db" "${db}_src" \
      >>"$d/source-keep.log" 2>&1
  fi
}

fastmig_tc_switch_point() {
  [ "$FASTMIG_TC_SWITCH" = 1 ] || return 0
  [ "$(readlink "$FASTMIG_TC_JOB/dbs")" = "$FASTMIG_TC_SOURCE_DBS" ] || return 0
  local running db
  running="$("$CUBRID/bin/cubrid" server status 2>/dev/null | sed -n 's/^ *Server \([^ ]*\) .*/\1/p')"
  for db in $running; do fastmig_tc_server stop "$db" >/dev/null; done
  "$CUBRID/bin/cubrid" service stop >/dev/null 2>&1
  : >>"$FASTMIG_TC_TARGET_DBS/databases.txt"
  # a pipe, not process substitution: testcases run under sh (bash in POSIX mode)
  fastmig_tc_source_dbs | while read -r db; do
    fastmig_tc_migrate_one "$db"
  done
  ln -sfn "$FASTMIG_TC_TARGET_DBS" "$FASTMIG_TC_JOB/dbs"
  for db in $running; do fastmig_tc_server start "$db" >/dev/null; done
  echo "$running" >"$FASTMIG_TC_JOB/restarted-servers"
}

export -f fastmig_tc_source_dbs fastmig_tc_migrate_one fastmig_tc_switch_point
