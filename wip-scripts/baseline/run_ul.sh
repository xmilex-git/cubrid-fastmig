#!/usr/bin/env bash
# One unload->load run: 11.4.6 unloaddb of fm114_clean -> develop createdb -> loaddb schema, data,
# index (--no-logging-index, always CS: the no-logging build exists only in SERVER_MODE), trigger.
#   bash run_ul.sh <input: small|big> <mode: SA|CS> <W: 1|4|16> <rep>
# SA: unloaddb -S, loaddb -S for schema and data. CS: 11.4 server + unloaddb -C, develop server + loaddb -C.
# Every process runs under taskset on the W CPUs of cpus_for; servers are started under the same mask.
# Appends "input mode W rep stage seconds rc" rows to results/ul.tsv; per-run logs in runs/<input>/<mode>-w<W>-r<rep>/.
set -uo pipefail
. "$(dirname "$0")/env.sh"
# pause point (1번 세션 2026-10-30): another session creates baseline/PAUSE before heavy I/O between runs
paused=0
while [ -e $BL/PAUSE ]; do sleep 30; paused=$((paused + 30)); done
[ $paused -eq 0 ] || echo "PAUSED $paused s before $(basename "$0") $*"
in=$1; mode=$2; W=$3; rep=$4
case "$in" in small) srcdbs=$BL/in/small114/dbs; srcdir=$BL/in/small114/db ;;
              big) srcdbs=$BL/basedb-big/11.4; srcdir=$BL/basedb-big/11.4/clean ;; *) exit 2 ;; esac
case "$mode" in SA) m=-S ;; CS) m=-C ;; *) exit 2 ;; esac
C=$(cpus_for "$W") || exit 2
run=$BL/runs/$in/$mode-w$W-r$rep; tgt=$run/tgt; ul=$run/unload; tgtdbs=$BL/tgtdbs
[ ! -e "$run" ] || { echo "$run exists" >&2; exit 1; }
mkdir -p "$ul" "$tgt" "$tgtdbs"; touch "$tgtdbs/databases.txt"
fail=0
# Every exit (success, failed stage, signal) leaves at most the source DB behind: servers of this run stopped,
# unload data files truncated (sizes kept in unload.ls), target DB deleted; du/df recorded before and after.
cleanup() {
  set +e
  if ! no_server_of $IDEV 2>/dev/null; then ( usedev "$tgtdbs"; bash $CTL stop fmbl > "$run/cleanup.stopdev.out" 2>&1 ); fi
  stop_master $IDEV
  if ! no_server_of $I114 2>/dev/null; then ( use114 "$srcdbs"; bash $CTL stop fm114_clean > "$run/cleanup.stop114.out" 2>&1 ); fi
  stop_master $I114
  ls -l "$ul" > "$run/unload.ls" 2>/dev/null
  for f in "$ul"/fm114_clean_objects "$ul"/fm114_clean_schema "$ul"/fm114_clean_indexes "$ul"/fm114_clean_trigger; do
    [ ! -f "$f" ] || : > "$f"
  done
  [ ! -d "$tgt" ] || du -sm "$tgt" > "$run/tgt.du"
  if grep -q '^fmbl[[:space:]]' "$tgtdbs/databases.txt" 2>/dev/null; then
    ( usedev "$tgtdbs"; cubrid deletedb fmbl > "$run/deletedb.out" 2>&1 ) || echo "deletedb fmbl failed" >&2
  fi
  echo "post-clean: $(meta)" >> "$run/meta.txt"
}
trap cleanup EXIT
trap 'exit 143' TERM INT
stage() {   # stage <name> <cmd...>: time one step, log to $run/<name>.out
  local name=$1; shift; local s e rc
  s=$(now); if "$@" > "$run/$name.out" 2>&1; then rc=0; else rc=$?; fi; e=$(now)
  printf '%s\t%s\t%s\t%s\t%s\t%.3f\t%s\n' "$in" "$mode" "$W" "$rep" "$name" "$(awk -v a="$s" -v b="$e" 'BEGIN{print b-a}')" "$rc" >> $RES/ul.tsv
  [ $rc -eq 0 ] || { echo "stage $name rc=$rc" >&2; fail=1; }
  return $rc
}
T="taskset -c $C"
{ echo "cpus=$C"; echo "pre: $(meta)"; } > "$run/meta.txt"
# quiet gate (1번 세션 2026-10-29): if the measured CPUs are busier than W/2, or /home's nvme0n1 is over 50 %util,
# wait 60 s and look again, at most 2 times; the run then goes ahead and the gate record stays in meta.txt.
for try in 0 1 2; do
  read -r busy util <<< "$(quiet_sample "$C")"
  echo "gate try=$try busy_on_cpus=$busy (limit $(awk -v w=$W 'BEGIN{print w/2}')) nvme0n1_util=$util% (limit 50) load1=$(cut -d' ' -f1 /proc/loadavg)" >> "$run/meta.txt"
  awk -v b="$busy" -v u="$util" -v w="$W" 'BEGIN{exit !(b > w / 2 || u > 50)}' || break
  [ $try -lt 2 ] && sleep 60 || echo "gate: still busy after 2 retries, run anyway" >> "$run/meta.txt"
done

# page cache: not dropped (shared host); every run starts with the source read once (warm)
find "$srcdir" -maxdepth 1 -type f -name 'fm114_clean*' ! -name '*_lgar*' -exec cat {} + > /dev/null   # archives are never read

# ---- 11.4 side
use114 "$srcdbs"; conf114
if [ $mode = CS ]; then stage srv114_start $T bash $CTL start fm114_clean || exit 1; fi
stage unload bash -c "cd '$ul' && $T cubrid unloaddb $m -u dba -t $W fm114_clean"
if [ $mode = CS ]; then stage srv114_stop bash $CTL stop fm114_clean; no_server_of $I114 && stop_master $I114; fi
ls -l "$ul" > "$run/unload.ls"
[ $fail -eq 0 ] || exit 1

# ---- develop side
usedev "$tgtdbs"; confdev "$W"
stage createdb bash -c "cd '$tgt' && $T cubrid createdb --db-volume-size=512M --log-volume-size=256M --file-path='$tgt' --log-path='$tgt' fmbl en_US.utf8" || exit 1
L="$T cubrid loaddb -u dba"
if [ $mode = SA ]; then
  stage schema $L -S -s "$ul/fm114_clean_schema" fmbl
  stage data $L -S -d "$ul/fm114_clean_objects" fmbl
  stage srvdev_start $T bash $CTL start fmbl || exit 1
else
  stage srvdev_start $T bash $CTL start fmbl || exit 1
  stage schema $L -C -s "$ul/fm114_clean_schema" fmbl
  stage data $L -C -d "$ul/fm114_clean_objects" fmbl
fi
stage index $L -C -i "$ul/fm114_clean_indexes" --no-logging-index fmbl
# --trigger-file alone stops at the "no input file" check (usage); the trigger file is plain DDL, so -s loads it
stage trigger $L -C -s "$ul/fm114_clean_trigger" fmbl
cubrid paramdump -C fmbl > "$run/paramdump.out" 2>&1 || true
sp=$(server_pid $IDEV fmbl); [ -z "$sp" ] || grep -h Cpus_allowed_list /proc/$sp/status | sed "s/^/fmbl pid $sp /" >> "$run/meta.txt"
csql -u dba -C fmbl -i $BL/scripts/rows.sql > "$run/rows.out" 2>&1 || true
stage srvdev_stop bash $CTL stop fmbl
no_server_of $IDEV && stop_master $IDEV

echo "post: $(meta)" >> "$run/meta.txt"
grep -h -E -i 'error|fail' "$run"/schema.out "$run"/data.out "$run"/index.out "$run"/trigger.out > "$run/errors.txt" || true
echo "RUN $in $mode W=$W rep=$rep fail=$fail"
exit $fail
