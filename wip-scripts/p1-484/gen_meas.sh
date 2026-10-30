#!/usr/bin/env bash
# Writes meas_same_big.sh and meas_114_big.sh: every rm -rf path spelled out literally (house rule).
# Speed is compared without verify (user 2026-10-30): 3 reps of the verify-less methods, then one --verify run per
# path as consistency evidence (no median).
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/484
f=$S/scripts/meas_same_big.sh
{
cat <<'EOF'
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
EOF
for cell in "cp_a 1" "cp_reflink 1" "fm_copy 1" "fm_clone 1" "cp_a 2" "cp_reflink 2" "fm_copy 2" "fm_clone 2" "cp_a 3" "cp_reflink 3" "fm_copy 3" "fm_clone 3" "fm_copy_verify 1"; do
  set -- $cell
  echo "bash \$S/scripts/run_same.sh $1 $2 || echo \"FAILED run_same $1 $2\""
  echo "rm -rf $S/meas/$1-r$2"
  echo ': > $S/meas/tgtdbs/databases.txt'
done
echo 'echo "MEAS_SAME DONE"'
} > $f
bash -n $f
f=$S/scripts/meas_114_big.sh
{
cat <<'EOF'
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
EOF
for rep in 1 2 3; do for cell in "fm 16" "fm_copy 16" "fm 4" "fm 1"; do
  set -- $cell
  echo "bash \$S/scripts/run_114.sh $1 $2 $rep || echo \"FAILED run_114 $1 $2 $rep\""
  echo "rm -rf $S/meas/$1-w$2-r$rep"
  echo ': > $S/meas/tgtdbs/databases.txt'
done; done
echo "bash \$S/scripts/run_114.sh fm_verify 16 1 || echo \"FAILED run_114 fm_verify 16 1\""
echo "rm -rf $S/meas/fm_verify-w16-r1"
echo "rm -rf $S/src114"
echo 'echo "MEAS_114 DONE"'
} > $f
bash -n $f
