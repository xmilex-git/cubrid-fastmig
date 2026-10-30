#!/usr/bin/env bash
# Re-runs of the same-version cells 3번 세션's verify runs overlapped (01:34:25-01:37:38 KST on CPUs 16-31, 1번 세션
# 2026-10-30): fm_clone r1, cp_a / cp_reflink / fm_copy / fm_clone r2. Rep ids get a "b"; the summary leaves the
# overlapped reps out (results/same.disturbed lists them).
set -uo pipefail
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/484
printf 'fm_clone\t1\ncp_a\t2\ncp_reflink\t2\nfm_copy\t2\nfm_clone\t2\n' > $S/results/same.disturbed
bash $S/scripts/run_same.sh fm_clone 1b || echo "FAILED run_same fm_clone 1b"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/fm_clone-r1b
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_same.sh cp_a 2b || echo "FAILED run_same cp_a 2b"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/cp_a-r2b
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_same.sh cp_reflink 2b || echo "FAILED run_same cp_reflink 2b"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/cp_reflink-r2b
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_same.sh fm_copy 2b || echo "FAILED run_same fm_copy 2b"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/fm_copy-r2b
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_same.sh fm_clone 2b || echo "FAILED run_same fm_clone 2b"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/fm_clone-r2b
: > $S/meas/tgtdbs/databases.txt
echo "MEAS_RERUN DONE"
