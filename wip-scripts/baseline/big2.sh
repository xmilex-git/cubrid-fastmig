#!/usr/bin/env bash
# Big input, reordered for speed (1번 세션 2026-10-30): loaddb runs in parallel only in CS mode (server-side
# loaddb_worker_count), so SA is measured at W=1 only. Order: CS W=16 x3 (the main baseline against fastmig),
# CS W=4 r1, CS W=1 r1, then SA W=1 r2 r3, CS W=4 r2 r3, CS W=1 r2 r3, then cp/copydb x3.
# Waits first for the run big.sh left in flight (SA W=1 r1). A run whose directory exists is refused by run_ul.sh.
set -uo pipefail
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/scripts
while pgrep -f 'scripts/run_ul.sh big' > /dev/null; do sleep 20; done
for cell in "CS 16 1" "CS 16 2" "CS 16 3" "CS 4 1" "CS 1 1" "SA 1 2" "SA 1 3" "CS 4 2" "CS 4 3" "CS 1 2" "CS 1 3"; do
  set -- $cell
  bash $S/run_ul.sh big "$1" "$2" "$3" || echo "FAILED run_ul big $1 $2 $3"
done
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
