# Shared settings of #561 (T31): the ~2 GB 11.4 and develop DBs on three storage kinds. Sourced, not run.
# Rules (1번 세션 2026-10-30): CPUs 16-31 for the measured process (W1: 31, W16: 16-31), cold source (per-file
# fadvise DONTNEED, checked with fincore; no global drop_caches), every target and copy removed by literal path.
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/561
RES=$S/results
FM=${FM_BIN:-$S/bin/cubrid-fastmig}
BASEPATH=/usr/local/bin:/usr/bin:/bin
P114=$S/basedb2g/11.4/pristine/clean
PDEV=$S/basedb2g/develop/pristine/clean

# device dir of a storage kind: sources src114/ srcdev/ and targets meas/ live there
devdir() { case "$1" in nvme) echo $S/dev ;; ssd) echo /bench/ssd/fastmig-scratch/561 ;; hdd) echo /bench/hdd/fastmig-scratch/561 ;; *) return 1 ;; esac; }
blockdev() { case "$1" in nvme) echo nvme0n1 ;; ssd) echo sdb ;; hdd) echo sdc ;; esac; }
cpus_for() { case "$1" in 1) echo 31 ;; 4) echo 28-31 ;; 16) echo 16-31 ;; *) echo "bad W $1" >&2; return 1 ;; esac; }

now() { date +%s.%N; }
dt() { awk -v a="$1" -v b="$2" 'BEGIN{printf "%.3f", b-a}'; }
meta() { echo "$(date -Is) load=$(cut -d' ' -f1-3 /proc/loadavg) memavail_kb=$(awk '/MemAvailable/{print $2}' /proc/meminfo) dirty_kb=$(awk '/^Dirty:/{print $2}' /proc/meminfo)"; }
heavy_on_measured_cpus() { ps -eo pid,psr,pcpu,comm | awk '$2 >= 16 && $2 <= 31 && $3 > 20 {print $1"/"$4"/"$3"%"}' | tr '\n' ' '; }

# cold: write back and drop the page cache of every file under $1, then print the pages still cached (want 0)
evict() {
  find "$1" -type f -print0 | xargs -0 -r sync
  find "$1" -type f -print0 | xargs -0 -r -n1 dd iflag=nocache count=0 status=none if
  find "$1" -type f -print0 | xargs -0 -r fincore -b -n -o RES 2>/dev/null | awk '{s += $1} END {print s + 0}'
}

# 1 s samples of sectors read/written of the three devices into $1 until killed
sample_diskstats() {
  while :; do
    awk -v t="$(date +%s.%N)" '$3 == "nvme0n1" || $3 == "sdb" || $3 == "sdc" {print t, $3, $6, $10}' /proc/diskstats
    sleep 1
  done >> "$1"
}
