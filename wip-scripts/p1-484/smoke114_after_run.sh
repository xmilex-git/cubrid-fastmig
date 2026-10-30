#!/usr/bin/env bash
# Starts smoke114_big.sh 15 s after 3번 세션's next RUN line in big2.log (past its next quiet gate), or at once when big2.sh is gone.
while [ $(/usr/bin/grep -c '^RUN' /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/results/big2.log) -le 5 ] && pgrep -f scripts/big2.sh > /dev/null; do sleep 5; done
sleep 15
echo "start $(date -Is) after RUN: $(/usr/bin/grep '^RUN' /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/results/big2.log | tail -1)"
bash /home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/scripts/smoke114_big.sh
