#!/usr/bin/env bash
# One-time setup: own install copies, and the T1 inputs as path-rewritten reflink copies of the pristines
# (a raw pristine copy names the registered T1 DB; the engine would write there, see basedb/README.md).
#   in/small114/db  fm114_clean, registered in in/small114/dbs   (11.4 source of unload->load)
#   in/smalldev/db  fmdev_clean, registered in in/smalldev/dbs   (develop source of cp/copydb)
set -euo pipefail
. "$(dirname "$0")/env.sh"
[ -e $I114 ] || cp -a --reflink=always /home/cubrid/release/CUBRID-fastmig-11.4.6 $I114
[ -e $IDEV ] || cp -a --reflink=always /home/cubrid/release/CUBRID-fastmig-dev $IDEV
[ -e $FM ] || cp /home/cubrid/dev/cubrid-fastmig-p1e-copy/target/release/cubrid-fastmig $FM
sha256sum $FM > $RES/fm-binary.sha256

prep() {   # $1 label dir, $2 pristine, $3 db
  local d=$BL/in/$1
  [ ! -e $d ] || { echo "$d exists" >&2; exit 1; }
  mkdir -p $d/dbs
  cp -a --reflink=always "$2" $d/raw
  printf '%s\t%s\tlocalhost\t%s\tfile:%s/lob\n' "$3" $d/raw $d/raw $d/raw > $d/raw.databases.txt
  ( cd $d/raw/lob && find . -type f -printf '%s\t%P\n' | sort -k2 ) > $d/lob-manifest
  $FM migrate "$3" --dest $d/db --volumes-in-first-dir --source-databases-txt $d/raw.databases.txt \
    --target-databases-txt $d/dbs/databases.txt --lob-manifest $d/lob-manifest > $d/migrate.out 2>&1
  cat $d/dbs/databases.txt
}
prep small114 /home/cubrid/fastmig-basedb/11.4/pristine/clean fm114_clean
prep smalldev /home/cubrid/fastmig-basedb/develop/pristine/clean fmdev_clean

use114 $BL/in/small114/dbs
csql -u dba -S fm114_clean -i $BL/scripts/rows.sql > $RES/rows.small114.src.out 2>&1
cat $RES/rows.small114.src.out
du -sm $BL/in/small114/db $BL/in/smalldev/db
echo "SETUP_SMALL DONE"
