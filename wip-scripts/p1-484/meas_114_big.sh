#!/usr/bin/env bash
# 11.4 -> develop measurement on 3번 세션's big 11.4 DB: 3 reps x {fm W=16, fm_copy W=16, fm W=4, fm W=1}
# interleaved per rep (fm = the default clone), then fm_verify W=16 once (consistency evidence; speed is compared
# without verify, user 2026-10-30). Each target is removed (literal path) before the next run. The source is
# src114/db, a reflink copy of 3번's pristine without log archives, made here and removed at the end. Runs only after
# 3번 세션's baseline drivers (big*.sh) ended: CPUs 16-31 are theirs until then.
set -uo pipefail
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/484
P=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/basedb-big/11.4/pristine/clean
! pgrep -f 'scripts/big[0-9]*\.sh' > /dev/null || { echo "3번 big*.sh still running; nothing run"; echo "MEAS_114 DONE"; exit 1; }
mkdir $S/src114 || { echo "src114 exists; nothing run"; echo "MEAS_114 DONE"; exit 1; }
mkdir $S/src114/db
mapfile -t items < <(ls -A $P | /usr/bin/grep -v _lgar)
cp -a --reflink=always "${items[@]/#/$P/}" $S/src114/db/ || { echo "copy failed"; echo "MEAS_114 DONE"; exit 1; }
printf 'fm114_clean\t%s\tlocalhost\t%s\tfile:%s/lob\n' $S/src114/db $S/src114/db $S/src114/db > $S/src114/databases.txt
bash $S/scripts/run_114.sh fm 16 1 || echo "FAILED run_114 fm 16 1"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/fm-w16-r1
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_114.sh fm_copy 16 1 || echo "FAILED run_114 fm_copy 16 1"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/fm_copy-w16-r1
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_114.sh fm 4 1 || echo "FAILED run_114 fm 4 1"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/fm-w4-r1
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_114.sh fm 1 1 || echo "FAILED run_114 fm 1 1"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/fm-w1-r1
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_114.sh fm 16 2 || echo "FAILED run_114 fm 16 2"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/fm-w16-r2
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_114.sh fm_copy 16 2 || echo "FAILED run_114 fm_copy 16 2"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/fm_copy-w16-r2
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_114.sh fm 4 2 || echo "FAILED run_114 fm 4 2"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/fm-w4-r2
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_114.sh fm 1 2 || echo "FAILED run_114 fm 1 2"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/fm-w1-r2
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_114.sh fm 16 3 || echo "FAILED run_114 fm 16 3"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/fm-w16-r3
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_114.sh fm_copy 16 3 || echo "FAILED run_114 fm_copy 16 3"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/fm_copy-w16-r3
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_114.sh fm 4 3 || echo "FAILED run_114 fm 4 3"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/fm-w4-r3
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_114.sh fm 1 3 || echo "FAILED run_114 fm 1 3"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/fm-w1-r3
: > $S/meas/tgtdbs/databases.txt
bash $S/scripts/run_114.sh fm_verify 16 1 || echo "FAILED run_114 fm_verify 16 1"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/meas/fm_verify-w16-r1
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/src114
echo "MEAS_114 DONE"
