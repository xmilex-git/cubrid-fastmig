#!/usr/bin/env bash
# Quiet gate of a measured run: the same gate as the unload->load baseline (workspace#484, map supervisor 2026-10-29).
#
# Input: bash e2e/quiet_gate.sh <cpu list "a" or "a-b">. Output: one "gate ..." line per try on stdout.
# Rule: if the measured CPUs are busier than (CPU count)/2 over 5 s, or /home's device (nvme0n1) is over 50 %util,
# wait 60 s and look again, at most 2 times; then the run goes ahead and the lines stay in the run's meta.txt.
cpus=$1
ids=$(echo "$cpus" | awk -F- '{ if (NF == 2) for (i = $1; i <= $2; i++) printf "cpu%d ", i; else printf "cpu%d ", $1 }')
n=$(echo "$ids" | wc -w)
sample() {   # "<busy CPUs> <%util>": /proc/stat and /proc/diskstats field 13 (io_ticks) deltas over 5 s
  local a b da db
  a=$(grep -E '^cpu[0-9]+ ' /proc/stat); da=$(awk '$3 == "nvme0n1" { print $13 }' /proc/diskstats)
  sleep 5
  b=$(grep -E '^cpu[0-9]+ ' /proc/stat); db=$(awk '$3 == "nvme0n1" { print $13 }' /proc/diskstats)
  awk -v ids="$ids" -v da="$da" -v db="$db" 'BEGIN { n = split(ids, x, " "); for (i = 1; i <= n; i++) want[x[i]] = 1 }
    { if (!($1 in want)) next; t = 0; for (i = 2; i <= NF; i++) t += $i; idle = $5 + $6
      if ($1 in T) { dt = t - T[$1]; if (dt > 0) busy += (dt - (idle - I[$1])) / dt } else { T[$1] = t; I[$1] = idle } }
    END { printf "%.2f %.1f", busy, (db - da) / 50.0 }' <(echo "$a") <(echo "$b")
}
for try in 0 1 2; do
  read -r busy util <<<"$(sample)"
  echo "gate try=$try busy_on_cpus=$busy (limit $(awk -v n="$n" 'BEGIN { print n / 2 }')) nvme0n1_util=$util% (limit 50) load1=$(cut -d' ' -f1 /proc/loadavg)"
  awk -v b="$busy" -v u="$util" -v n="$n" 'BEGIN { exit !(b > n / 2 || u > 50) }' || exit 0
  if [ $try -lt 2 ]; then sleep 60; else echo "gate: still busy after 2 retries, run anyway"; fi
done
