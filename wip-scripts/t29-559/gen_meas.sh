#!/usr/bin/env bash
# Writes the #559 drivers with every rm -rf path spelled out literally (house rule):
#   meas_ul114_CS.sh   11.4.6 unloaddb -> 11.4.6 loaddb CS W=16 x3 (first scope; run 2026-10-30 12:01-12:13, kept as a record)
#   meas_fm.sh         (A) 11.4 -> develop and (B) develop -> develop, scope of 2026-10-30 (1번 세션):
#                      per path a fresh source copy, a cold fm_clone r0, then 3 warm reps interleaved per rep
#                      (A: fm_clone, fm_copy; B: cp_a, cp_reflink, fm_clone, fm_copy). Verify is not run (speed is
#                      compared without it, user 2026-10-30). At most the source copy and one target exist at a time.
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/559
f=$S/scripts/meas_ul114_CS.sh
{
cat <<EOF
#!/usr/bin/env bash
# Official path 11.4.6 unloaddb -> 11.4.6 loaddb on the big 11.4 DB: 3 reps of CS W=16. Each run makes its own
# safe_copy.sh source copy; it is removed (literal path) right after the run, and the run deletes its target DB.
set -uo pipefail
S=$S
EOF
for rep in 1 2 3; do
  echo "bash \$S/scripts/run_ul114.sh CS 16 $rep || echo \"FAILED run_ul114 CS 16 $rep\""
  echo "rm -rf $S/src/ul-CS-w16-r$rep"
done
echo 'echo "MEAS_UL114_CS DONE"'
} > $f
bash -n $f
f=$S/scripts/meas_fm.sh
{
cat <<EOF
#!/usr/bin/env bash
set -uo pipefail
S=$S
[ -x \$S/bin/cubrid-fastmig ] || { echo "no \$S/bin/cubrid-fastmig; nothing run"; echo "MEAS_FM DONE"; exit 1; }
. \$S/scripts/env.sh
# load average and busy processes every 10 s for the whole run (1번 세션 2026-10-30), pinned to CPUs 0-15
taskset -c 0-15 bash -c '. "\$0"/scripts/env.sh; while :; do echo "\$(date +%T) \$(load_and_0_15)"; sleep 9; done' \$S >> \$S/results/load.log 2>&1 &
sampler=\$!
trap "kill \$sampler 2>/dev/null" EXIT
mksrc() {   # mksrc <A|B> <pristine dir> <db>: reflink copy without log archives, read with --volumes-in-first-dir
  mkdir \$S/src\$1 \$S/src\$1/db || return 1
  mapfile -t items < <(ls -A \$2 | /usr/bin/grep -v _lgar)
  cp -a --reflink=always "\${items[@]/#/\$2/}" \$S/src\$1/db/ || return 1
  printf '%s\t%s\tlocalhost\t%s\tfile:%s/lob\n' \$3 \$S/src\$1/db \$S/src\$1/db \$S/src\$1/db > \$S/src\$1/databases.txt
}
EOF
for p in "A /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/basedb-big/11.4/pristine/clean fm114_clean fm_clone fm_copy" \
         "B /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/basedb-big/develop/pristine/clean fmdev_clean cp_a cp_reflink fm_clone fm_copy"; do
  set -- $p; path=$1; pristine=$2; db=$3; shift 3; methods="$*"
  echo "mksrc $path $pristine $db || { echo \"FAILED source copy $path\"; echo \"MEAS_FM DONE\"; exit 1; }"
  echo "bash \$S/scripts/run_fm.sh $path fm_clone 0 || echo \"FAILED run_fm $path fm_clone 0\""
  echo "rm -rf $S/meas/$path-fm_clone-r0"
  for rep in 1 2 3; do for m in $methods; do
    echo "bash \$S/scripts/run_fm.sh $path $m $rep || echo \"FAILED run_fm $path $m $rep\""
    echo "rm -rf $S/meas/$path-$m-r$rep"
  done; done
  echo "rm -rf $S/src$path"
done
echo 'echo "MEAS_FM DONE"'
} > $f
bash -n $f
