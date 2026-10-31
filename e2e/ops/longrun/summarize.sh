#!/usr/bin/env bash
# Markdown summary of a long run (T39 workspace#594), in progress or finished: per path, each side's state and the
# cycles so far, RSS and volume size of the two sides next to each other, parity of the checks both sides have
# written, and at the end the length, restore and PITR rows. Read-only; run.sh refreshes <out>/summary.md with it
# every minute, and the "Long run" section of docs/ops-verification.md is written from its final output.
#
# Input: bash e2e/ops/longrun/summarize.sh <out dir>. Output: markdown on stdout.
set -uo pipefail
OUT=$(realpath "${1:?out dir}")

col() { awk -F'\t' -v c="$2" 'NR == 1 { for (i = 1; i <= NF; i++) if ($i == c) k = i; next } { print $k }' "$1"; }   # by header name

echo "# Long run $(basename "$OUT") — $(date -Is)"
echo
for d in "$OUT"/*/; do
  id=$(basename "$d")
  [ -d "$d/migrated" ] || [ -d "$d/official" ] || continue
  echo "## $id"
  echo
  echo "| side | state | step now (age s) | cycles | rounds | checkpoints | archives (first..last) | failed steps | error lines | asserts | cores | PL drops | -1066 lines |"
  echo "|---|---|---|---|---|---|---|---|---|---|---|---|---|"
  for s in migrated official; do
    h=$d/$s/$s/hours.tsv
    [ -f "$h" ] || { echo "| $s | not started |||||||||||"; continue; }
    # the step a side is in: its newest step log; a running side whose step stays the same for 30 min is stalled
    # (README § Watching)
    l=$(ls -t "$d/$s/$s/log" | head -1)
    age=$(( $(date +%s) - $(stat -c %Y "$d/$s/$s/log/$l") ))
    tail -n +2 "$h" | tail -1 | awk -F'\t' -v s="$s" -v st="$(cat "$d/$s/$s/rc" 2>/dev/null)" -v now="${l%.log} ($age)" \
      '{ printf "| %s | %s | %s | %s | %s | %s | %s..%s (%s kept) | %s | %s | %s | %s | %s | %s |\n", s, (st == "running" ? "running" : "rc " st), now, $1, $3, $6, $9, $10, $11, $20, $15, $16, $17, $18, $19 }'
  done
  echo
  if [ -f "$d/migrated/migrated/hours.tsv" ] && [ -f "$d/official/official/hours.tsv" ]; then
    echo "| cycle | at (migrated) | RSS MB mig / off | volume MB mig / off | backup MB mig / off | vacuumed log pages in the statdump window mig / off |"
    echo "|---|---|---|---|---|---|"
    paste <(col "$d/migrated/migrated/hours.tsv" cycle) <(col "$d/migrated/migrated/hours.tsv" at) \
          <(col "$d/migrated/migrated/hours.tsv" rss_kb) <(col "$d/official/official/hours.tsv" rss_kb) \
          <(col "$d/migrated/migrated/hours.tsv" volume_mb) <(col "$d/official/official/hours.tsv" volume_mb) \
          <(col "$d/migrated/migrated/hours.tsv" backup_mb) <(col "$d/official/official/hours.tsv" backup_mb) \
          <(col "$d/migrated/migrated/hours.tsv" vacuumed_log_pages_window) <(col "$d/official/official/hours.tsv" vacuumed_log_pages_window) |
      awk -F'\t' '{ printf "| %s | %s | %d / %d | %s / %s | %s / %s | %s / %s |\n", $1, $2, $3 / 1024, $4 / 1024, $5, $6, $7, $8, $9, $10 }'
    echo
  fi
  # parity so far: the checks both sides have written
  same=0 differ=""
  for f in "$d"/migrated/migrated/check/*.out; do
    [ -f "$f" ] || continue
    o=$d/official/official/check/$(basename "$f")
    [ -f "$o" ] || continue
    if cmp -s "$f" "$o"; then same=$(( same + 1 )); else differ="$differ $(basename "$f" .out)"; fi
  done
  echo "Parity so far: $same checks equal; differ:${differ:- none}"
  echo
  for s in migrated official; do
    for t in length recovery faults; do
      [ -f "$d/$s/$s/$t.tsv" ] && { echo "$s $t: $(tr '\t\n' '= ' <"$d/$s/$s/$t.tsv")"; echo; }
    done
  done
  failed=$(awk -F'\t' 'FNR > 1 && $4 != 0 { print $1 "/" $2 "=" $4 }' "$d"/migrated/steps.tsv "$d"/official/steps.tsv 2>/dev/null | tr '\n' ' ')
  echo "Steps not exited as expected (rc != 0 includes wanted failures; see recovery want): ${failed:-none}"
  echo
done
[ ! -f "$OUT/result" ] || head -1 "$OUT/result"
