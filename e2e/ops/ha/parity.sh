#!/usr/bin/env bash
# Parity of the HA check (G11 (1), workspace#590): the checks of a migrated run against those of an official run of the
# same baseline. Input: bash e2e/ops/ha/parity.sh <migrated run dir> <official run dir> (both from e2e/ops/ha/run.sh).
# Output: one line per check file ("<phase>-<host> same|DIFF") and <official run dir>/parity/<file>.diff; exit 0 when
# every check file of either run exists in both and is equal. The master-against-slave diffs (<phase>.diff) are
# compared too: a table HA does not replicate must differ the same way on both pairs.
set -uo pipefail
M=${1:?migrated run dir}
O=${2:?official run dir}
grep -q '^side=migrated ' "$M/meta.txt" && grep -q '^side=official ' "$O/meta.txt" ||
  { echo "expected a migrated and an official run" >&2; exit 2; }
mkdir -p "$O/parity"
rc=0
for f in $( (ls "$M/check"; ls "$O/check") | sort -u); do
  if diff "$M/check/$f" "$O/check/$f" >"$O/parity/$f.diff" 2>&1; then
    echo "${f%.*} same"
  else
    echo "${f%.*} DIFF"; rc=1
  fi
done
exit "$rc"
