#!/usr/bin/env bash
# Runs one lane of the T37 version matrix: the rows of paths.tsv for one target, each through e2e/ops/run.sh, one
# after the other on one install and one port (workspace#589).
#
# Input: bash e2e/ops-matrix/run_lane.sh <bin> <lane: develop|11.4|big> <out root> [id...]
#   <bin>        the cubrid-fastmig release binary
#   <lane>       develop / 11.4: the small rows of that target; big: the 4.2 GB row (one round)
#   <out root>   the matrix's run directory, shared by the lanes; created when missing
#   [id...]      only these rows of the lane (a rerun); default every row of the lane
# env: FASTMIG_OPS_INSTALL, FASTMIG_OPS_PORT, FASTMIG_OPS_SERVER_CTL (required, passed to the runner; the install is the
#   lane's own reflink copy of the optdebug target install, G11 decision 6), FASTMIG_OPS_MATRIX_BASEDB_ROOT (small rows,
#   default ~/fastmig-basedb), FASTMIG_OPS_MATRIX_BIG_ROOT and FASTMIG_OPS_MATRIX_BIG_UNLOAD (big lane: the root holding
#   11.4/pristine/clean and the 11.4 unloaddb prefix of that DB, big_unload.sh), FASTMIG_OPS_MATRIX_DRY_RUN=1 (print
#   each row's runner command after checking its pristine, unload and LOB base exist; writes nothing, runs nothing).
# Output: <out root>/<id>/run-NN/ (the runner's out dir, NN the next free number), <out root>/<id>/run-NN.rc,
#   a row "id run rc seconds" per run in <out root>/lane-<lane>.tsv, the install's cubrid_rel and libcubrid sha256 in
#   <out root>/lane-<lane>-install.txt. Exit 0 when every run exited 0, else 1; 2 bad arguments.
# Source: T37 workspace#589 (matrix, lanes), G11 workspace#587; runner interface e2e/ops/README.md.
set -uo pipefail

BIN=$(realpath -m "${1:?cubrid-fastmig binary}")
LANE=${2:?lane: develop, 11.4 or big}
ROOT=${3:?out root}
shift 3
MATRIX=$(dirname "$(realpath "$0")")
RUNNER=$MATRIX/../ops/run.sh
INST=$(realpath "${FASTMIG_OPS_INSTALL:?FASTMIG_OPS_INSTALL required}")
: "${FASTMIG_OPS_PORT:?FASTMIG_OPS_PORT required}" "${FASTMIG_OPS_SERVER_CTL:?FASTMIG_OPS_SERVER_CTL required}"
SMALL=${FASTMIG_OPS_MATRIX_BASEDB_ROOT:-$HOME/fastmig-basedb}
DRY=${FASTMIG_OPS_MATRIX_DRY_RUN:-0}
# the runner overwrites the install's conf/cubrid.conf: never a shared install other sessions use (a dry run only reads it)
[ "$DRY" = 1 ] || case "$INST" in "$HOME"/optdebug/* | "$HOME"/release/*) echo "use the lane's own reflink copy, not $INST" >&2; exit 2 ;; esac

case "$LANE" in
  develop | 11.4) want_target=$LANE want_size=small ;;
  big) want_target=develop want_size=big
    : "${FASTMIG_OPS_MATRIX_BIG_ROOT:?big lane needs FASTMIG_OPS_MATRIX_BIG_ROOT}"
    [ -s "${FASTMIG_OPS_MATRIX_BIG_UNLOAD:?big lane needs FASTMIG_OPS_MATRIX_BIG_UNLOAD}_schema" ] || [ "$DRY" = 1 ] ||
      { echo "no unload ${FASTMIG_OPS_MATRIX_BIG_UNLOAD}_schema (run big_unload.sh)" >&2; exit 2; } ;;
  *) echo "lane must be develop, 11.4 or big" >&2; exit 2 ;;
esac
[ -f "$RUNNER" ] || { echo "no runner $RUNNER" >&2; exit 2; }
[ -x "$BIN" ] || [ "$DRY" = 1 ] || { echo "no binary $BIN (just release)" >&2; exit 2; }

# the install must be the lane's target line and an assert-enabled build: a wrong install verifies nothing (G11 6)
rel=$(CUBRID=$INST LD_LIBRARY_PATH=$INST/lib "$INST/bin/cubrid_rel" 2>&1 | grep -m1 'CUBRID')
case "$rel" in *"release build"*) echo "install is a release build: $rel" >&2; exit 2 ;; esac
case "$want_target:$rel" in
  11.4:*"CUBRID 11.4."* | develop:*) ;;
  *) echo "install is not the 11.4 line: $rel" >&2; exit 2 ;;
esac
[ "$want_target" = 11.4 ] || [[ "$rel" != *"CUBRID 11.4."* ]] || { echo "install is not develop: $rel" >&2; exit 2; }

ROOT=$(realpath -m "$ROOT")
if [ "$DRY" = 1 ]; then
  echo "dry run, install: $rel"
else
  mkdir -p "$ROOT"
  { echo "lane=$LANE install=$INST port=$FASTMIG_OPS_PORT"
    echo "$rel"
    echo "libcubrid_sha256=$(sha256sum "$INST/lib/libcubrid.so" | cut -d' ' -f1)"
    echo "bin_sha256=$(sha256sum "$BIN" | cut -d' ' -f1)"
  } >"$ROOT/lane-$LANE-install.txt"
fi

FAIL=0
while IFS=$'\t' read -r id label variant target size lob rounds min; do
  case "$id" in '' | '#'*) continue ;; esac
  [ "$target" = "$want_target" ] && [ "$size" = "$want_size" ] || continue
  if [ $# -gt 0 ]; then
    hit=0; for a in "$@"; do [ "$a" = "$id" ] && hit=1; done
    [ $hit = 1 ] || continue
  fi
  if [ "$size" = big ]; then base=$FASTMIG_OPS_MATRIX_BIG_ROOT; else base=$SMALL; fi
  pristine=$base/$label/pristine/$variant
  [ -d "$pristine" ] || { echo "$id: no pristine $pristine" >&2; FAIL=1; continue; }
  # every run gets a new out dir: a rerun never reads what an earlier run left (common.md speed rules)
  n=1; while [ -e "$ROOT/$id/$(printf 'run-%02d' $n)" ]; do n=$((n + 1)); done
  run=$(printf 'run-%02d' $n)
  env=()
  [ "$lob" = - ] || env+=("FASTMIG_OPS_LOB_BASE=$base/$lob")
  [ "$min" = - ] || env+=("FASTMIG_OPS_MIN_ARCHIVES=$min")
  [ "$size" != big ] || env+=("FASTMIG_OPS_SOURCE_UNLOAD=$FASTMIG_OPS_MATRIX_BIG_UNLOAD")
  args=("$BIN" "$pristine" "$target" "$ROOT/$id/$run")
  [ "$rounds" = - ] || args+=("$rounds")
  echo "== $id $run: ${env[*]} bash $(realpath "$RUNNER") ${args[*]}"
  if [ "$DRY" = 1 ]; then
    # the inputs the runner refuses to start without (e2e/ops/run.sh argument checks)
    vinf=$(ls "$pristine"/*_vinf 2>/dev/null | head -1)
    unload=$base/$label/unload/$variant/$(basename "$vinf" _vinf)
    [ "$size" != big ] || unload=$FASTMIG_OPS_MATRIX_BIG_UNLOAD
    [ -n "$vinf" ] || { echo "   missing *_vinf in $pristine"; FAIL=1; }
    [ -s "${unload}_schema" ] || { echo "   missing unload ${unload}_schema"; FAIL=1; }
    [ "$lob" = - ] || [ -d "$base/$lob" ] || { echo "   missing LOB base $base/$lob"; FAIL=1; }
    continue
  fi
  mkdir -p "$ROOT/$id"
  s=$(date +%s)
  # a subshell, not env(1): a ~/.local/bin/env shim shadows /usr/bin/env on the dev host
  ( for kv in "${env[@]}"; do export "$kv"; done; exec bash "$RUNNER" "${args[@]}" ) >"$ROOT/$id/$run.console" 2>&1
  rc=$?
  echo "$rc" >"$ROOT/$id/$run.rc"
  printf '%s\t%s\t%s\t%s\n' "$id" "$run" "$rc" "$(($(date +%s) - s))" >>"$ROOT/lane-$LANE.tsv"
  echo "   rc=$rc"
  [ "$rc" = 0 ] || FAIL=1
done <"$MATRIX/paths.tsv"
exit $FAIL
