#!/usr/bin/env bash
# One migrate run for workspace#560 (start estimate vs actual). One run at a time: fixed work dirs, removed at the end.
#   bash run_est.sh <src:t114|tdev|b114|bdev> <workers> <verify:0|1> <tag> <cpus> <binary>
# The source is a reflink copy (no log archives) of the pristine, read with --volumes-in-first-dir; nothing opens it
# with the engine. Evidence goes to runs/<tag>/ (report.json, stderr with the plain progress lines, time -v).
set -uo pipefail
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/560
src=$1; W=$2; ver=$3; tag=$4; C=$5; FMB=$6
case "$src" in
  t114) P=/home/cubrid/fastmig-basedb/11.4/pristine/clean; db=fm114_clean ;;
  tdev) P=/home/cubrid/fastmig-basedb/develop/pristine/clean; db=fmdev_clean ;;
  b114) P=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/basedb-big/11.4/pristine/clean; db=fm114_clean ;;
  bdev) P=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/basedb-big/develop/pristine/clean; db=fmdev_clean ;;
  *) echo "bad src $src" >&2; exit 2 ;;
esac
run=$S/runs/$tag
[ ! -e $S/work ] && [ ! -e "$run" ] || { echo "$S/work or $run exists" >&2; exit 1; }
mkdir -p "$run" $S/work/src/db $S/work/tgtdbs
mapfile -t items < <(ls -A "$P" | /usr/bin/grep -v _lgar)
cp -a --reflink=always "${items[@]/#/$P/}" $S/work/src/db/
D=$S/work/src/db
printf '%s\t%s\tlocalhost\t%s\tfile:%s/lob\n' $db $D $D $D > $S/work/src/databases.txt
: > $S/work/tgtdbs/databases.txt
for f in "${items[@]}"; do [ ! -f "$D/$f" ] || cat "$D/$f"; done > /dev/null   # warm, as #484 and #559
extra=(); [ "$ver" = 1 ] && extra+=(--verify)
echo "src=$src W=$W verify=$ver cpus=$C fm_sha=$(sha256sum < $FMB | cut -c1-16) start=$(date -Is) load=$(cut -d' ' -f1-3 /proc/loadavg)" > "$run/meta.txt"
echo "t_start=$(date +%s.%N)" >> "$run/meta.txt"
/usr/bin/time -v -o "$run/time.txt" taskset -c $C $FMB migrate $db --dest $S/work/dest --target-version develop \
  --target-name fm_m --workers $W --mem 16G --volumes-in-first-dir \
  --source-conf $S/../559/scripts/fm.conf --target-conf $S/../559/scripts/fm.conf \
  --source-databases-txt $S/work/src/databases.txt --target-databases-txt $S/work/tgtdbs/databases.txt \
  --progress plain --progress-interval 2 "${extra[@]}" > "$run/out.txt" \
  2> >(while IFS= read -r l; do printf '%s %s\n' "$(date +%s.%N)" "$l"; done > "$run/err.txt")
rc=$?
sleep 1   # the stamping reader drains
cp $S/work/dest/fastmig/report.json "$run/" 2>/dev/null
echo "end=$(date -Is) rc=$rc" >> "$run/meta.txt"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/560/work
jq -c '{e: .progress.estimated_total_s_at_start, a: .progress.actual_total_s}' "$run/report.json" 2>/dev/null
echo "RUN $tag rc=$rc"
exit $rc
