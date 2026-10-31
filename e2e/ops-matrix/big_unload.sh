#!/usr/bin/env bash
# Makes the official side's input for the big row of the T37 matrix: the 11.4 engine's unloaddb of the 4.2 GB 11.4
# baseline DB, which keeps no unload of its own (workspace#589).
#
# Input: bash e2e/ops-matrix/big_unload.sh <big pristine> <work dir> <11.4 install>
#   <big pristine>  <FASTMIG_OPS_MATRIX_BIG_ROOT>/11.4/pristine/clean; only read (reflink copied)
#   <work dir>      a new directory: <work dir>/copy (the safe copy, removed by the caller by literal path once this
#                   exits) and <work dir>/unload/<db>_* (the result, FASTMIG_OPS_MATRIX_BIG_UNLOAD=<work dir>/unload/<db>)
#   <11.4 install>  the caller's own reflink copy of the 11.4 release install (the source line, as the small
#                   baselines' unloads): unloaddb -S writes into its log/
# Output: the unload, <work dir>/unload.log, <work dir>/unload.sha256 (sha256 of every unload file). Exit 0 on success.
# Source: basedb/safe_copy.sh (an engine run on a plain reflink copy opens the registered volumes, workspace#515);
#   e2e/ops/README.md (FASTMIG_OPS_SOURCE_UNLOAD).
set -euo pipefail

PRISTINE=$(realpath "${1:?big pristine}")
WORK=${2:?work dir}
INST=$(realpath "${3:?11.4 install}")
BASEDB=$(dirname "$(realpath "$0")")/../../basedb

[ ! -e "$WORK" ] || { echo "$WORK exists" >&2; exit 2; }
CUBRID=$INST LD_LIBRARY_PATH=$INST/lib "$INST/bin/cubrid_rel" | grep -q 'CUBRID 11\.4\.' ||
  { echo "$INST is not an 11.4 install" >&2; exit 2; }
mkdir -p "$WORK/unload"
WORK=$(realpath "$WORK")

# safe_copy rewrites _vinf to the copy and prints it: every line must name the copy before the engine opens it
bash "$BASEDB/safe_copy.sh" "$PRISTINE" "$WORK/copy" | tee "$WORK/safe_copy.log"
db=$(basename "$(ls "$WORK"/copy/*_vinf)" _vinf)

export CUBRID=$INST CUBRID_DATABASES=$WORK/copy
export PATH=$INST/bin:$PATH LD_LIBRARY_PATH=$INST/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
cd "$WORK/unload"
# the same command basedb/build.sh unloads the small baselines with
cubrid unloaddb -S -u dba "$db" >"$WORK/unload.log" 2>&1
(cd "$WORK/unload" && sha256sum "$db"_*) >"$WORK/unload.sha256"
du -sm "$WORK/unload" >>"$WORK/unload.log"
echo "FASTMIG_OPS_MATRIX_BIG_UNLOAD=$WORK/unload/$db"
echo "remove the copy by its literal path: rm -rf $WORK/copy"
