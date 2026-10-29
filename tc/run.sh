#!/usr/bin/env bash
# Runs a list of ported testcases one at a time, each as a baseline run and a run through the switch point, and
# prints the verdict table.
#
# usage: run.sh <tc-root> <tc-list> <install> <cubrid-fastmig binary> <ctp dir> <server-ctl> <out dir>
#   <tc-root>   a cubrid-testcases-private-ex checkout (read only; each testcase is copied into its job)
#   <tc-list>   one testcase directory per line, relative to <tc-root>; text after a tab and lines starting
#               with # are ignored. The list stays local: testcase names are not published (workspace#502).
#   <out dir>   a new directory; one job dir per testcase and mode, summary.tsv, host-servers.txt
# env: FASTMIG_TC_MODES (default "0 1": baseline, then switch point), FASTMIG_TC_TIMEOUT (seconds per job,
#      default 720 as CTP shell), FASTMIG_TC_MAX_GB (stop when <out dir> grows past it, default 20)
# Verdicts: run_case.sh. Before and after the run the cub_server pids outside the run are listed; a pid that is
# gone afterwards is reported (the namespaces should make that impossible).
set -u

SELF_DIR="$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")"
[ $# -eq 7 ] || { sed -n '3,13p' "${BASH_SOURCE[0]}"; exit 2; }
ROOT="$(readlink -f "$1")" LIST="$(readlink -f "$2")" INST="$3" BIN="$4" CTP="$5" SERVER_CTL="$6" OUT="$7"
[ -e "$OUT" ] && { echo "$OUT exists (never reused)"; exit 2; }
mkdir -p "$OUT" || exit 2
OUT="$(readlink -f "$OUT")"
MAX_KB=$(( ${FASTMIG_TC_MAX_GB:-20} * 1024 * 1024 ))

pgrep -u "$(id -u)" -x cub_server | sort >"$OUT/host-servers.before"
df -h /home | tail -1 >"$OUT/df.before"

n=0
while IFS=$'\t' read -r tc _; do
  case "$tc" in '' | '#'*) continue ;; esac
  n=$((n + 1))
  for mode in ${FASTMIG_TC_MODES:-0 1}; do
    bash "$SELF_DIR/run_case.sh" "$OUT/$(printf '%02d' "$n")-$(basename "$tc")-switch$mode" "$ROOT/$tc" "$INST" "$BIN" \
      "$CTP" "$SERVER_CTL" "$mode" | tail -1 >>"$OUT/summary.tsv"
  done
  used=$(du -sk "$OUT" 2>/dev/null | cut -f1)
  [ "$used" -le "$MAX_KB" ] || { echo "stopped: $OUT holds ${used} KB > cap" | tee -a "$OUT/summary.tsv"; break; }
done <"$LIST"

pgrep -u "$(id -u)" -x cub_server | sort >"$OUT/host-servers.after"
comm -23 "$OUT/host-servers.before" "$OUT/host-servers.after" >"$OUT/host-servers.gone"
df -h /home | tail -1 >"$OUT/df.after"

column -t -s $'\t' "$OUT/summary.tsv"
echo "host cub_server before=$(wc -l <"$OUT/host-servers.before") gone=$(wc -l <"$OUT/host-servers.gone")"
if /usr/bin/grep -q $'\tFAIL\t' "$OUT/summary.tsv" || /usr/bin/grep -q '^stopped' "$OUT/summary.tsv"; then
  echo "RESULT: FAILED"
  exit 1
fi
echo "RESULT: OK"
