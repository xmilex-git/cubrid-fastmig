#!/usr/bin/env bash
# One run of a #559 comparison (scope set by the user 2026-10-30, 1번 세션), W=16 on CPUs 16-31:
#   (A) 11.4 -> develop: the big 11.4 DB, cubrid-fastmig against the official path of #484 (cited, not run here)
#   (B) develop -> develop: the big develop DB, cubrid-fastmig against cp
#   bash run_fm.sh <A|B> <method> <rep>
#   cp_a        (B) cp -a (coreutils 8.30: a byte copy, no reflink), then sync of every copied file
#   cp_reflink  (B) cp -a --reflink=always (the clone reference)
#   fm_clone    migrate --target-version develop (clone where the filesystem can, the default)
#   fm_copy     the same with --no-clone (every volume byte-copied)
# rep 0 is the cold run: the first read of a fresh source copy (page cache not dropped on this shared host, so "cold"
# means this copy's files were never read); reps 1.. read the source once before the run (warm), as #484 and the
# official-path baseline did. The source is src<A|B>/db, a reflink copy of the pristine without log archives that the
# driver makes (read with --volumes-in-first-dir); nothing here writes it.
# Output: the copy or target stays in meas/<path>-<method>-r<rep> (the driver removes it by literal path); rows
# "path method rep stage seconds rc" in results/fm.tsv (fastmig: report.json "seconds" plus the wall total); per-run
# logs, report.json and /usr/bin/time -v in runs/<path>-<method>-r<rep>/.
set -uo pipefail
. "$(dirname "$0")/env.sh"
path=$1; method=$2; rep=$3
case "$path" in A) db=fm114_clean ;; B) db=fmdev_clean ;; *) echo "bad path $path" >&2; exit 2 ;; esac
SRC=${FM_SRC_DIR:-$S/src$path/db}          # FM_SRC_DIR / FM_SRC_DBS / FM_CPUS / FM_BIN: the smoke only
SRCDBS=${FM_SRC_DBS:-$S/src$path/databases.txt}
FMB=${FM_BIN:-$FM}
C=${FM_CPUS:-16-31}; W=16
id=$path-$method-r$rep; d=$S/meas/$id; run=$S/runs/$id
[ ! -e "$d" ] && [ ! -e "$run" ] || { echo "$d or $run exists" >&2; exit 1; }
mkdir -p "$run" $S/meas/tgtdbs
: > $S/meas/tgtdbs/databases.txt   # migrate registers each target here; one target at a time
row() { printf '%s\t%s\t%s\t%s\t%.3f\t%s\n' "$path" "$method" "$rep" "$1" "$2" "$3" >> $RES/fm.tsv; }

{ echo "path=$path method=$method rep=$rep cpus=$C workers=$W fm_sha256=$(sha256sum < $FMB | cut -c1-16) fm=$FMB"; echo "pre: $(meta)"; } > "$run/meta.txt"
load_gate "$run/meta.txt" || { echo "FM $path $method rep=$rep skipped: load"; exit 3; }
quiet_gate "$C" "$W" "$run/meta.txt"
echo "heavy on 16-31 at start: $(heavy_on_measured_cpus | tr '\n' ';')" >> "$run/meta.txt"
echo "start $(load_and_0_15)" >> "$run/meta.txt"

mapfile -t items < <(ls -A "$SRC" | /usr/bin/grep -v '_lgar')
paths=("${items[@]/#/$SRC/}")
if [ "$rep" = 0 ]; then echo "cold: source not read before this run" >> "$run/meta.txt"
else for p in "${paths[@]}"; do [ ! -f "$p" ] || cat "$p"; done > /dev/null; fi   # warm
echo "src without lgar: $(du -smc "${paths[@]}" | tail -1 | cut -f1) MB; entries: ${items[*]}" >> "$run/meta.txt"
T="taskset -c $C"
rc=0
case "$method" in
  cp_a)
    mkdir "$d"
    s=$(now); $T cp -a "${paths[@]}" "$d/"; rc1=$?; m=$(now)
    find "$d" -type f -print0 | xargs -0 $T sync; rc2=$?; e=$(now)
    row cp "$(dt $s $m)" $rc1; row sync "$(dt $m $e)" $rc2; rc=$((rc1 | rc2)); row total "$(dt $s $e)" $rc ;;
  cp_reflink)
    mkdir "$d"
    s=$(now); $T cp -a --reflink=always "${paths[@]}" "$d/"; rc=$?; e=$(now)
    row total "$(dt $s $e)" $rc ;;
  fm_clone|fm_copy)
    extra=(); [ $method = fm_copy ] && extra+=(--no-clone)
    s=$(now)
    /usr/bin/time -v -o "$run/time.txt" $T $FMB migrate $db --dest "$d" --target-version develop \
      --target-name fm_m --workers $W --mem 16G --volumes-in-first-dir \
      --source-conf $S/scripts/fm.conf --target-conf $S/scripts/fm.conf \
      --source-databases-txt $SRCDBS --target-databases-txt $S/meas/tgtdbs/databases.txt \
      --progress plain --progress-interval 10 "${extra[@]}" > "$run/migrate.out" 2> "$run/migrate.err"
    rc=$?; e=$(now)
    cp "$d/fastmig/report.json" "$run/" 2>/dev/null
    if [ -f "$run/report.json" ]; then
      jq -r '.seconds | to_entries[] | "\(.key)\t\(.value)"' "$run/report.json" | while read -r k v; do row "$k" "$v" $rc; done
      jq -c '{version_pair, volumes: [.volumes[] | {volid, method, bytes}], added_volumes, heap_peak_bytes, catalog}' "$run/report.json" >> "$run/meta.txt"
    fi
    row total "$(dt $s $e)" $rc
    /usr/bin/grep -E 'Maximum resident' "$run/time.txt" >> "$run/meta.txt" ;;
  *) echo "bad method $method" >&2; exit 2 ;;
esac
du -sm "$d" >> "$run/meta.txt" 2>&1
echo "end $(load_and_0_15)" >> "$run/meta.txt"
echo "post: $(meta) rc=$rc" >> "$run/meta.txt"
echo "FM $path $method rep=$rep rc=$rc"
exit $rc
