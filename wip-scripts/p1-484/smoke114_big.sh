#!/usr/bin/env bash
# Failure-finding smoke (not a measurement, 1번 세션 2026-10-30): the big 11.4 DB (3번 세션's rows multiplier 180)
# through cubrid-fastmig migrate 11.4 -> develop once, on CPUs 0-15, --workers 16 --mem 16G (the measurement's
# settings). Looks for what only a big DB hits: out of space (volume growth, workspace#508), spill, memory, verify.
#   bash smoke114_big.sh
# Source: a reflink copy of baseline/basedb-big/11.4/pristine/clean without its log archives, read with
# --volumes-in-first-dir (its headers still name the registered DB; no engine utility touches it).
# Output under 484/smoke114/: migrate.out (report.json), migrate.err (progress, warnings, /usr/bin/time -v),
# summary.txt; ends with summary line "SMOKE114 DONE rc=<n>". The caller deletes src/ and tgt/ by literal path.
set -uo pipefail
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/484
P=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/basedb-big/11.4/pristine/clean
D=$S/smoke114
[ ! -e $D ] || { echo "$D exists"; echo "SMOKE114 DONE rc=2"; exit 2; }
mkdir -p $D/src $D/src-dbs $D/tgt-dbs
mapfile -t items < <(ls -A $P | /usr/bin/grep -v '_lgar')
cp -a --reflink=always "${items[@]/#/$P/}" $D/src/ || { echo "SMOKE114 DONE rc=3"; exit 3; }
printf 'fm114_clean\t%s\tlocalhost\t%s\tfile:%s/lob\n' $D/src $D/src $D/src > $D/src-dbs/databases.txt
: > $D/tgt-dbs/databases.txt
{ echo "bin $(cat $S/bin/cubrid-fastmig.sha256 | tr '\n' ' ')"; echo "pre $(date -Is) load=$(cut -d' ' -f1-3 /proc/loadavg) df=[$(df -h /home | awk 'NR==2{print $4" avail"}')]"; } > $D/summary.txt
/usr/bin/time -v taskset -c 0-15 $S/bin/cubrid-fastmig migrate fm114_clean --dest $D/tgt --target-version develop \
  --target-name fm114_dev --workers 16 --mem 16G --volumes-in-first-dir --verify \
  --source-databases-txt $D/src-dbs/databases.txt --target-databases-txt $D/tgt-dbs/databases.txt \
  --progress plain --progress-interval 30 > $D/migrate.out 2> $D/migrate.err
rc=$?
{
  echo "post $(date -Is) rc=$rc"
  /usr/bin/grep -E 'Maximum resident|Elapsed \(wall|Exit status' $D/migrate.err
  echo "target du: $(du -sm $D/tgt | cut -f1) MB; volumes: $(ls $D/tgt | /usr/bin/grep -cE '^fm114_dev(_x[0-9]+)?$')"
  ls -la $D/tgt | /usr/bin/grep -E 'fm114_dev(_x[0-9]+)?$'
  [ ! -f $D/tgt/fastmig/report.json ] || jq -c '{seconds, volumes: [.volumes[] | {volid, method, bytes}], added_volumes, heap_peak_bytes, catalog, verify: (.verify | {migration, pre_existing, by_check})}' $D/tgt/fastmig/report.json
  du -sm $D/tgt/fastmig/spill 2>/dev/null
} >> $D/summary.txt 2>&1
cat $D/summary.txt
echo "SMOKE114 DONE rc=$rc"
