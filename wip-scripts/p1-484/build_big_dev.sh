#!/usr/bin/env bash
# Build the big develop input of #484's same-version measurement: basedb/build.sh develop clean with rows
# multiplier 180, the same rows as 3번 세션's basedb-big/11.4 (baseline/scripts/build_big.sh).
#   bash build_big_dev.sh
# Output: 484/basedb-big/develop/{clean,pristine/clean,unload/clean,evidence/clean}, databases.txt there.
# basedb scripts: 484/basedb-src (git archive of cubrid-fastmig origin/main 16fcb9c), install: 484/instdev (reflink copy
# of /home/cubrid/release/CUBRID-fastmig-dev, develop 682b080b2 release). Port 1714 (claims.md, fmdev_clean).
# Runs under taskset 0-15: CPUs 16-31 carry 3번 세션's baseline measurement.
# Size cap 30000 MB without the reflinked pristine copy (build.sh's own check counts it twice, so it gets 60000).
# Ends with results/build_big_dev.DONE or results/build_big_dev.FAILED (rc inside).
set -euo pipefail
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/484
INST=$S/instdev
ROOT=$S/basedb-big
CTL=/home/cubrid/dev/workspace/.claude/skills/cubrid-server-control/scripts/cubrid-server-ctl.sh
RES=$S/results
export CUBRID_TMP=/home/cubrid/.bltmp/484   # the default socket dir under the install copy is too long (cubrid-server-control P5)
mkdir -p "$CUBRID_TMP"

stop_master() {   # the wrapper's stop leaves cub_master of this install alive
  local p
  for p in $(pgrep -u "$(id -u)" -x cub_master || true); do
    [ "$(readlink /proc/$p/exe)" != "$INST/bin/cub_master" ] || kill -TERM "$p" || true
  done
}
finish() {
  local rc=$?
  set +e
  for p in $(pgrep -u "$(id -u)" -x cub_server || true); do
    if [ "$(readlink /proc/$p/exe)" = "$INST/bin/cub_server" ]; then
      CUBRID=$INST CUBRID_DATABASES=$ROOT/develop PATH=$INST/bin:/usr/local/bin:/usr/bin:/bin LD_LIBRARY_PATH=$INST/lib \
        bash $CTL stop fmdev_clean > $RES/build_big_dev.stop-on-exit.out 2>&1
    fi
  done
  stop_master
  echo "end $(date -Is) rc=$rc" >> $RES/build_big_dev.meta
  if [ $rc -eq 0 ]; then touch $RES/build_big_dev.DONE; else echo "rc=$rc" > $RES/build_big_dev.FAILED; fi
}
trap finish EXIT

echo "start $(date -Is) load=$(cut -d' ' -f1-3 /proc/loadavg) df=[$(df -h /home | awk 'NR==2{print $3" used "$4" avail"}')]" > $RES/build_big_dev.meta
BASEDB_ROOT=$ROOT BASEDB_ROWS_MULTIPLIER=180 BASEDB_CAP_MB=60000 \
  taskset -c 0-15 bash $S/basedb-src/basedb/build.sh develop $INST 1714 clean

# own cap: everything but the reflinked pristine copy
mb=$(du -smc $ROOT/develop/clean $ROOT/develop/unload $ROOT/develop/evidence | tail -1 | cut -f1)
echo "size without pristine: ${mb} MB (cap 30000); pristine apparent $(du -sm --apparent-size $ROOT/develop/pristine | cut -f1) MB" >> $RES/build_big_dev.meta
[ "$mb" -le 30000 ] || { echo "size cap exceeded" >&2; exit 1; }

# row counts on the registered DB (SA; the pristine copy is untouched), compared with 3번 세션's 11.4 big DB
CUBRID=$INST CUBRID_DATABASES=$ROOT/develop PATH=$INST/bin:/usr/local/bin:/usr/bin:/bin LD_LIBRARY_PATH=$INST/lib \
  csql -u dba -S fmdev_clean -i /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/scripts/rows.sql \
  > $RES/rows.bigdev.out 2>&1
rows() { /usr/bin/grep -E "^  '[a-z_0-9]+' +[0-9]+$" "$1" | awk '{print $1, $2}'; }
diff <(rows /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/results/rows.big114.src.out) <(rows $RES/rows.bigdev.out) \
  > $RES/rows.big114-vs-bigdev.diff && echo "rows: same as big 11.4" >> $RES/build_big_dev.meta \
  || echo "rows: DIFFER from big 11.4 (rows.big114-vs-bigdev.diff)" >> $RES/build_big_dev.meta
ls -la $ROOT/develop/clean > $RES/build_big_dev.clean.ls
echo "BUILD_BIG_DEV DONE"
