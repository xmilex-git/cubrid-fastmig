# The end of one side's long run (e2e/ops/longrun/README.md § End): restoredb of the latest backup chain to the end of
# the log, PITR to the mark of the PITR cycle, and the probes of where the restorable range starts. Every restore goes
# into its own empty directory, as an operator restores, and is compared with the checks of the state it must
# reproduce, on the same side. Sourced by side_loop.sh after e2e/ops/sides.sh; defines functions only. The side's
# server is down and its DB is left as the last cycle left it.
#
# Output: <side>/restore/<probe>/ (a restored DB, removed after its checks), <side>/check/<probe>.out, <side>/recovery/<probe>.diff, rows
# "probe rc want diff_rc" in <side>/recovery.tsv (diff_rc -1: no comparison). Source: T39 workspace#594.

# restore_into <probe> <lob source> <restoredb args...>: a new <side>/restore/<probe> registered in its own
# databases.txt (volumes and log there, lob-base a reflink copy of <lob source>), the side's active log, log info and
# archives copied in (restoredb -u reads them from the restored log path), then restoredb -u. Restoring over the
# running directory or over an earlier restore leaves files the backup does not hold, whose page LSAs are newer than
# the PITR stop: smoke-long asserted in log_recovery.c:531 that way (workspace#594). CUBRID_DATABASES stays on the
# probe for the checks; restore_end puts the side's back.
restore_into() {
  local probe=$1 lob=$2 p=$OUT/$SIDE/restore/$1 a; shift 2
  mkdir -p "$p/db" "$p/dbs" || return 1
  cp -a --reflink=always "$lob" "$p/lob" || return 1
  cp --reflink=always "$SIDE_DB/${DB}_lgat" "$SIDE_DB/${DB}_lginf" "$p/db/" || return 1
  # what restoredb does not restore but the DB needs where its volumes are: the TDE key file and the Java SP classes
  # (out-of-volume assets), as an operator copies them along with the backup
  cp --reflink=always "$SIDE_DB/${DB}_keys" "$p/db/" || return 1
  for a in java java_static; do [ ! -d "$SIDE_DB/$a" ] || cp -a --reflink=always "$SIDE_DB/$a" "$p/db/" || return 1; done
  for a in "$SIDE_DB/${DB}"_lgar[0-9]*; do [ ! -e "$a" ] || cp --reflink=always "$a" "$p/db/" || return 1; done
  printf '%s\t%s\tlocalhost\t%s\tfile:%s\n' "$DB" "$p/db" "$p/db" "$p/lob" >"$p/dbs/databases.txt"
  export CUBRID_DATABASES=$p/dbs
  cubrid restoredb -u "$@" "$DB"
}
# restore_end <probe>: the probe's server stopped, the side's CUBRID_DATABASES back, and <side>/restore/<probe>/
# (self-contained: its volumes, log, databases.txt and LOB copy) removed by the line of FASTMIG_LONGRUN_CLEANUP that
# run.sh generated with the literal path (map #472 rule: copies go by rm -rf of a literal path, never deletedb). Without
# that script (side_loop.sh run alone) the directory stays for the caller. The evidence is text: check/, recovery/, log/.
restore_end() {
  [ "$UP" = 0 ] || stop
  export CUBRID_DATABASES=$SIDE_DBS
  [ -z "${FASTMIG_LONGRUN_CLEANUP:-}" ] || bash "$FASTMIG_LONGRUN_CLEANUP" "$1"
}

# restore_probe <probe> <want rc> <expected check or -> <lob source> <restoredb args...>: one restore_into, then
# (rc 0) its checks and the diff against <side>/check/<expected>.out. FAIL=1 when the rc differs from want or the diff
# is not empty.
restore_probe() {
  local probe=$1 want=$2 expected=$3 lob=$4 rc d=-1; shift 4
  step "restore-$probe" "$want" restore_into "$probe" "$lob" "$@"
  rc=$?
  if [ "$rc" = 0 ] && [ "$expected" != - ]; then
    step "check-$probe" 0 check "$probe"
    diff "$OUT/$SIDE/check/$expected.out" "$OUT/$SIDE/check/$probe.out" >"$OUT/$SIDE/recovery/$probe.diff" 2>&1
    d=$?
    [ "$d" = 0 ] || { FAIL=1; echo "$SIDE $probe: differs from $expected, see $SIDE/recovery/$probe.diff" >&2; }
  fi
  restore_end "$probe"
  printf '%s\t%s\t%s\t%s\n' "$probe" "$rc" "$want" "$d" >>"$OUT/$SIDE/recovery.tsv"
}

times_field() { awk -F'\t' -v k="$1" -v n="$2" '$1 == k { v = $n } END { print v }' "$OUT/$SIDE/times.tsv"; }   # last row

# end_recovery and range_probes: the probes, each in its own directory:
#   latest        newest chain, up to the end of the log            = check end
#   pitr          the chain the mark lies in, -d <mark>, the LOB files copied at the mark = check pitr-snapshot
#   chain0-bktime first chain (right after the migration), -d backuptime = check baseline (no write before it)
#   before-move   first chain, -d one minute before the migration   must fail: the new log chain starts at the move
#   chain0-next   first chain, -d the first cycle's end             recorded only: needs archives log_max_archives may
#                                                                   have deleted (README § Restorable range)
end_recovery() {
  mkdir -p "$OUT/$SIDE/recovery"
  printf 'probe\trc\twant\tdiff_rc\n' >"$OUT/$SIDE/recovery.tsv"
  SIDE_DBS=$CUBRID_DATABASES
  local bk=$OUT/$SIDE/backup lob latest pitr_at pitr_chain
  lob=$(lob_base)
  latest=$(times_field backup 5)
  restore_probe latest 0 end "$lob" -B "$bk/$latest"
  pitr_at=$(times_field pitr 4); pitr_chain=$(times_field pitr 5)
  if [ -n "$pitr_at" ]; then
    restore_probe pitr 0 pitr-snapshot "$OUT/$SIDE/pitr-lob" -B "$bk/$pitr_chain" -d "$pitr_at"
    [ -z "${FASTMIG_LONGRUN_CLEANUP:-}" ] || bash "$FASTMIG_LONGRUN_CLEANUP" pitr-lob
  else
    FAIL=1; echo "$SIDE: no PITR mark" >&2
  fi
  restore_probe chain0-bktime 0 baseline "$lob" -B "$bk/chain-c00" -d backuptime
}

# range_probes: where the restorable range starts. Run after the error lines are counted (side_loop.sh): a refused
# restore writes its refusal as an error line, which is the expected outcome here, not a fault.
range_probes() {
  local bk=$OUT/$SIDE/backup lob moved c1 rc
  lob=$(lob_base)
  moved=$(times_field moved 2)
  restore_probe before-move 1 - "$lob" -B "$bk/chain-c00" -d "$(date -d "@$(( moved - 60 ))" +%d-%m-%Y:%H:%M:%S)"
  c1=$(awk -F'\t' '$1 == "backup" && $3 == 1 { print $2 }' "$OUT/$SIDE/times.tsv")
  if [ -n "$c1" ]; then
    # recorded, not judged
    restore_into chain0-next "$lob" -B "$bk/chain-c00" -d "$(date -d "@$c1" +%d-%m-%Y:%H:%M:%S)" \
      >"$OUT/$SIDE/log/restore-chain0-next.log" 2>&1
    rc=$?
    restore_end chain0-next
    printf 'chain0-next\t%s\t-\t-1\n' "$rc" >>"$OUT/$SIDE/recovery.tsv"
  fi
}
