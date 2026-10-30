#!/usr/bin/env bash
# workspace#560 calibration pass 1 on CPUs 16-31: the large DBs at W1/W4 with and without --verify, W16 with --verify
# (W16 without verify comes from 3번 T29). Runs one after another; each run removes its own work dir.
set -uo pipefail
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/560
B=${1:?binary}
for src in b114 bdev; do
  for cfg in "1 0" "1 1" "4 0" "4 1" "16 1"; do
    read -r w v <<< "$cfg"
    bash $S/run_est.sh $src $w $v c1-$src-w$w-v$v 16-31 $B
  done
done
echo CALIB1-DONE
