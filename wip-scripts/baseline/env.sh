# Shared settings of the unload->load / cp baseline (map #472, compared against P1 #484). Sourced, not run.
BL=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline
I114=$BL/inst114            # reflink copy of /home/cubrid/release/CUBRID-fastmig-11.4.6 (11.4.6.1963)
IDEV=$BL/instdev            # reflink copy of /home/cubrid/release/CUBRID-fastmig-dev (develop 682b080b2 release)
FM=$BL/bin/cubrid-fastmig   # copy of the #494 binary that migrated T1 11.4 in its smoke (only to rewrite paths)
CTL=/home/cubrid/dev/workspace/.claude/skills/cubrid-server-control/scripts/cubrid-server-ctl.sh
PORT114=1708; PORTDEV=1711  # claimed in .git_ignored_dir/port-registry/claims.md (bl114, bldev)
export JAVA_HOME=/usr/lib/jvm/java-1.8.0-openjdk
BASEPATH=/usr/local/bin:/usr/bin:/bin
RES=$BL/results

# W CPUs on distinct physical cores at the top of NUMA node 1 (cpu31 downward; SMT siblings 48..63 left idle),
# the range other sessions use least (1번 세션 2026-10-29). The small runs used 16 / 16-19 / 16-31.
# The engine sizes worker pools (and the no-logging index build degree) from sched_getaffinity at boot,
# so the server is started under the same mask.
cpus_for() { case "$1" in 1) echo 31 ;; 4) echo 28-31 ;; 16) echo 16-31 ;; *) echo "bad W $1" >&2; return 1 ;; esac; }

# $1 = databases dir
use114() { export CUBRID=$I114 CUBRID_DATABASES=$1 CUBRID_TMP=/home/cubrid/.bltmp/114 PATH=$I114/bin:$BASEPATH LD_LIBRARY_PATH=$I114/lib; }
usedev() { export CUBRID=$IDEV CUBRID_DATABASES=$1 CUBRID_TMP=/home/cubrid/.bltmp/dev PATH=$IDEV/bin:$BASEPATH LD_LIBRARY_PATH=$IDEV/lib; }

conf114() {
  printf '[service]\nservice=server\n[common]\ncubrid_port_id=%s\ndata_buffer_size=512M\n' $PORT114 > $I114/conf/cubrid.conf
}
# $1 = W. loaddb_worker_count has a floor of 2 (system_parameter.c), so W=1 runs 2 workers on 1 CPU.
confdev() {
  local lw=$1; [ "$lw" -ge 2 ] || lw=2
  printf '[service]\nservice=server\n[common]\ncubrid_port_id=%s\ndata_buffer_size=512M\nstored_procedure=yes\nloaddb_worker_count=%s\nparallelism=%s\nindex_build_parallelism=%s\n' \
    $PORTDEV "$lw" "$1" "$1" > $IDEV/conf/cubrid.conf
}

# stop the cub_master of install $1 (the wrapper's stop leaves it; a new W needs a master born under the new mask)
stop_master() {
  local p n=0
  for p in $(pgrep -u "$(id -u)" -x cub_master || true); do
    [ "$(readlink /proc/$p/exe)" = "$1/bin/cub_master" ] && kill -TERM "$p" || true
  done
  while pgrep -u "$(id -u)" -x cub_master | while read -r p; do [ "$(readlink /proc/$p/exe)" = "$1/bin/cub_master" ] && echo x; done | grep -q x; do
    n=$((n + 1)); [ $n -le 60 ] || { echo "master of $1 did not stop" >&2; return 1; }; sleep 0.5
  done
}
no_server_of() {   # fails while any cub_server of install $1 lives
  local p
  for p in $(pgrep -u "$(id -u)" -x cub_server || true); do
    [ "$(readlink /proc/$p/exe)" != "$1/bin/cub_server" ] || { echo "cub_server $p of $1 still running" >&2; return 1; }
  done
}

now() { date +%s.%N; }
meta() { echo "$(date -Is) bl_du_mb=$(du -sm $BL | cut -f1) home_df=[$(df -h /home | awk 'NR==2{print $3" used "$4" avail "$5}')] load=$(cut -d' ' -f1-3 /proc/loadavg) memavail_kb=$(awk '/MemAvailable/{print $2}' /proc/meminfo) cached_kb=$(awk '/^Cached/{print $2}' /proc/meminfo)"; }
# pid of the cub_server serving DB $2 of install $1 (exe under the install, argv[1] = the DB name)
server_pid() {
  local p
  for p in $(pgrep -u "$(id -u)" -x cub_server || true); do
    [ "$(readlink /proc/$p/exe)" = "$1/bin/cub_server" ] && [ "$(tr '\0' ' ' < /proc/$p/cmdline | awk '{print $2}')" = "$2" ] && echo "$p"
  done
}

# One 5 s sample: "<busy on CPU list $1> <%util of /home's device nvme0n1>" (util = io_ticks delta / 5000 ms,
# field 13 of /proc/diskstats, the figure iostat reports as %util).
quiet_sample() {
  local ids; ids=$(echo "$1" | awk -F- '{ if (NF == 2) for (i = $1; i <= $2; i++) printf "cpu%d ", i; else printf "cpu%d ", $1 }')
  local a b da db; a=$(grep -E '^cpu[0-9]+ ' /proc/stat); da=$(awk '$3 == "nvme0n1" {print $13}' /proc/diskstats)
  sleep 5
  b=$(grep -E '^cpu[0-9]+ ' /proc/stat); db=$(awk '$3 == "nvme0n1" {print $13}' /proc/diskstats)
  awk -v ids="$ids" -v da="$da" -v db="$db" 'BEGIN { n = split(ids, x, " "); for (i = 1; i <= n; i++) want[x[i]] = 1 }
    { if (!($1 in want)) next; t = 0; for (i = 2; i <= NF; i++) t += $i; idle = $5 + $6
      if ($1 in T) { dt = t - T[$1]; if (dt > 0) busy += (dt - (idle - I[$1])) / dt } else { T[$1] = t; I[$1] = idle } }
    END { printf "%.2f %.1f", busy, (db - da) / 50.0 }' <(echo "$a") <(echo "$b")
}
