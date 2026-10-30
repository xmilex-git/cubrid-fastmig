#!/usr/bin/env bash
# Writes meas_same_rerun.sh (literal rm -rf paths). Run with bash: word splitting of "$cell" is bash behavior.
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/484; f=$S/scripts/meas_same_rerun.sh
{
cat <<'EOF'
#!/usr/bin/env bash
# Re-runs of the same-version cells 3번 세션's verify runs overlapped (01:34:25-01:37:38 KST on CPUs 16-31, 1번 세션
# 2026-10-30): fm_clone r1, cp_a / cp_reflink / fm_copy / fm_clone r2. Rep ids get a "b"; the summary leaves the
# overlapped reps out (results/same.disturbed lists them).
set -uo pipefail
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/484
printf 'fm_clone\t1\ncp_a\t2\ncp_reflink\t2\nfm_copy\t2\nfm_clone\t2\n' > $S/results/same.disturbed
EOF
for cell in "fm_clone 1b" "cp_a 2b" "cp_reflink 2b" "fm_copy 2b" "fm_clone 2b"; do
  set -- $cell
  echo "bash \$S/scripts/run_same.sh $1 $2 || echo \"FAILED run_same $1 $2\""
  echo "rm -rf $S/meas/$1-r$2"
  echo ': > $S/meas/tgtdbs/databases.txt'
done
echo 'echo "MEAS_RERUN DONE"'
} > $f
bash -n $f
