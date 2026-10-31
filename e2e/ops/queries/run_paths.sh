#!/usr/bin/env bash
# The basedb query set over the version matrix: one run of run.sh per `clean` small row of e2e/ops-matrix/paths.tsv whose
# target is <target version> (the nine supported pairs across the two targets, T40 workspace#595 "9경로").
#
# Input: bash e2e/ops/queries/run_paths.sh <bin> <basedb root> <target version: develop|11.4> <out root>
# env: FASTMIG_OPS_INSTALL (optdebug, of that target line), FASTMIG_OPS_PORT, FASTMIG_OPS_SERVER_CTL, as run.sh.
# Output: <out root>/<id>/ (run.sh's out dir), <out root>/<id>.console, <out root>/paths.tsv (id, rc, queries, same).
#   Exit 0 when every row passed, 1 otherwise.
# Source: T40 workspace#595; rows: T37 workspace#589 (paths.tsv).
set -uo pipefail
here=$(dirname "$(realpath "$0")")
bin=${1:?bin}; root=$(realpath "${2:?basedb root}"); tv=${3:?target version}; out=${4:?out root}
mkdir -p "$out"
printf 'id\trc\tqueries\tsame\n' >"$out/paths.tsv"
fail=0
while IFS=$'\t' read -r id label variant target size lob_base _; do
  case "$id" in '#'* | '') continue ;; esac
  [ "$target" = "$tv" ] && [ "$variant" = clean ] && [ "$size" = small ] || continue
  lob=""; [ "$lob_base" = - ] || lob=$root/$lob_base
  FASTMIG_OPS_LOB_BASE=$lob bash "$here/run.sh" "$bin" "$root/$label/pristine/$variant" "$tv" basedb "$out/$id" \
    >"$out/$id.console" 2>&1
  rc=$?
  # a lob-base outside the copy leaves the migrated side's source copy (sides.sh prepare_migrated): its literal path
  [ -z "$lob" ] || echo "remove by literal path: $out/$id/migrated/source" >>"$out/$id.console"
  printf '%s\t%s\t%s\t%s\n' "$id" "$rc" "$(awk 'NR > 1' "$out/$id/queries.tsv" 2>/dev/null | wc -l)" \
    "$(awk -F'\t' 'NR > 1 && $4 == "yes"' "$out/$id/queries.tsv" 2>/dev/null | wc -l)" >>"$out/paths.tsv"
  [ "$rc" = 0 ] || fail=1
done <"$here/../../ops-matrix/paths.tsv"
cat "$out/paths.tsv"
exit $fail
