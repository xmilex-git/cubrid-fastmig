#!/usr/bin/env bash
# Builds docs/ops-verification.md, the pass table of the T37 version matrix, from the runs run_lane.sh left
# (workspace#589).
#
# Input: bash e2e/ops-matrix/summarize.sh <out root> > docs/ops-verification.md
#   <out root>  the run_lane.sh out root; per row of paths.tsv the highest <id>/run-NN is read
# Reads per run: run-NN.rc, steps.tsv (side step seconds rc: migrated/official step and check-<step> rows, parity
#   rows), server-log-summary.tsv, archives.tsv, meta.txt; lane-*-install.txt; defects.tsv (row id -> defect ticket).
# Output: the markdown document on stdout. Exit 0, also when rows failed or did not run (the table says so).
# Source: T37 workspace#589 (pass rule: assert, core and error log 0, parity diff 0, log archives >= 2 after the
#   rounds); runner output e2e/ops/README.md § Output.
set -uo pipefail

ROOT=$(realpath "${1:?out root}")
MATRIX=$(dirname "$(realpath "$0")")

# col <tsv> <header name> <side>: the named column of the side's row (the files carry a header line)
col() {
  awk -F'\t' -v c="$2" -v s="$3" 'NR == 1 { for (i = 1; i <= NF; i++) if ($i == c) k = i; next }
    $1 == s && k { print $k; exit }' "$1" 2>/dev/null
}
# side_logs <server-log-summary.tsv> <side>: errors/asserts/cores/PL drops of one side
side_logs() {
  echo "$(col "$1" error_lines "$2")/$(col "$1" assert_lines "$2")/$(col "$1" cores "$2")/$(col "$1" pl_drops "$2")"
}

cat <<'EOF'
# Operations verification: the version matrix (workspace#589)

Each row takes one baseline DB to the target version two ways — cubrid-fastmig (*migrated*) and the official path,
source `unloaddb` → target `loaddb` (*official*) — and runs the same operations scenario on both
([e2e/ops/README.md](../e2e/ops/README.md)): DML rounds over every object kind, UPDATE STATISTICS, checkpoint,
`kill -9` and restart recovery, vacuumdb, addvoldb, compactdb, backupdb, one more DML round, restoredb. After each
step both sides run the same checks; the two outputs must be equal (*parity*). Matrix and pass rule: G11
[workspace#587](https://github.com/xmilex-git/workspace/issues/587), T37
[workspace#589](https://github.com/xmilex-git/workspace/issues/589); rows: [e2e/ops-matrix/paths.tsv](../e2e/ops-matrix/paths.tsv).

A row passes when the runner exited 0: every step and check exited 0, every parity diff is empty, the server error
logs of both sides hold no `ERROR ***` line and no assert, no core of either side's servers appeared, and the
migrated side created at least the minimum number of log archives (2 for the small DBs; 0 for the 4.2 GB DB, which
runs one round). Server statistics and error-log wording are not compared (G11 decision 1).

Columns: *Steps* = failed / all step and check rows of both sides; *Parity* = differing / all checked steps;
*Err/Assert/Core/PL* = migrated, official (PL = "Networking with PL server failed" lines, a known flake counted apart, e2e/ops/README.md § Known flakes); *Archives* = log archives created on the migrated side (official) / minimum.

EOF

echo '## Engines and binary'
echo
for f in "$ROOT"/lane-*-install.txt; do
  [ -f "$f" ] || continue
  printf -- '- `%s`: %s; %s; libcubrid sha256 `%.16s…`; cubrid-fastmig sha256 `%.16s…`\n' \
    "$(basename "$f" -install.txt)" "$(sed -n 's/.*install=\([^ ]*\).*/\1/p' "$f" | xargs basename)" \
    "$(sed -n 2p "$f")" "$(sed -n 's/^libcubrid_sha256=//p' "$f")" "$(sed -n 's/^bin_sha256=//p' "$f")"
done
echo
echo '## Table'
echo
echo '| Row | Source → target | Variant | Rounds | Steps | Parity | Err/Assert/Core/PL | Archives | Verdict | Defect |'
echo '|---|---|---|---|---|---|---|---|---|---|'

while IFS=$'\t' read -r id label variant target size lob rounds min; do
  case "$id" in '' | '#'*) continue ;; esac
  defect=$(awk -F'\t' -v i="$id" '$1 == i { print $2 }' "$MATRIX/defects.tsv" 2>/dev/null | paste -sd' ')
  [ "$size" = big ] && variant="$variant (4.2 GB)"
  run=$(ls -d "$ROOT/$id"/run-[0-9][0-9] 2>/dev/null | sort | tail -1)
  if [ -z "$run" ]; then
    printf '| `%s` | %s → %s | %s | | | | | | not run | %s |\n' "$id" "$label" "$target" "$variant" "$defect"
    continue
  fi
  rc=$(cat "$run.rc" 2>/dev/null || echo '?')
  steps=$(awk -F'\t' 'NR > 1 && $1 != "parity" { n++; if ($4 != 0) f++ } END { printf "%d / %d", f, n }' "$run/steps.tsv" 2>/dev/null)
  failed=$(awk -F'\t' 'NR > 1 && $1 != "parity" && $4 != 0 { printf "%s%s:%s", s, $1, $2; s = ", " }' "$run/steps.tsv" 2>/dev/null)
  parity=$(awk -F'\t' '$1 == "parity" { n++; if ($4 != 0) f++ } END { printf "%d / %d", f, n }' "$run/steps.tsv" 2>/dev/null)
  differ=$(awk -F'\t' '$1 == "parity" && $4 != 0 { printf "%s%s", s, $2; s = ", " }' "$run/steps.tsv" 2>/dev/null)
  sl=$run/server-log-summary.tsv
  logs="$(side_logs "$sl" migrated), $(side_logs "$sl" official)"
  ar=$run/archives.tsv
  am=$(awk -F'\t' '$1 == "migrated" { print $3 - $2 }' "$ar" 2>/dev/null)
  ao=$(awk -F'\t' '$1 == "official" { print $3 - $2 }' "$ar" 2>/dev/null)
  amin=$(sed -n 's/^archives created on migrated: .* (min \([0-9]*\))$/\1/p' "$run/meta.txt" 2>/dev/null)
  r=$(sed -n 's/.* rounds=\([0-9]*\) .*/\1/p' "$run/meta.txt" 2>/dev/null | head -1)
  if [ "$rc" = 0 ]; then verdict=pass; else verdict="**FAIL** (rc $rc)"; fi
  [ -z "$failed" ] || steps="$steps ($failed)"
  [ -z "$differ" ] || parity="$parity ($differ)"
  printf '| `%s` | %s → %s | %s | %s | %s | %s | %s | %s (%s) / %s | %s | %s |\n' "$id" "$label" "$target" "$variant" \
    "$r" "$steps" "$parity" "$logs" "${am:-?}" "${ao:-?}" "${amin:-?}" "$verdict" "$defect"
done <"$MATRIX/paths.tsv"

echo
echo "Runs: \`$(basename "$ROOT")\`, the highest \`run-NN\` per row; generated $(date -Is) by e2e/ops-matrix/summarize.sh."
