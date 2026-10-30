#!/usr/bin/env bash
# Small input (T1, ~0.5 GB): 3 reps x W {1,4,16} x {SA,CS} unload->load, then 3 reps of cp/copydb.
# Order interleaves W and mode inside each rep so host drift spreads over all cells.
set -uo pipefail
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/scripts
for rep in 1 2 3; do for W in 1 4 16; do for mode in SA CS; do
  bash $S/run_ul.sh small $mode $W $rep || echo "FAILED run_ul small $mode $W $rep"
done; done; done
bash $S/run_cp.sh small 1 || echo "FAILED run_cp small 1"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/cp/small-r1-never /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/cp/small-r1-reflink /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/cp/small-r1-copydb
bash $S/run_cp.sh small 2 || echo "FAILED run_cp small 2"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/cp/small-r2-never /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/cp/small-r2-reflink /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/cp/small-r2-copydb
bash $S/run_cp.sh small 3 || echo "FAILED run_cp small 3"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/cp/small-r3-never /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/cp/small-r3-reflink /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/cp/small-r3-copydb
echo "SMALL DONE"
