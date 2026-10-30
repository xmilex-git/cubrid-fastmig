#!/usr/bin/env bash
# One run of #561: bash run_one.sh <dev> <path> <method> <W> <rep>
#   dev     nvme | ssd | hdd (source and target on that device) | nvme2hdd (source NVMe, target HDD)
#   path    A = 11.4 -> develop (fm114_clean), B = develop -> develop (fmdev_clean)
#   method  fm (clone where the filesystem can, the default) | fm_copy (--no-clone) | cp_a (cp -a + sync) | cp_reflink
# Cold: the source's page cache is dropped file by file before the run (env.sh evict; cached pages logged).
# Output: target in <devdir>/meas/<id> (the driver removes it by literal path); rows
# "dev path method W rep stage seconds rc" in results/runs.tsv; per run in runs/<id>/: meta.txt, migrate.out/err,
# report.json, diskstats.log, phase_io.tsv (bytes per phase and device).
set -uo pipefail
. "$(dirname "$0")/env.sh"
dev=$1; path=$2; method=$3; W=$4; rep=$5
case "$path" in A) db=fm114_clean; sname=src114 ;; B) db=fmdev_clean; sname=srcdev ;; *) echo "bad path" >&2; exit 2 ;; esac
case "$dev" in nvme2hdd) sdir=$(devdir nvme); tdir=$(devdir hdd) ;; *) sdir=$(devdir $dev) && tdir=$sdir || exit 2 ;; esac
SRC=$sdir/$sname/db; SRCDBS=$sdir/$sname/databases.txt
id=$dev-$path-$method-w$W-r$rep; d=$tdir/meas/$id; run=$S/runs/$id
[ ! -e "$d" ] && [ ! -e "$run" ] || { echo "$d or $run exists" >&2; exit 1; }
mkdir -p "$run" $tdir/meas/tgtdbs
: > $tdir/meas/tgtdbs/databases.txt
C=$(cpus_for $W) || exit 2
row() { printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$dev" "$path" "$method" "$W" "$rep" "$1" "$2" "$3" >> $RES/runs.tsv; }

{ echo "id=$id src=$SRC target=$d cpus=$C fm_sha256=$(sha256sum < $FM | cut -c1-16)"; echo "pre: $(meta)"; } > "$run/meta.txt"
touch "$run/start.stamp"
echo "cached source pages after evict: $(evict "$SRC")" >> "$run/meta.txt"
echo "heavy on 16-31 at start: $(heavy_on_measured_cpus)" >> "$run/meta.txt"
taskset -c 0-15 bash -c '. "$0"; sample_diskstats "$1"' "$(dirname "$0")/env.sh" "$run/diskstats.log" &
sampler=$!
trap 'kill $sampler 2>/dev/null' EXIT
sleep 1.2
T="taskset -c $C"
mapfile -t items < <(ls -A "$SRC")
rc=0
case "$method" in
  cp_a|cp_reflink)
    mkdir "$d"
    flag=(); [ $method = cp_reflink ] && flag=(--reflink=always)
    s=$(now); $T cp -a "${flag[@]}" "${items[@]/#/$SRC/}" "$d/"; rc1=$?; m=$(now)
    find "$d" -type f -print0 | xargs -0 $T sync; rc2=$?; e=$(now)
    row cp "$(dt $s $m)" $rc1; row sync "$(dt $m $e)" $rc2; rc=$((rc1 | rc2)); row total "$(dt $s $e)" $rc
    printf 'copy:%s,sync:%s' "$(dt $s $m)" "$(dt $m $e)" > "$run/phases.txt"; start=$s ;;
  fm|fm_copy)
    extra=(); [ $method = fm_copy ] && extra+=(--no-clone)
    s=$(now)
    /usr/bin/time -v -o "$run/time.txt" $T $FM migrate $db --dest "$d" --target-version develop \
      --target-name fm_m --workers $W --mem 16G --volumes-in-first-dir \
      --source-conf $S/scripts/fm.conf --target-conf $S/scripts/fm.conf \
      --source-databases-txt $SRCDBS --target-databases-txt $tdir/meas/tgtdbs/databases.txt \
      --progress plain --progress-interval 5 "${extra[@]}" > "$run/migrate.out" 2> "$run/migrate.err"
    rc=$?; e=$(now)
    cp "$d/fastmig/report.json" "$run/" 2>/dev/null
    if [ -f "$run/report.json" ]; then
      jq -r '.progress.phases[] | "\(.name)\t\(.seconds)"' "$run/report.json" | while IFS=$'\t' read -r k v; do row "$k" "$v" $rc; done
      row est_start "$(jq -r '.progress.estimated_total_s_at_start' "$run/report.json")" $rc
      jq -r '[.progress.phases[] | "\(.name):\(.seconds)"] | join(",")' "$run/report.json" > "$run/phases.txt"
      jq -c '{volumes: [.volumes[] | {volid, method, bytes}], heap_peak_bytes, temp_disk}' "$run/report.json" >> "$run/meta.txt"
      start=$(awk -v e="$e" -v a="$(jq -r '.progress.actual_total_s' "$run/report.json")" 'BEGIN{printf "%.3f", e - a}')
    fi
    row total "$(dt $s $e)" $rc ;;
  *) echo "bad method" >&2; exit 2 ;;
esac
sleep 1.2; kill $sampler 2>/dev/null; wait $sampler 2>/dev/null
# bytes per phase and device: cumulative sectors interpolated at the phase boundaries (phases run back to back)
[ -f "$run/phases.txt" ] && awk -v start="${start:-0}" -v phases="$(cat "$run/phases.txt")" '
  { t[$2, ++n[$2]] = $1; r[$2, n[$2]] = $3; w[$2, n[$2]] = $4; devs[$2] = 1 }
  function at(dv, x, which,   i, k) {
    for (i = 1; i <= n[dv]; i++) if (t[dv, i] >= x) break
    if (i > n[dv]) i = n[dv]
    if (i == 1) return which == "r" ? r[dv, 1] : w[dv, 1]
    k = (x - t[dv, i - 1]) / (t[dv, i] - t[dv, i - 1]); if (k > 1) k = 1
    return which == "r" ? r[dv, i - 1] + (r[dv, i] - r[dv, i - 1]) * k : w[dv, i - 1] + (w[dv, i] - w[dv, i - 1]) * k
  }
  END {
    np = split(phases, ph, ","); t0 = start
    for (p = 1; p <= np; p++) {
      split(ph[p], kv, ":"); t1 = t0 + kv[2]
      for (dv in devs) printf "%s\t%s\t%s\t%.0f\t%.0f\n", kv[1], kv[2], dv, (at(dv, t1, "r") - at(dv, t0, "r")) * 512, (at(dv, t1, "w") - at(dv, t0, "w")) * 512
      t0 = t1
    }
  }' "$run/diskstats.log" > "$run/phase_io.tsv"
du -sm "$d" >> "$run/meta.txt" 2>&1
echo "new files in /bench/hdd/core during the run: $(find /bench/hdd/core -newer "$run/start.stamp" -type f 2>/dev/null | wc -l)" >> "$run/meta.txt"
echo "post: $(meta) rc=$rc" >> "$run/meta.txt"
echo "RUN $id rc=$rc"
exit $rc
