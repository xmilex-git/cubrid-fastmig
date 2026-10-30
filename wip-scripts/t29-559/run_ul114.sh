#!/usr/bin/env bash
# One official-path run to an 11.4 target: 11.4.6 unloaddb of the big 11.4 DB -> 11.4.6 createdb -> loaddb schema,
# data, index, trigger. Same steps as the stage-1 baseline (../../baseline/scripts/run_ul.sh, target develop), with
# the target install 11.4.6: no --no-logging-index and no parallelism/index_build_parallelism (11.4.6 has neither);
# the index file still loads in CS like there.
#   bash run_ul114.sh <mode: SA|CS> <W: 1|4|16> <rep>
# The source is a safe_copy.sh copy of the pristine (_vinf rewritten to the copy) at src/ul-<mode>-w<W>-r<rep>,
# made here; the driver removes it by literal path after the run. Every process runs under taskset on the W CPUs.
# Rows "mode W rep stage seconds rc" in results/ul114.tsv; logs in runs/ul-<mode>-w<W>-r<rep>/.
set -uo pipefail
. "$(dirname "$0")/env.sh"
mode=$1; W=$2; rep=$3
case "$mode" in SA) m=-S ;; CS) m=-C ;; *) exit 2 ;; esac
C=$(cpus_for "$W") || exit 2
P=${UL_P:-$P}; C=${UL_CPUS:-$C}   # UL_P / UL_CPUS: the smoke on T1 only
id=ul-$mode-w$W-r$rep
src=$S/src/$id; run=$S/runs/$id; tgt=$run/tgt; ul=$run/unload; tgtdbs=$S/tgtdbs
[ ! -e "$run" ] && [ ! -e "$src" ] || { echo "$run or $src exists" >&2; exit 1; }
mkdir -p "$ul" "$tgt" "$tgtdbs"; touch "$tgtdbs/databases.txt"
bash $SAFE_COPY "$P" "$src" > "$run/safe_copy.out" 2>&1 || { echo "safe_copy failed" >&2; exit 1; }
fail=0
# Every exit leaves at most the source copy (the driver removes it): servers stopped, unload files truncated
# (sizes kept in unload.ls), target DB deleted (a DB createdb made here, so deletedb is safe).
cleanup() {
  set +e
  if ! no_server_of $I114 2>/dev/null; then
    ( use114 "$tgtdbs"; bash $CTL stop fmbl > "$run/cleanup.stoptgt.out" 2>&1 )
    ( use114 "$src"; bash $CTL stop fm114_clean > "$run/cleanup.stopsrc.out" 2>&1 )
  fi
  stop_master $I114
  ls -l "$ul" > "$run/unload.ls" 2>/dev/null
  for f in "$ul"/fm114_clean_objects "$ul"/fm114_clean_schema "$ul"/fm114_clean_indexes "$ul"/fm114_clean_trigger; do
    [ ! -f "$f" ] || : > "$f"
  done
  [ ! -d "$tgt" ] || du -sm "$tgt" > "$run/tgt.du"
  if /usr/bin/grep -q '^fmbl[[:space:]]' "$tgtdbs/databases.txt" 2>/dev/null; then
    ( use114 "$tgtdbs"; cubrid deletedb fmbl > "$run/deletedb.out" 2>&1 ) || echo "deletedb fmbl failed" >&2
  fi
  echo "post-clean: $(meta)" >> "$run/meta.txt"
}
trap cleanup EXIT
trap 'exit 143' TERM INT
stage() {   # stage <name> <cmd...>: time one step, log to $run/<name>.out
  local name=$1; shift; local s e rc
  s=$(now); if "$@" > "$run/$name.out" 2>&1; then rc=0; else rc=$?; fi; e=$(now)
  printf '%s\t%s\t%s\t%s\t%.3f\t%s\n' "$mode" "$W" "$rep" "$name" "$(dt $s $e)" "$rc" >> $RES/ul114.tsv
  [ $rc -eq 0 ] || { echo "stage $name rc=$rc" >&2; fail=1; }
  return $rc
}
T="taskset -c $C"
{ echo "cpus=$C install=$(LD_LIBRARY_PATH=$I114/lib CUBRID=$I114 $I114/bin/cubrid_rel 2>/dev/null | /usr/bin/grep CUBRID)"; echo "src _vinf:"; cat "$src"/fm114_clean_vinf; echo "pre: $(meta)"; } > "$run/meta.txt"
/usr/bin/grep -qv "^ *-\?[0-9]* $src/" "$src"/fm114_clean_vinf && { echo "_vinf of $src names another dir" >&2; exit 1; }
quiet_gate "$C" "$W" "$run/meta.txt"
echo "heavy on 16-31 at start: $(heavy_on_measured_cpus | tr '\n' ';')" >> "$run/meta.txt"

# page cache not dropped (shared host): the source read once (warm), log archives never read
find "$src" -maxdepth 1 -type f -name 'fm114_clean*' ! -name '*_lgar*' -exec cat {} + > /dev/null

# ---- source side (11.4.6)
use114 "$src"; conf114 "$W"
if [ $mode = CS ]; then stage srcsrv_start $T bash $CTL start fm114_clean || exit 1; fi
stage unload bash -c "cd '$ul' && $T cubrid unloaddb $m -u dba -t $W fm114_clean"
if [ $mode = CS ]; then stage srcsrv_stop bash $CTL stop fm114_clean; no_server_of $I114 && stop_master $I114; fi
ls -l "$ul" > "$run/unload.ls"
[ $fail -eq 0 ] || exit 1

# ---- target side (11.4.6)
use114 "$tgtdbs"; conf114 "$W"
stage createdb bash -c "cd '$tgt' && $T cubrid createdb --db-volume-size=512M --log-volume-size=256M --file-path='$tgt' --log-path='$tgt' fmbl en_US.utf8" || exit 1
L="$T cubrid loaddb -u dba"
if [ $mode = SA ]; then
  stage schema $L -S -s "$ul/fm114_clean_schema" fmbl
  stage data $L -S -d "$ul/fm114_clean_objects" fmbl
  stage tgtsrv_start $T bash $CTL start fmbl || exit 1
else
  stage tgtsrv_start $T bash $CTL start fmbl || exit 1
  stage schema $L -C -s "$ul/fm114_clean_schema" fmbl
  stage data $L -C -d "$ul/fm114_clean_objects" fmbl
fi
stage index $L -C -i "$ul/fm114_clean_indexes" fmbl
# --trigger-file alone stops at the "no input file" check (usage); the trigger file is plain DDL, so -s loads it
stage trigger $L -C -s "$ul/fm114_clean_trigger" fmbl
cubrid paramdump -C fmbl > "$run/paramdump.out" 2>&1 || true
sp=$(server_pid $I114 fmbl); [ -z "$sp" ] || /usr/bin/grep -h Cpus_allowed_list /proc/$sp/status | sed "s/^/fmbl pid $sp /" >> "$run/meta.txt"
csql -u dba -C fmbl -i $S/scripts/rows.sql > "$run/rows.out" 2>&1 || true
stage tgtsrv_stop bash $CTL stop fmbl
no_server_of $I114 && stop_master $I114

echo "post: $(meta)" >> "$run/meta.txt"
/usr/bin/grep -h -E -i 'error|fail' "$run"/schema.out "$run"/data.out "$run"/index.out "$run"/trigger.out > "$run/errors.txt" || true
echo "RUN UL114 $mode W=$W rep=$rep fail=$fail"
exit $fail
