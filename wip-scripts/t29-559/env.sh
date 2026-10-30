# Shared settings of #559 (T29): the big 11.4 DB moved to 11.4, the official path (11.4.6 unloaddb -> 11.4.6 loaddb)
# and cubrid-fastmig 11.4 -> 11.4. Sourced, not run. Same rules as the stage-1 baseline (../../baseline/scripts/env.sh)
# and #484 (../../484/scripts/README.md): CPUs 16-31, quiet gate, page cache not dropped, log archives never read.
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/559
P=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/basedb-big/11.4/pristine/clean   # read only
SAFE_COPY=/home/cubrid/dev/cubrid-fastmig/basedb/safe_copy.sh
I114=$S/inst114             # reflink copy of /home/cubrid/release/CUBRID-fastmig-11.4.6 (11.4.6.1963), its conf is ours
FM=$S/bin/cubrid-fastmig    # release build of the measured cubrid-fastmig commit (meta.txt keeps its sha256)
CTL=/home/cubrid/dev/workspace/.claude/skills/cubrid-server-control/scripts/cubrid-server-ctl.sh
PORT=1708                   # claimed in .git_ignored_dir/port-registry/claims.md (fm559)
export JAVA_HOME=/usr/lib/jvm/java-1.8.0-openjdk
BASEPATH=/usr/local/bin:/usr/bin:/bin
RES=$S/results

cpus_for() { case "$1" in 1) echo 31 ;; 4) echo 28-31 ;; 16) echo 16-31 ;; *) echo "bad W $1" >&2; return 1 ;; esac; }

# $1 = databases dir
use114() { export CUBRID=$I114 CUBRID_DATABASES=$1 CUBRID_TMP=/home/cubrid/.bltmp/559 PATH=$I114/bin:$BASEPATH LD_LIBRARY_PATH=$I114/lib; mkdir -p $CUBRID_TMP; }

# $1 = W. 11.4.6 has loaddb_worker_count (floor 2, as develop) but neither parallelism nor index_build_parallelism
# nor loaddb --no-logging-index: those settings of the develop baseline have no 11.4 counterpart.
conf114() {
  local lw=$1; [ "$lw" -ge 2 ] || lw=2
  printf '[service]\nservice=server\n[common]\ncubrid_port_id=%s\ndata_buffer_size=512M\njava_stored_procedure=yes\nloaddb_worker_count=%s\n' \
    $PORT "$lw" > $I114/conf/cubrid.conf
}

stop_master() {   # stop the cub_master of install $1 (a new W needs a master born under the new mask)
  local p n=0
  for p in $(pgrep -u "$(id -u)" -x cub_master || true); do
    [ "$(readlink /proc/$p/exe)" = "$1/bin/cub_master" ] && kill -TERM "$p" || true
  done
  while pgrep -u "$(id -u)" -x cub_master | while read -r p; do [ "$(readlink /proc/$p/exe)" = "$1/bin/cub_master" ] && echo x; done | /usr/bin/grep -q x; do
    n=$((n + 1)); [ $n -le 60 ] || { echo "master of $1 did not stop" >&2; return 1; }; sleep 0.5
  done
}
no_server_of() {   # fails while any cub_server of install $1 lives
  local p
  for p in $(pgrep -u "$(id -u)" -x cub_server || true); do
    [ "$(readlink /proc/$p/exe)" != "$1/bin/cub_server" ] || { echo "cub_server $p of $1 still running" >&2; return 1; }
  done
}
server_pid() {    # pid of the cub_server of install $1 serving DB $2
  local p
  for p in $(pgrep -u "$(id -u)" -x cub_server || true); do
    [ "$(readlink /proc/$p/exe)" = "$1/bin/cub_server" ] && [ "$(tr '\0' ' ' < /proc/$p/cmdline | awk '{print $2}')" = "$2" ] && echo "$p"
  done
}

now() { date +%s.%N; }
dt() { awk -v a="$1" -v b="$2" 'BEGIN{print b-a}'; }
meta() { echo "$(date -Is) s_du_mb=$(du -sm $S | cut -f1) home_df=[$(df -h /home | awk 'NR==2{print $3" used "$4" avail "$5}')] load=$(cut -d' ' -f1-3 /proc/loadavg) memavail_kb=$(awk '/MemAvailable/{print $2}' /proc/meminfo) cached_kb=$(awk '/^Cached/{print $2}' /proc/meminfo)"; }

# "<busy CPUs of list $1> <%util of nvme0n1>" over 5 s (io_ticks delta / 5000 ms, as iostat %util)
quiet_sample() {
  local ids; ids=$(echo "$1" | awk -F- '{ if (NF == 2) for (i = $1; i <= $2; i++) printf "cpu%d ", i; else printf "cpu%d ", $1 }')
  local a b da db; a=$(/usr/bin/grep -E '^cpu[0-9]+ ' /proc/stat); da=$(awk '$3 == "nvme0n1" {print $13}' /proc/diskstats)
  sleep 5
  b=$(/usr/bin/grep -E '^cpu[0-9]+ ' /proc/stat); db=$(awk '$3 == "nvme0n1" {print $13}' /proc/diskstats)
  awk -v ids="$ids" -v da="$da" -v db="$db" 'BEGIN { n = split(ids, x, " "); for (i = 1; i <= n; i++) want[x[i]] = 1 }
    { if (!($1 in want)) next; t = 0; for (i = 2; i <= NF; i++) t += $i; idle = $5 + $6
      if ($1 in T) { dt = t - T[$1]; if (dt > 0) busy += (dt - (idle - I[$1])) / dt } else { T[$1] = t; I[$1] = idle } }
    END { printf "%.2f %.1f", busy, (db - da) / 50.0 }' <(echo "$a") <(echo "$b")
}
# quiet gate: busier than W/2 on the measured CPUs or nvme0n1 over 50 %util -> wait 60 s, at most twice, then run.
# Also records any process outside this run sitting on the measured CPUs (ps psr), for the overlap column.
quiet_gate() {   # quiet_gate <cpus> <W> <meta file>
  local try busy util
  for try in 0 1 2; do
    read -r busy util <<< "$(quiet_sample "$1")"
    echo "gate try=$try busy_on_cpus=$busy (limit $(awk -v w=$2 'BEGIN{print w/2}')) nvme0n1_util=$util% (limit 50) load1=$(cut -d' ' -f1 /proc/loadavg)" >> "$3"
    awk -v b="$busy" -v u="$util" -v w="$2" 'BEGIN{exit !(b > w / 2 || u > 50)}' || return 0
    [ $try -lt 2 ] && sleep 60 || echo "gate: still busy after 2 retries, run anyway" >> "$3"
  done
}
heavy_on_measured_cpus() {   # engine or fastmig processes of anyone on CPUs 16-31 right now
  ps -eo pid,psr,pcpu,comm | awk '$2 >= 16 && $2 <= 31 && $4 ~ /^(cub_|csql|cubrid|cargo|rustc|cc1|cc1plus|ld|cp|rsync)/'
}
# load average and big processes on CPUs 0-15 (1번 세션 2026-10-30: other sessions' load there during measurements)
load_and_0_15() {   # 1 s top sample: processes over 50 %CPU with their allowed CPUs
  echo "loadavg=$(cut -d" " -f1-3 /proc/loadavg) busy: $(top -b -n 2 -d 1 -w 200 | awk "/^top -/ {n++} n == 2 && \$1 ~ /^[0-9]+$/ && \$9 + 0 >= 50 {print \$1, \$12, \$9}" | while read -r p c u; do printf "%s/%s/%s%%/cpus=%s; " $p $c $u "$(awk '/Cpus_allowed_list/ {print $2}' /proc/$p/status 2>/dev/null)"; done)"
}
# load gate (1번 세션 2026-10-30, after the 12:28 run was spoiled by builds on CPUs 0-15, load 145): before the run and
# before every rep, wait until the 1-minute load average is at most 4, looking every 15 s; after 30 min give up (rc 3)
load_gate() {   # load_gate <meta file or -> ; prints nothing, records the wait
  local waited=0
  while awk '{exit !($1 > 4)}' /proc/loadavg; do
    [ $waited -lt 1800 ] || { echo "load gate: still over 4 after ${waited} s ($(cut -d' ' -f1-3 /proc/loadavg)), not run" >> "${1:-/dev/stderr}"; return 3; }
    sleep 15; waited=$((waited + 15))
  done
  echo "load gate: waited ${waited} s, load1=$(cut -d' ' -f1 /proc/loadavg)" >> "${1:-/dev/stderr}"
}
