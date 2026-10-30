#!/usr/bin/env bash
# fio device table of #561 (docs/storage_kinds.fio in cubrid-fastmig), one device after the other on CPUs 16-31.
set -uo pipefail
. "$(dirname "$0")/env.sh"
JOB=/home/cubrid/dev/cubrid-fastmig-p561/docs/storage_kinds.fio
mkdir -p $RES/fio; cp $JOB $RES/fio/
for dev in nvme ssd hdd; do
  dir=$(devdir $dev); mkdir -p "$dir"
  echo "$dev pre: $(meta)" >> $RES/fio/meta.txt
  DIR=$dir taskset -c 16-31 /home/cubrid/.nix-profile/bin/fio --output-format=json --output=$RES/fio/$dev.json $JOB
  echo "$dev rc=$? post: $(meta)" >> $RES/fio/meta.txt
done
rm -f /home/cubrid/dev/workspace/.git_ignored_dir/scratch/561/dev/storage_kinds.fio.dat
rm -f /bench/ssd/fastmig-scratch/561/storage_kinds.fio.dat
rm -f /bench/hdd/fastmig-scratch/561/storage_kinds.fio.dat
echo "FIO DONE"
