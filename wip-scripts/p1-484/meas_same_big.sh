#!/usr/bin/env bash
# Same-version measurement on the big develop DB: 3 reps x {cp_a, cp_reflink, fm_copy, fm_clone} interleaved per rep
# so host drift spreads over every method, then fm_copy_verify once (consistency evidence; speed is compared without
# verify, user 2026-10-30). Each copy is removed (literal path) before the next run, so at most the source and one
# copy exist at a time. First restores the registered source to its pristine bytes (the row count after the build
# booted it SA) and checks them against evidence/clean/pristine.sha256.
set -uo pipefail
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/484
B=$S/basedb-big/develop
rsync -a --delete $B/pristine/clean/ $B/clean/
( cd $B/clean && find . -type f -print0 | sort -z | xargs -0 sha256sum ) | diff -q - $B/evidence/clean/pristine.sha256 \
  && echo "source == pristine" || { echo "source differs from pristine; nothing run"; echo "MEAS_SAME DONE"; exit 1; }
bash $S/scripts/run_same.sh cp_a 1 || echo "FAILED run_same cp_a 1"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/cp_a-r1
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_same.sh cp_reflink 1 || echo "FAILED run_same cp_reflink 1"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/cp_reflink-r1
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_same.sh fm_copy 1 || echo "FAILED run_same fm_copy 1"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/fm_copy-r1
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_same.sh fm_clone 1 || echo "FAILED run_same fm_clone 1"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/fm_clone-r1
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_same.sh cp_a 2 || echo "FAILED run_same cp_a 2"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/cp_a-r2
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_same.sh cp_reflink 2 || echo "FAILED run_same cp_reflink 2"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/cp_reflink-r2
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_same.sh fm_copy 2 || echo "FAILED run_same fm_copy 2"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/fm_copy-r2
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_same.sh fm_clone 2 || echo "FAILED run_same fm_clone 2"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/fm_clone-r2
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_same.sh cp_a 3 || echo "FAILED run_same cp_a 3"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/cp_a-r3
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_same.sh cp_reflink 3 || echo "FAILED run_same cp_reflink 3"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/cp_reflink-r3
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_same.sh fm_copy 3 || echo "FAILED run_same fm_copy 3"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/fm_copy-r3
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_same.sh fm_clone 3 || echo "FAILED run_same fm_clone 3"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/fm_clone-r3
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_same.sh fm_copy_verify 1 || echo "FAILED run_same fm_copy_verify 1"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/fm_copy_verify-r1
: > $S/meas/tgtdbs/databases.txt
echo "MEAS_SAME DONE"
