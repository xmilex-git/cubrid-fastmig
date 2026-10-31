#!/usr/bin/env bash
# One side of the long run (e2e/ops/longrun/README.md): prepare the side as the T36 runner does (e2e/ops/sides.sh),
# keep its server up for CYCLES cycles of paced DML and hourly operations (cycle.sh), then restore and PITR at the end
# (recovery.sh). Started by e2e/ops/longrun/run.sh, one process per path and side, all at once.
#
# Input: bash e2e/ops/longrun/side_loop.sh <bin> <pristine dir> <target version> <side out dir> <migrated|official>
#   <side out dir> is new (sides.sh ops_setup); run.sh passes <out>/<path>/<side>.
# env: FASTMIG_OPS_INSTALL (this side's own install copy), FASTMIG_OPS_PORT, FASTMIG_OPS_SERVER_CTL (required);
#   FASTMIG_LONGRUN_CYCLES (8), FASTMIG_LONGRUN_ROUNDS (30 per cycle), FASTMIG_LONGRUN_CYCLE_SECONDS (3600),
#   FASTMIG_LONGRUN_KILL_EVERY (4), FASTMIG_LONGRUN_PITR_CYCLE (CYCLES - 2, at least 1), FASTMIG_LONGRUN_MIN_ARCHIVES
#   (50), FASTMIG_LONGRUN_MIN_CHECKPOINTS (100), FASTMIG_LONGRUN_STAT_SECONDS (60, the statdump window),
#   FASTMIG_LONGRUN_MAX_ARCHIVES (40, log_max_archives),
#   FASTMIG_LONGRUN_BREAK (pitr-extra-row), FASTMIG_OPS_WORKERS (4).
# Output: <side out>/steps.tsv, <side out>/<side>/{hours.tsv,times.tsv,length.tsv,recovery.tsv,faults.tsv,rc,check/,
#   log/,stat/,recovery/,server-log/,cores.txt}. Exit 0 pass, 1 fail, 2 bad arguments.
# Source: T39 workspace#594; G12 workspace#587; schedule approved by the map supervisor 2026-10-31.
set -uo pipefail

S=${5:?side}
CYCLES=${FASTMIG_LONGRUN_CYCLES:-8}
ROUNDS=${FASTMIG_LONGRUN_ROUNDS:-30}   # 30: about 0.33 archives a round (T36 measure) -> >= 50 archives in 8 cycles
CYCLE_SECONDS=${FASTMIG_LONGRUN_CYCLE_SECONDS:-3600}
KILL_EVERY=${FASTMIG_LONGRUN_KILL_EVERY:-4}
PITR_CYCLE=${FASTMIG_LONGRUN_PITR_CYCLE:-$(( CYCLES > 2 ? CYCLES - 2 : 1 ))}
MIN_ARCHIVES=${FASTMIG_LONGRUN_MIN_ARCHIVES:-50}
MIN_CHECKPOINTS=${FASTMIG_LONGRUN_MIN_CHECKPOINTS:-100}
BREAK=${FASTMIG_LONGRUN_BREAK:-}
STAT_SECONDS=${FASTMIG_LONGRUN_STAT_SECONDS:-60}
MAX_ARCHIVES=${FASTMIG_LONGRUN_MAX_ARCHIVES:-40}
HERE=$(dirname "$(realpath "$0")")
OPS=$(realpath "$HERE/..")
. "$OPS/sides.sh"
. "$HERE/cycle.sh"
. "$HERE/recovery.sh"

case "$S" in migrated | official) ;; *) echo "side must be migrated or official" >&2; exit 2 ;; esac
case "$BREAK" in "" | pitr-extra-row) ;; *) echo "unknown FASTMIG_LONGRUN_BREAK" >&2; exit 2 ;; esac
for v in "$CYCLES" "$ROUNDS" "$CYCLE_SECONDS" "$KILL_EVERY" "$PITR_CYCLE" "$STAT_SECONDS" "$MAX_ARCHIVES"; do
  [[ "$v" =~ ^[1-9][0-9]*$ ]] || { echo "cycles, rounds, seconds, kill period and PITR cycle are positive integers" >&2; exit 2; }
done
(( PITR_CYCLE <= CYCLES )) || { echo "PITR cycle $PITR_CYCLE beyond $CYCLES cycles" >&2; exit 2; }
ops_setup "$1" "$2" "$3" "$4"
round_files
# checkpoint_interval 2min: about 30 checkpoints an hour. log_max_archives MAX_ARCHIVES: the engine deletes archives
# beyond it without regard to backups (log_page_buffer.c logpb_remove_archive_logs_exceed_limit), so it must hold the
# log a restore reads past the backup it starts from: the PITR reads from the chain's last backup before the mark
# (level 0 or 1, restoredb -l) to the mark, at most two cycles (README § Archives)
printf 'checkpoint_interval=2min\nlog_max_archives=%s\n' "$MAX_ARCHIVES" >>"$INST/conf/cubrid.conf"

CHAIN=""      # backup chain directory the next level 1 or 2 goes to
printf 'side\tstep\tseconds\trc\n' >"$OUT/steps.tsv"
side_begin "$S"
mkdir -p "$OUT/$SIDE/stat"
printf 'event\tepoch\tcycle\tvalue\tchain\n' >"$OUT/$SIDE/times.tsv"
printf 'cycle\tat\trounds\tserver_pid\trss_kb\tcheckpoints\tvacuumed_log_pages_window\tlog_pages_to_vacuum_window\tarchive_first\tarchive_last\tarchives\tvolume_mb\tbackup_mb\thome_avail_mb\terror_lines\tasserts\tcores\tpl_drops\tsession_expired\tfailed_steps\n' \
  >"$OUT/$SIDE/hours.tsv"
echo "running" >"$OUT/$SIDE/rc"

finish() {   # finish <rc>: the side's DB deleted (volumes, log, the backups the DB lists), master stopped, rc recorded
  local rc=$1
  [ "$UP" = 0 ] || stop >/dev/null 2>&1
  step deletedb 0 cubrid deletedb -d "$DB" || rc=1
  stop_master >"$OUT/$SIDE/log/master-stop.log" 2>&1
  echo "$rc" >"$OUT/$SIDE/rc"
  exit "$rc"
}

step prepare 0 "prepare_$SIDE" || finish 1
printf 'moved\t%s\t0\t-\t-\n' "$(date +%s)" >>"$OUT/$SIDE/times.tsv"
step server-start 0 start || finish 1
step check-baseline 0 check baseline
step c00-backup 0 backup_cycle 0 || finish 1
A0=$(last_archive)
snapshot 0

for ((c = 1; c <= CYCLES; c++)); do
  CYCLE_START=$(date +%s); n=$(printf 'c%02d' "$c")
  scenario_step "$n-dml" dml_paced
  (( c != PITR_CYCLE )) || step "$n-pitr-mark" 0 pitr_mark "$c"
  step "$n-statistics" 0 sql_file -C "$OPS/statistics.sql"
  if (( c % KILL_EVERY == 0 )); then maintenance "$c" || { FAIL=1; echo "$SIDE: server lost in $n" >&2; break; }; fi
  step "$n-backup" 0 backup_cycle "$c"
  step "$n-snapshot" 0 snapshot "$c"
  [ -n "$(server_pid)" ] || { FAIL=1; echo "$SIDE: no server after $n" >&2; break; }
  (( c == CYCLES )) || { left=$(( CYCLE_START + CYCLE_SECONDS - $(date +%s) )); (( left <= 0 )) || sleep "$left"; }
done

# the length the ticket asks for: archives created since the move and checkpoints over every server lifetime
step check-end 0 check end
side_faults >/dev/null   # refreshes <side>/server-log/, which checkpoints reads
ckpt=$(checkpoints)
created=$(( $(last_archive) - A0 ))
printf 'archives_created\t%s\nmin_archives\t%s\ncheckpoints\t%s\nmin_checkpoints\t%s\n' "$created" "$MIN_ARCHIVES" "$ckpt" "$MIN_CHECKPOINTS" \
  >"$OUT/$SIDE/length.tsv"
(( created >= MIN_ARCHIVES )) || { FAIL=1; echo "$SIDE: $created archives created, fewer than $MIN_ARCHIVES" >&2; }
(( ckpt >= MIN_CHECKPOINTS )) || { FAIL=1; echo "$SIDE: $ckpt checkpoints, fewer than $MIN_CHECKPOINTS" >&2; }
step server-stop 0 stop

end_recovery
# error lines and cores up to here; range_probes then adds its expected refusals and missing-archive aborts (cores),
# recorded apart as range_cores; asserts are read after it
read -r e a co pl se < <(side_faults)
# restoredb into an empty directory reports first that the probe's own <db>_vinf is not there yet (-551): expected
fresh=$(cat /dev/null "$OUT/$SIDE"/server-log/"${DB}"_restoredb.err | grep -c "ERROR CODE = -551,")
e=$(( e - fresh ))
range_probes
read -r _ a co2 _ _ < <(side_faults)
printf 'error_lines\t%s\nasserts\t%s\ncores\t%s\npl_drops\t%s\nsession_expired\t%s\nrestore_fresh_dir\t%s\nrange_cores\t%s\n' \
  "$e" "$a" "$co" "$pl" "$se" "$fresh" "$(( co2 - co ))" >"$OUT/$SIDE/faults.tsv"
[ "$e$a$co" = 000 ] || { FAIL=1; echo "$SIDE: server error lines $e (PL server drops $pl), asserts $a, cores $co" >&2; }
finish "$FAIL"
