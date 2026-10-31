#!/usr/bin/env bash
# Writes the section "## Version matrix" of docs/ops-verification.md, the pass table of the T37 version matrix, from
# the runs run_lane.sh left (workspace#589). The other sections of the document (HA, T38) are kept as they are.
#
# Input: bash e2e/ops-matrix/summarize.sh <out root> <document>
#   <out root>  the run_lane.sh out root; per row of paths.tsv the highest <id>/run-NN is read
#   <document>  docs/ops-verification.md: its "## Version matrix" section is replaced (up to the next "## " heading),
#               or inserted before the first "## " heading when it has none
# Reads per run: run-NN.rc, steps.tsv (side step seconds rc: migrated/official step and check-<step> rows, parity
#   rows), server-log-summary.tsv, archives.tsv, meta.txt; lane-*-install.txt; defects.tsv (row id -> defect ticket).
# Output: the document rewritten in place, the section alone also in <out root>/ops-verification-section.md. Exit 0,
#   also when rows failed or did not run (the table says so); 2 bad arguments.
# Source: T37 workspace#589 (pass rule: assert, core and error log 0, parity diff 0, log archives >= 2 after the
#   rounds); runner output e2e/ops/README.md § Output.
set -uo pipefail

ROOT=$(realpath "${1:?out root}")
DOC=${2:?document (docs/ops-verification.md)}
[ -f "$DOC" ] || { echo "no $DOC" >&2; exit 2; }
MATRIX=$(dirname "$(realpath "$0")")

# col <tsv> <header name> <side>: the named column of the side's row (the files carry a header line)
col() {
  awk -F'\t' -v c="$2" -v s="$3" 'NR == 1 { for (i = 1; i <= NF; i++) if ($i == c) k = i; next }
    $1 == s && k { print $k; exit }' "$1" 2>/dev/null
}
# side_logs <server-log-summary.tsv> <side>: errors/asserts/cores/PL drops/session expired of one side
side_logs() {
  echo "$(col "$1" error_lines "$2")/$(col "$1" assert_lines "$2")/$(col "$1" cores "$2")/$(col "$1" pl_drops "$2")/$(col "$1" session_expired "$2")"
}

SECTION=$ROOT/ops-verification-section.md
{
cat <<'EOF'
## Version matrix

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
*Err/Assert/Core/PL/Exp* = migrated, official (PL = "Networking with PL server failed" lines, a known flake counted apart, e2e/ops/README.md § Known flakes; Exp = engine "-1066 Session expired" lines, not errors, § Engine facts); *Archives* = log archives created on the migrated side (official) / minimum.
*Verdict* = the runner's exit code of the shown run (the highest `run-NN`); *Earlier runs* = every run before it, with its exit code and PL server drops (rerun rule: e2e/ops-matrix/README.md § Lanes).

EOF

echo '### Engines and binary'
echo
for f in "$ROOT"/lane-*-install.txt; do
  [ -f "$f" ] || continue
  printf -- '- `%s`: %s; %s; libcubrid sha256 `%.16s…`; cubrid-fastmig sha256 `%.16s…`\n' \
    "$(basename "$f" -install.txt)" "$(sed -n 's/.*install=\([^ ]*\).*/\1/p' "$f" | xargs basename)" \
    "$(sed -n 2p "$f")" "$(sed -n 's/^libcubrid_sha256=//p' "$f")" "$(sed -n 's/^bin_sha256=//p' "$f")"
done
echo
echo '### Table'
echo
echo '| Row | Source → target | Variant | Rounds | Steps | Parity | Err/Assert/Core/PL/Exp | Archives | Verdict | Earlier runs | Defect |'
echo '|---|---|---|---|---|---|---|---|---|---|---|'

# awk prologue: h[<header name>] = column number, from the header line every runner TSV carries
HDR='NR == 1 { for (i = 1; i <= NF; i++) h[$i] = i; next }'

# earlier_runs <row dir> <shown run>: "run-NN rc R, PL P" for every run of the row before the shown one, so a pass
# after a rerun stays visible (rerun rule: README § Lanes)
earlier_runs() {
  local d out="" pl
  for d in $(ls -d "$1"/run-[0-9][0-9] 2>/dev/null | sort); do
    [ "$d" = "$2" ] && continue
    pl=$(awk -F'\t' "$HDR"' { p += $h["pl_drops"] } END { print p + 0 }' "$d/server-log-summary.tsv" 2>/dev/null)
    out="$out${out:+; }$(basename "$d") rc $(cat "$d.rc" 2>/dev/null || echo '?'), PL ${pl:-?}"
  done
  echo "$out"
}

while IFS=$'\t' read -r id label variant target size lob rounds min; do
  case "$id" in '' | '#'*) continue ;; esac
  defect=$(awk -F'\t' -v i="$id" '$1 == i { print $2 }' "$MATRIX/defects.tsv" 2>/dev/null | paste -sd' ')
  [ "$size" = big ] && variant="$variant (4.2 GB)"
  run=$(ls -d "$ROOT/$id"/run-[0-9][0-9] 2>/dev/null | sort | tail -1)
  if [ -z "$run" ]; then
    printf '| `%s` | %s → %s | %s | | | | | | not run | | %s |\n' "$id" "$label" "$target" "$variant" "$defect"
    continue
  fi
  rc=$(cat "$run.rc" 2>/dev/null || echo '?')
  st=$run/steps.tsv
  steps=$(awk -F'\t' "$HDR"' $h["side"] != "parity" { n++; if ($h["rc"] != 0) f++ } END { printf "%d / %d", f, n }' "$st" 2>/dev/null)
  failed=$(awk -F'\t' "$HDR"' $h["side"] != "parity" && $h["rc"] != 0 { printf "%s%s:%s", s, $h["side"], $h["step"]; s = ", " }' "$st" 2>/dev/null)
  parity=$(awk -F'\t' "$HDR"' $h["side"] == "parity" { n++; if ($h["rc"] != 0) f++ } END { printf "%d / %d", f, n }' "$st" 2>/dev/null)
  differ=$(awk -F'\t' "$HDR"' $h["side"] == "parity" && $h["rc"] != 0 { printf "%s%s", s, $h["step"]; s = ", " }' "$st" 2>/dev/null)
  sl=$run/server-log-summary.tsv
  logs="$(side_logs "$sl" migrated), $(side_logs "$sl" official)"
  created='{ print $h["last_archive_after"] - $h["last_archive_before"] }'
  am=$(awk -F'\t' "$HDR"' $h["side"] == "migrated" '"$created" "$run/archives.tsv" 2>/dev/null)
  ao=$(awk -F'\t' "$HDR"' $h["side"] == "official" '"$created" "$run/archives.tsv" 2>/dev/null)
  amin=$(sed -n 's/^archives created on migrated: .* (min \([0-9]*\))$/\1/p' "$run/meta.txt" 2>/dev/null)
  r=$(sed -n 's/.* rounds=\([0-9]*\) .*/\1/p' "$run/meta.txt" 2>/dev/null | head -1)
  if [ "$rc" = 0 ]; then verdict=pass; else verdict="**FAIL** (rc $rc)"; fi
  [ -z "$failed" ] || steps="$steps ($failed)"
  [ -z "$differ" ] || parity="$parity ($differ)"
  printf '| `%s` | %s → %s | %s | %s | %s | %s | %s | %s (%s) / %s | %s | %s | %s |\n' "$id" "$label" "$target" "$variant" \
    "$r" "$steps" "$parity" "$logs" "${am:-?}" "${ao:-?}" "${amin:-?}" "$verdict (\`$(basename "$run")\`)" \
    "$(earlier_runs "$ROOT/$id" "$run")" "$defect"
done <"$MATRIX/paths.tsv"

echo
echo "Runs: \`$(basename "$ROOT")\`, the highest \`run-NN\` per row; generated $(date -Is) by e2e/ops-matrix/summarize.sh."
} >"$SECTION"

# splice: replace the old section (its heading up to the next "## "), or insert before the first "## " heading
has=0; grep -qx '## Version matrix' "$DOC" && has=1
awk -v sec="$SECTION" -v has="$has" '
  function put() { while ((getline l < sec) > 0) print l; print ""; done = 1 }
  /^## Version matrix$/ { put(); skip = 1; next }
  skip && /^## / { skip = 0 }
  skip { next }
  /^## / && !done && !has { put() }
  { print }
  END { if (!done) { print ""; put() } }
' "$DOC" >"$DOC.new" && mv "$DOC.new" "$DOC"
