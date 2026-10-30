#!/usr/bin/env bash
# Big input (11.4 T1 x rows multiplier 180, heap ~4 GiB): 3 reps x W {1,4,16} x {SA,CS} unload->load, then 3 reps of cp/copydb.
# Order interleaves W and mode inside each rep so host drift spreads over all cells.
set -uo pipefail
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/scripts
# The big runs wait for a quiet host: the lead creates results/BIG_GO when 1번 세션 says the other benchmarks ended.
[ -e /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/results/BIG_GO ] || { echo "BIG BLOCKED: results/BIG_GO missing (waiting for a quiet window); nothing run"; echo "BIG DONE"; exit 0; }
for rep in 1 2 3; do for W in 1 4 16; do for mode in SA CS; do
  bash $S/run_ul.sh big $mode $W $rep || echo "FAILED run_ul big $mode $W $rep"
done; done; done
bash $S/run_cp.sh big 1 cp_never || echo "FAILED run_cp big 1 cp_never"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/cp/big-r1-never
bash $S/run_cp.sh big 1 cp_reflink || echo "FAILED run_cp big 1 cp_reflink"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/cp/big-r1-reflink
bash $S/run_cp.sh big 1 copydb || echo "FAILED run_cp big 1 copydb"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/cp/big-r1-copydb
bash $S/run_cp.sh big 2 cp_never || echo "FAILED run_cp big 2 cp_never"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/cp/big-r2-never
bash $S/run_cp.sh big 2 cp_reflink || echo "FAILED run_cp big 2 cp_reflink"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/cp/big-r2-reflink
bash $S/run_cp.sh big 2 copydb || echo "FAILED run_cp big 2 copydb"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/cp/big-r2-copydb
bash $S/run_cp.sh big 3 cp_never || echo "FAILED run_cp big 3 cp_never"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/cp/big-r3-never
bash $S/run_cp.sh big 3 cp_reflink || echo "FAILED run_cp big 3 cp_reflink"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/cp/big-r3-reflink
bash $S/run_cp.sh big 3 copydb || echo "FAILED run_cp big 3 copydb"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/cp/big-r3-copydb
echo "BIG DONE"
