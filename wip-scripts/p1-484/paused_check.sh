#!/usr/bin/env bash
# Exit 0 when every live run_ul.sh / run_cp.sh of 3번 세션 waits in its PAUSE loop (only `sleep` children) and no
# baseline engine process runs; else exit 1. Prints the state.
B=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline
busy=0
for p in $(pgrep -f 'scripts/run_(ul|cp).sh'); do
  kids=$(pgrep -P $p | xargs -r -n1 ps -o comm= -p | sort -u | tr '\n' ' ')
  echo "driver $p children: [$kids]"
  [ -z "$kids" ] || [ "$kids" = "sleep " ] || busy=1
done
for p in $(pgrep -u "$(id -u)" -x 'cub_server|cub_master|cubrid|loaddb|cub_admin'); do
  case "$(readlink /proc/$p/exe)" in $B/*) echo "engine $p $(readlink /proc/$p/exe)"; busy=1 ;; esac
done
exit $busy
