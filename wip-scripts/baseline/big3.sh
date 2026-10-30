#!/usr/bin/env bash
# Re-runs of big-input cells another job overlapped (1번 세션 2026-10-30): SA W=1 r1 (the lead's builds), SA W=1 r2
# (an unpinned migrate of another session), CS W=16 r2 and r3 (another session's DB build on the same nvme).
# Rep ids with a "b" suffix; results/disturbed.txt lists the overlapped runs, which summarize.sh keeps out of medians.
# Starts after big2.sh has finished. SA W=1 r1b/r2b were dropped (1번 세션): SA data is single-threaded on CPU 31
# and the overlapped values matched; SA W=1 r3 is the clean value.
set -uo pipefail
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/scripts
while pgrep -f 'scripts/big2.sh' > /dev/null; do sleep 20; done
for cell in "CS 16 2b" "CS 16 3b"; do
  set -- $cell
  bash $S/run_ul.sh big "$1" "$2" "$3" || echo "FAILED run_ul big $1 $2 $3"
done
echo "BIG3 DONE"
