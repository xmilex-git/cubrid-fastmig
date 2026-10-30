#!/usr/bin/env bash
# Build one ~2 GB input of #561 (T31): basedb/build.sh <label> clean with rows multiplier 80 (big DB is 180 = 4.2 GB).
#   bash build_2g.sh <11.4|develop>
# Output: 561/basedb2g/<label>/{clean,pristine/clean,unload/clean,evidence/clean}; ends with results/build_<label>.DONE
# or .FAILED. Install: a reflink copy of the release install (build.sh writes its conf). CPUs 0-15 (16-31 measure).
set -euo pipefail
label=${1:?label}
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/561
case "$label" in
  11.4) rel=/home/cubrid/release/CUBRID-fastmig-11.4.6; port=1711; db=fm114_clean; inst=$S/inst114 ;;
  develop) rel=/home/cubrid/release/CUBRID-fastmig-dev; port=1712; db=fmdev_clean; inst=$S/instdev ;;
  *) echo "bad label" >&2; exit 2 ;;
esac
ROOT=$S/basedb2g
RES=$S/results
CTL=/home/cubrid/dev/workspace/.claude/skills/cubrid-server-control/scripts/cubrid-server-ctl.sh
export CUBRID_TMP=/home/cubrid/.bltmp/561$label   # short socket dir (cubrid-server-control P5)
mkdir -p "$CUBRID_TMP" "$RES"
[ -e "$inst" ] || cp -a --reflink=always "$rel" "$inst"

finish() {
  local rc=$? p
  set +e
  for p in $(pgrep -u "$(id -u)" -x cub_server || true); do
    [ "$(readlink /proc/$p/exe)" != "$inst/bin/cub_server" ] || \
      CUBRID=$inst CUBRID_DATABASES=$ROOT/$label PATH=$inst/bin:/usr/local/bin:/usr/bin:/bin LD_LIBRARY_PATH=$inst/lib \
        bash $CTL stop $db > $RES/build_$label.stop-on-exit.out 2>&1
  done
  for p in $(pgrep -u "$(id -u)" -x cub_master || true); do
    [ "$(readlink /proc/$p/exe)" != "$inst/bin/cub_master" ] || kill -TERM "$p"
  done
  echo "end $(date -Is) rc=$rc" >> $RES/build_$label.meta
  if [ $rc -eq 0 ]; then touch $RES/build_$label.DONE; else echo "rc=$rc" > $RES/build_$label.FAILED; fi
}
trap finish EXIT

echo "start $(date -Is) df=[$(df -h /home | awk 'NR==2{print $3" used "$4" avail"}')]" > $RES/build_$label.meta
BASEDB_ROOT=$ROOT BASEDB_ROWS_MULTIPLIER=80 BASEDB_CAP_MB=20000 \
  bash /home/cubrid/dev/cubrid-fastmig-p561/basedb/build.sh $label $inst $port clean
ls -la $ROOT/$label/pristine/clean > $RES/build_$label.pristine.ls
echo "volumes without log: $(ls $ROOT/$label/pristine/clean | /usr/bin/grep -E "^${db}(_x[0-9]+)?$" | sed "s|^|$ROOT/$label/pristine/clean/|" | xargs du -cm --apparent-size | tail -1 | cut -f1) MB" >> $RES/build_$label.meta
CUBRID=$inst CUBRID_DATABASES=$ROOT/$label PATH=$inst/bin:/usr/local/bin:/usr/bin:/bin LD_LIBRARY_PATH=$inst/lib \
  csql -u dba -S $db -i /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/scripts/rows.sql > $RES/rows.$label.out 2>&1
echo "BUILD $label DONE"
