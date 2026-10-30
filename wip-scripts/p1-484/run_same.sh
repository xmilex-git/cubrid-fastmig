#!/usr/bin/env bash
# One same-version (develop -> develop) copy run of the big develop DB, one method:
#   cp_a        cp -a (coreutils 8.30 here: a byte copy, no reflink), then sync of every copied file
#   cp_reflink  cp -a --reflink=always (the clone reference)
#   fm_copy     cubrid-fastmig migrate --target-version develop --no-clone (copy mode: every volume byte-copied)
#   fm_clone    cubrid-fastmig migrate --target-version develop (clone where the filesystem can; shown apart)
#   fm_copy_verify, fm_clone_verify   the same with --verify (verify is opt-in since fastmig a3da243, ADR 0015)
#   bash run_same.sh <method> <rep>
# Rules of the unload->load baseline (baseline/scripts/env.sh, run_cp.sh): CPUs 16-31 (taskset; fastmig --workers 16),
# quiet gate (busy on those CPUs > 8 or nvme0n1 > 50 %util: wait 60 s, at most 2 times, then run), page cache not
# dropped (shared host): the source files are read once (warm) before every run, log archives (*_lgar*) are left out
# of the cp methods and never read by fastmig.
# The source is the registered big develop DB (484/basedb-big/develop/clean, == its pristine bytes); it is only read.
# Output: the copy stays in meas/<method>-r<rep> (the caller removes it by literal path before the next run);
# rows "method rep stage seconds rc" in results/same.tsv (fastmig stages from its report.json "seconds"),
# per-run logs and report.json in runs/<method>-r<rep>/.
set -uo pipefail
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/484
SRC=${SAME_SRC_DIR:-$S/basedb-big/develop/clean}   # SAME_SRC_* / SAME_DB / SAME_CPUS: the smoke on T1 only
SRCDBS=${SAME_SRC_DBS:-$S/basedb-big/develop/databases.txt}
FM=$S/bin/cubrid-fastmig
RES=$S/results
C=${SAME_CPUS:-16-31}; W=16; DB=${SAME_DB:-fmdev_clean}
method=$1; rep=$2
d=$S/meas/$method-r$rep; run=$S/runs/$method-r$rep
[ ! -e "$d" ] && [ ! -e "$run" ] || { echo "$d or $run exists" >&2; exit 1; }
mkdir -p "$run" $S/meas/tgtdbs
: > $S/meas/tgtdbs/databases.txt   # migrate registers each passing target here; one target at a time

# shared pause point with 3번 세션's baseline (1번 세션 2026-10-30): another session creates baseline/PAUSE before heavy I/O
paused=0
while [ -e /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/PAUSE ]; do sleep 30; paused=$((paused + 30)); done
[ $paused -eq 0 ] || echo "PAUSED $paused s before $(basename "$0") $*"

now() { date +%s.%N; }
dt() { awk -v a="$1" -v b="$2" 'BEGIN{print b-a}'; }
row() { printf '%s\t%s\t%s\t%.3f\t%s\n' "$method" "$rep" "$1" "$2" "$3" >> $RES/same.tsv; }
meta() { echo "$(date -Is) df=[$(df -h /home | awk 'NR==2{print $3" used "$4" avail"}')] load=$(cut -d' ' -f1-3 /proc/loadavg) memavail_kb=$(awk '/MemAvailable/{print $2}' /proc/meminfo) cached_kb=$(awk '/^Cached/{print $2}' /proc/meminfo)"; }
quiet_sample() {   # baseline/scripts/env.sh: "<busy CPUs of $1> <%util of nvme0n1>" over 5 s
  local ids; ids=$(echo "$1" | awk -F- '{ if (NF == 2) for (i = $1; i <= $2; i++) printf "cpu%d ", i; else printf "cpu%d ", $1 }')
  local a b da db; a=$(/usr/bin/grep -E '^cpu[0-9]+ ' /proc/stat); da=$(awk '$3 == "nvme0n1" {print $13}' /proc/diskstats)
  sleep 5
  b=$(/usr/bin/grep -E '^cpu[0-9]+ ' /proc/stat); db=$(awk '$3 == "nvme0n1" {print $13}' /proc/diskstats)
  awk -v ids="$ids" -v da="$da" -v db="$db" 'BEGIN { n = split(ids, x, " "); for (i = 1; i <= n; i++) want[x[i]] = 1 }
    { if (!($1 in want)) next; t = 0; for (i = 2; i <= NF; i++) t += $i; idle = $5 + $6
      if ($1 in T) { dt = t - T[$1]; if (dt > 0) busy += (dt - (idle - I[$1])) / dt } else { T[$1] = t; I[$1] = idle } }
    END { printf "%.2f %.1f", busy, (db - da) / 50.0 }' <(echo "$a") <(echo "$b")
}

{ echo "method=$method rep=$rep cpus=$C workers=$W fm_sha256=$(sha256sum < $FM | cut -c1-16)"; echo "pre: $(meta)"; } > "$run/meta.txt"
for try in 0 1 2; do
  read -r busy util <<< "$(quiet_sample $C)"
  echo "gate try=$try busy_on_cpus=$busy (limit $((W / 2))) nvme0n1_util=$util% (limit 50) load1=$(cut -d' ' -f1 /proc/loadavg)" >> "$run/meta.txt"
  awk -v b="$busy" -v u="$util" -v w="$W" 'BEGIN{exit !(b > w / 2 || u > 50)}' || break
  [ $try -lt 2 ] && sleep 60 || echo "gate: still busy after 2 retries, run anyway" >> "$run/meta.txt"
done

mapfile -t items < <(ls -A "$SRC" | /usr/bin/grep -v '_lgar')
paths=("${items[@]/#/$SRC/}")
for p in "${paths[@]}"; do [ ! -f "$p" ] || cat "$p"; done > /dev/null   # warm
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
  fm_copy|fm_clone|fm_copy_verify|fm_clone_verify)
    extra=(); case $method in fm_copy*) extra+=(--no-clone) ;; esac; case $method in *_verify) extra+=(--verify) ;; esac
    s=$(now)
    $T $FM migrate $DB --dest "$d" --target-version develop --target-name fmdev_m --workers $W --mem 16G \
      --source-databases-txt $SRCDBS --target-databases-txt $S/meas/tgtdbs/databases.txt \
      --progress plain --progress-interval 10 "${extra[@]}" > "$run/migrate.out" 2> "$run/migrate.err"
    rc=$?; e=$(now)
    cp "$d/fastmig/report.json" "$run/" 2>/dev/null
    cp "$d/fastmig/verify.jsonl" "$run/" 2>/dev/null
    if [ -f "$run/report.json" ]; then
      jq -r '.seconds | to_entries[] | "\(.key)\t\(.value)"' "$run/report.json" | while read -r k v; do row "$k" "$v" $rc; done
      jq -c '.volumes' "$run/report.json" >> "$run/meta.txt"
    fi
    row total "$(dt $s $e)" $rc ;;
  *) echo "bad method $method" >&2; exit 2 ;;
esac
du -sm "$d" >> "$run/meta.txt" 2>&1
echo "post: $(meta) rc=$rc" >> "$run/meta.txt"
echo "SAME $method rep=$rep rc=$rc"
exit $rc
