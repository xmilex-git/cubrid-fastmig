#!/usr/bin/env bash
# One same-version copy run on a registered source DB, one method or all three:
#   cp_never   cp -a --reflink=never, then sync of every copied file (timed apart and together)
#   cp_reflink cp -a --reflink=always
#   copydb     cubrid copydb --copy-lob-path (SA, server down), then deletedb of the copy
#   bash run_cp.sh <input: small|big> <rep> [cp_never|cp_reflink|copydb]   (no method = all three)
# small = T1 develop clean (develop install), big = the rows-multiplied 11.4 DB (11.4 install; 11.4 -> 11.4).
# Log archives (*_lgar*) are left out of both cp methods (1번 세션 2026-10-29): the engine does not open them to
# boot, and neither unload->load nor fastmig reads them; the DB keeps them. cp.meta records both sizes.
# (The small runs before this rule copied the whole directory, archives included.)
# The cp copies are left in cp/<input>-r<rep>-{never,reflink}; the caller removes each by literal path before the
# next method, so at most the source and one copy exist at a time.
# Appends "input method rep stage seconds rc" rows to results/cp.tsv. cp runs under taskset on cpus_for 16.
set -uo pipefail
. "$(dirname "$0")/env.sh"
# pause point (1번 세션 2026-10-30): another session creates baseline/PAUSE before heavy I/O between runs
paused=0
while [ -e $BL/PAUSE ]; do sleep 30; paused=$((paused + 30)); done
[ $paused -eq 0 ] || echo "PAUSED $paused s before $(basename "$0") $*"
in=$1; rep=$2; only=${3:-all}
case "$in" in small) usedev $BL/in/smalldev/dbs; db=fmdev_clean; src=$BL/in/smalldev/db ;;
              big) use114 $BL/basedb-big/11.4; conf114; db=fm114_clean; src=$BL/basedb-big/11.4/clean ;; *) exit 2 ;; esac
[ $in = big ] || { confdev 16; }
d=$BL/cp/$in-r$rep; mkdir -p $BL/cp
T="taskset -c $(cpus_for 16)"
fail=0
row() { printf '%s\t%s\t%s\t%s\t%.3f\t%s\n' "$in" "$1" "$rep" "$2" "$3" "$4" >> $RES/cp.tsv; }
dt() { awk -v a="$1" -v b="$2" 'BEGIN{print b-a}'; }
echo "rep $rep $only pre: $(meta) cpus=$(cpus_for 16)" >> $RES/cp.meta.$in

mapfile -t items < <(ls -A "$src" | grep -v '_lgar')   # copied entries: everything but the log archives
paths=("${items[@]/#/$src/}")
echo "src $src: all $(du -sm "$src" | cut -f1) MB, without lgar $(du -smc "${paths[@]}" | tail -1 | cut -f1) MB; entries: ${items[*]}" >> $RES/cp.meta.$in
for p in "${paths[@]}"; do [ ! -f "$p" ] || cat "$p"; done > /dev/null   # warm, as in run_ul.sh

if [ $only = all ] || [ $only = cp_never ]; then
mkdir "$d-never"
s=$(now); $T cp -a --reflink=never "${paths[@]}" "$d-never/"; rc1=$?; m=$(now)
find "$d-never" -type f -print0 | xargs -0 $T sync; rc2=$?; e=$(now)
row cp_never cp "$(dt $s $m)" $rc1; row cp_never sync "$(dt $m $e)" $rc2; row cp_never total "$(dt $s $e)" $((rc1 | rc2))
du -sm "$d-never" >> $RES/cp.meta.$in
fi
if [ $only = all ] || [ $only = cp_reflink ]; then
mkdir "$d-reflink"
s=$(now); $T cp -a --reflink=always "${paths[@]}" "$d-reflink/"; rc=$?; e=$(now)
row cp_reflink total "$(dt $s $e)" $rc
du -sm "$d-reflink" >> $RES/cp.meta.$in
fi
if [ $only = all ] || [ $only = copydb ]; then
mkdir -p "$d-copydb"
s=$(now); $T cubrid copydb --copy-lob-path -F "$d-copydb" -L "$d-copydb" $db blcopy > "$d-copydb.out" 2>&1; rc=$?; e=$(now)
row copydb total "$(dt $s $e)" $rc
echo "copydb copied: $(du -sm "$d-copydb" | cut -f1) MB, $(ls "$d-copydb" | tr '\n' ' ')" >> $RES/cp.meta.$in
cubrid deletedb blcopy >> "$d-copydb.out" 2>&1 || fail=1
fi
echo "rep $rep $only post: $(meta)" >> $RES/cp.meta.$in
echo "CP $in rep=$rep $only fail=$fail"
exit $fail
