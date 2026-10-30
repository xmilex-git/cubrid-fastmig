#!/usr/bin/env bash
set -uo pipefail
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/559
[ -x $S/bin/cubrid-fastmig ] || { echo "no $S/bin/cubrid-fastmig; nothing run"; echo "MEAS_FM DONE"; exit 1; }
. $S/scripts/env.sh
# load average and busy processes every 10 s for the whole run (1번 세션 2026-10-30), pinned to CPUs 0-15
taskset -c 0-15 bash -c '. "$0"/scripts/env.sh; while :; do echo "$(date +%T) $(load_and_0_15)"; sleep 9; done' $S >> $S/results/load.log 2>&1 &
sampler=$!
trap "kill $sampler 2>/dev/null" EXIT
mksrc() {   # mksrc <A|B> <pristine dir> <db>: reflink copy without log archives, read with --volumes-in-first-dir
  mkdir $S/src$1 $S/src$1/db || return 1
  mapfile -t items < <(ls -A $2 | /usr/bin/grep -v _lgar)
  cp -a --reflink=always "${items[@]/#/$2/}" $S/src$1/db/ || return 1
  printf '%s\t%s\tlocalhost\t%s\tfile:%s/lob\n' $3 $S/src$1/db $S/src$1/db $S/src$1/db > $S/src$1/databases.txt
}
mksrc A /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/basedb-big/11.4/pristine/clean fm114_clean || { echo "FAILED source copy A"; echo "MEAS_FM DONE"; exit 1; }
bash $S/scripts/run_fm.sh A fm_clone 0 || echo "FAILED run_fm A fm_clone 0"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/meas/A-fm_clone-r0
bash $S/scripts/run_fm.sh A fm_clone 1 || echo "FAILED run_fm A fm_clone 1"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/meas/A-fm_clone-r1
bash $S/scripts/run_fm.sh A fm_copy 1 || echo "FAILED run_fm A fm_copy 1"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/meas/A-fm_copy-r1
bash $S/scripts/run_fm.sh A fm_clone 2 || echo "FAILED run_fm A fm_clone 2"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/meas/A-fm_clone-r2
bash $S/scripts/run_fm.sh A fm_copy 2 || echo "FAILED run_fm A fm_copy 2"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/meas/A-fm_copy-r2
bash $S/scripts/run_fm.sh A fm_clone 3 || echo "FAILED run_fm A fm_clone 3"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/meas/A-fm_clone-r3
bash $S/scripts/run_fm.sh A fm_copy 3 || echo "FAILED run_fm A fm_copy 3"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/meas/A-fm_copy-r3
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/srcA
mksrc B /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/basedb-big/develop/pristine/clean fmdev_clean || { echo "FAILED source copy B"; echo "MEAS_FM DONE"; exit 1; }
bash $S/scripts/run_fm.sh B fm_clone 0 || echo "FAILED run_fm B fm_clone 0"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/meas/B-fm_clone-r0
bash $S/scripts/run_fm.sh B cp_a 1 || echo "FAILED run_fm B cp_a 1"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/meas/B-cp_a-r1
bash $S/scripts/run_fm.sh B cp_reflink 1 || echo "FAILED run_fm B cp_reflink 1"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/meas/B-cp_reflink-r1
bash $S/scripts/run_fm.sh B fm_clone 1 || echo "FAILED run_fm B fm_clone 1"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/meas/B-fm_clone-r1
bash $S/scripts/run_fm.sh B fm_copy 1 || echo "FAILED run_fm B fm_copy 1"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/meas/B-fm_copy-r1
bash $S/scripts/run_fm.sh B cp_a 2 || echo "FAILED run_fm B cp_a 2"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/meas/B-cp_a-r2
bash $S/scripts/run_fm.sh B cp_reflink 2 || echo "FAILED run_fm B cp_reflink 2"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/meas/B-cp_reflink-r2
bash $S/scripts/run_fm.sh B fm_clone 2 || echo "FAILED run_fm B fm_clone 2"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/meas/B-fm_clone-r2
bash $S/scripts/run_fm.sh B fm_copy 2 || echo "FAILED run_fm B fm_copy 2"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/meas/B-fm_copy-r2
bash $S/scripts/run_fm.sh B cp_a 3 || echo "FAILED run_fm B cp_a 3"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/meas/B-cp_a-r3
bash $S/scripts/run_fm.sh B cp_reflink 3 || echo "FAILED run_fm B cp_reflink 3"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/meas/B-cp_reflink-r3
bash $S/scripts/run_fm.sh B fm_clone 3 || echo "FAILED run_fm B fm_clone 3"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/meas/B-fm_clone-r3
bash $S/scripts/run_fm.sh B fm_copy 3 || echo "FAILED run_fm B fm_copy 3"
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/meas/B-fm_copy-r3
rm -rf /home/cubrid/dev/workspace/.git_ignored_dir/scratch/559/srcB
echo "MEAS_FM DONE"
