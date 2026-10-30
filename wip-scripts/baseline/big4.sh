#!/usr/bin/env bash
# Re-run of CS W=16 r1, which another session's DB build (00:06-00:23, same nvme) also overlapped (CS W=4 r1b dropped:
# 1번 세션 uses the clean r2/r3).
# Rep ids with a "b" suffix; results/disturbed.txt lists the overlapped runs, which summarize.sh keeps out of medians.
# Starts after big3.sh has finished.
set -uo pipefail
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/scripts
while pgrep -f 'scripts/big[23].sh' > /dev/null; do sleep 20; done
for cell in "CS 16 1b"; do
  set -- $cell
  bash $S/run_ul.sh big "$1" "$2" "$3" || echo "FAILED run_ul big $1 $2 $3"
done
echo "BIG4 DONE"
