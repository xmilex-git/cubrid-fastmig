#!/usr/bin/env bash
# Build the big 11.4 input: basedb/build.sh with rows multiplier 180 (heap ~4 GiB) into its own BASEDB_ROOT.
# Output: basedb-big/11.4/{clean,pristine/clean,unload/clean,evidence/clean}, databases.txt there.
set -euo pipefail
. "$(dirname "$0")/env.sh"
export CUBRID_TMP=/home/cubrid/.bltmp/114   # the default socket dir under the install copy is too long (cubrid-server-control P5)
# build.sh exits on a csql error with its server still up; stop it (and the master) on any exit
trap 'set +e; if ! no_server_of $I114 2>/dev/null; then ( use114 $BL/basedb-big/11.4; bash $CTL stop fm114_clean ); fi; stop_master $I114' EXIT
BASEDB_ROOT=$BL/basedb-big BASEDB_ROWS_MULTIPLIER=180 BASEDB_CAP_MB=20000 \
  bash /home/cubrid/dev/cubrid-fastmig-basedb-big/basedb/build.sh 11.4 $I114 $PORT114 clean   # cubrid-fastmig 810ecef
use114 $BL/basedb-big/11.4
csql -u dba -S fm114_clean -i $BL/scripts/rows.sql > $RES/rows.big114.src.out 2>&1
cat $RES/rows.big114.src.out

echo "BUILD_BIG DONE"
