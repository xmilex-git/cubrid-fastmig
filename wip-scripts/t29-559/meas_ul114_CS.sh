#!/usr/bin/env bash
# Official path 11.4.6 unloaddb -> 11.4.6 loaddb on the big 11.4 DB: 3 reps of CS W=16. Each run makes its own
# safe_copy.sh source copy; it is removed (literal path) right after the run, and the run deletes its target DB.
set -uo pipefail
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/559
bash $S/scripts/run_ul114.sh CS 16 1 || echo "FAILED run_ul114 CS 16 1"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/src/ul-CS-w16-r1
bash $S/scripts/run_ul114.sh CS 16 2 || echo "FAILED run_ul114 CS 16 2"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/src/ul-CS-w16-r2
bash $S/scripts/run_ul114.sh CS 16 3 || echo "FAILED run_ul114 CS 16 3"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/src/ul-CS-w16-r3
echo "MEAS_UL114_CS DONE"
