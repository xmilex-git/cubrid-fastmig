#!/usr/bin/env bash
# One 11.4 -> develop cubrid-fastmig run of 3번 세션's big 11.4 DB (the unload->load baseline's input), the
# cubrid-fastmig cell of the baseline table:
#   bash run_114.sh <method> <W: 1|4|16> <rep>
#   fm          migrate --target-version develop (clone where the filesystem can, the default)
#   fm_verify   the same with --verify (verify is opt-in since fastmig a3da243, ADR 0015)
#   fm_copy, fm_copy_verify   --no-clone (every volume byte-copied)
# Same rules as the baseline (baseline/scripts/env.sh, run_ul.sh): CPUs of cpus_for W (1: 31, 4: 28-31, 16: 16-31),
# --workers W, quiet gate (busy on those CPUs > W/2 or nvme0n1 > 50 %util: wait 60 s, at most 2 times, then run),
# page cache not dropped: the source files but the log archives are read once (warm) before every run.
# The source is src114/db, a reflink copy of baseline/basedb-big/11.4/pristine/clean without log archives that
# meas_114_big.sh makes (read with --volumes-in-first-dir; 3번 세션 keeps its registered DB); migrate only reads it.
# Output: the target stays in meas/<method>-w<W>-r<rep> (the caller removes it by literal path); rows
# "method W rep stage seconds rc" in results/u114.tsv from report.json "seconds" plus the wall total; per-run
# logs, report.json and /usr/bin/time -v in runs/114-<method>-w<W>-r<rep>/.
set -uo pipefail
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/484
SRC=${U114_SRC_DIR:-$S/src114/db}   # U114_SRC_DIR / U114_SRC_DBS / U114_CPUS: the smoke on T1 only
SRCDBS=${U114_SRC_DBS:-$S/src114/databases.txt}
FM=$S/bin/cubrid-fastmig
RES=$S/results
method=$1; W=$2; rep=$3
case "$W" in 1) C=31 ;; 4) C=28-31 ;; 16) C=16-31 ;; *) echo "bad W $W" >&2; exit 2 ;; esac
C=${U114_CPUS:-$C}
extra=()
case "$method" in fm|fm_verify) ;; fm_copy|fm_copy_verify) extra+=(--no-clone) ;; *) echo "bad method $method" >&2; exit 2 ;; esac
case "$method" in *_verify) extra+=(--verify) ;; esac
d=$S/meas/$method-w$W-r$rep; run=$S/runs/114-$method-w$W-r$rep
[ ! -e "$d" ] && [ ! -e "$run" ] || { echo "$d or $run exists" >&2; exit 1; }
mkdir -p "$run" $S/meas/tgtdbs
: > $S/meas/tgtdbs/databases.txt   # migrate registers each target here; one target at a time

# shared pause point with 3번 세션's baseline (1번 세션 2026-10-30): another session creates baseline/PAUSE before heavy I/O
paused=0
while [ -e /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/PAUSE ]; do sleep 30; paused=$((paused + 30)); done
[ $paused -eq 0 ] || echo "PAUSED $paused s before $(basename "$0") $*"

now() { date +%s.%N; }
dt() { awk -v a="$1" -v b="$2" 'BEGIN{print b-a}'; }
row() { printf '%s\t%s\t%s\t%s\t%.3f\t%s\n' "$method" "$W" "$rep" "$1" "$2" "$3" >> $RES/u114.tsv; }
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

{ echo "method=$method W=$W rep=$rep cpus=$C fm_sha256=$(sha256sum < $FM | cut -c1-16) extra=${extra[*]}"; echo "pre: $(meta)"; } > "$run/meta.txt"
for try in 0 1 2; do
  read -r busy util <<< "$(quiet_sample $C)"
  echo "gate try=$try busy_on_cpus=$busy (limit $(awk -v w=$W 'BEGIN{print w/2}')) nvme0n1_util=$util% (limit 50) load1=$(cut -d' ' -f1 /proc/loadavg)" >> "$run/meta.txt"
  awk -v b="$busy" -v u="$util" -v w="$W" 'BEGIN{exit !(b > w / 2 || u > 50)}' || break
  [ $try -lt 2 ] && sleep 60 || echo "gate: still busy after 2 retries, run anyway" >> "$run/meta.txt"
done

find "$SRC" -maxdepth 1 -type f -name 'fm114_clean*' ! -name '*_lgar*' -exec cat {} + > /dev/null   # warm, as run_ul.sh
s=$(now)
/usr/bin/time -v -o "$run/time.txt" taskset -c $C $FM migrate fm114_clean --dest "$d" --target-version develop \
  --target-name fm114_dev --workers $W --mem 16G --volumes-in-first-dir \
  --source-databases-txt $SRCDBS --target-databases-txt $S/meas/tgtdbs/databases.txt \
  --progress plain --progress-interval 30 "${extra[@]}" > "$run/migrate.out" 2> "$run/migrate.err"
rc=$?; e=$(now)
cp "$d/fastmig/report.json" "$run/" 2>/dev/null
cp "$d/fastmig/verify.jsonl" "$run/" 2>/dev/null
if [ -f "$run/report.json" ]; then
  jq -r '.seconds | to_entries[] | "\(.key)\t\(.value)"' "$run/report.json" | while read -r k v; do row "$k" "$v" $rc; done
  jq -c '{volumes: [.volumes[] | {volid, method, bytes}], added_volumes, heap_peak_bytes, catalog}' "$run/report.json" >> "$run/meta.txt"
fi
row total "$(dt $s $e)" $rc
/usr/bin/grep -E 'Maximum resident' "$run/time.txt" >> "$run/meta.txt"
du -sm "$d" >> "$run/meta.txt" 2>&1
echo "post: $(meta) rc=$rc" >> "$run/meta.txt"
echo "U114 $method W=$W rep=$rep rc=$rc"
exit $rc
