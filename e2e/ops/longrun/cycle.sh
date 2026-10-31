# The hourly work of one side in the long run (e2e/ops/longrun/README.md § Cycle): paced DML rounds, the PITR mark,
# online backups in chains, the kill -9 maintenance block, and the cycle's snapshot row. Sourced by side_loop.sh after
# e2e/ops/sides.sh; defines functions only.
#
# Uses sides.sh's globals and side_loop.sh's: ROUNDS (per cycle), CYCLE_SECONDS, KILL_EVERY, CYCLE_START, CHAIN,
# BREAK. Source: T39 workspace#594 (schedule approved by the map supervisor 2026-10-31).

# dml_paced: ROUNDS DML rounds spread over the first 80 % of the cycle, so the server takes writes all hour; round i
# does not start before CYCLE_START + 0.8 * CYCLE_SECONDS * (i - 1) / ROUNDS. The rows a round writes depend on the
# round count only (dml/round.sql), so both sides stay comparable whatever the timing.
dml_paced() {
  local i at
  for ((i = 1; i <= ROUNDS; i++)); do
    at=$(( CYCLE_START + CYCLE_SECONDS * 8 * (i - 1) / (10 * ROUNDS) ))
    (( at <= $(date +%s) )) || sleep $(( at - $(date +%s) ))
    dml_rounds 1 || return 1
  done
}

# lob_base: the side's lob-base directory, from its databases.txt
lob_base() { awk -v d="$DB" '$1 == d { sub(/^file:/, "", $5); print $5 }' "$CUBRID_DATABASES/databases.txt"; }

# pitr_mark <cycle>: the time PITR restores to at the end. Nothing writes between the mark and the snapshot taken right
# after it, so the snapshot is the state at the mark. The LOB files are copied at the mark into <side>/pitr-lob: they
# live outside the volumes, the backups and the log (as an operator keeps a file copy of the lob-base), and a later
# round deletes those its DELETE removes. BREAK=pitr-extra-row writes one row after the mark and before the snapshot:
# the PITR comparison must then fail (README § Fail-before probe).
pitr_mark() {
  sleep 2   # the last round's commit lies a full second before the mark (restoredb -d has second resolution)
  local t; t=$(date +%s)
  printf 'pitr\t%s\t%s\t%s\t%s\n' "$t" "$1" "$(date -d "@$t" +%d-%m-%Y:%H:%M:%S)" "$CHAIN" >>"$OUT/$SIDE/times.tsv"
  cp -a --reflink=always "$(lob_base)" "$OUT/$SIDE/pitr-lob" || return 1
  sleep 2
  if [ "$BREAK" = pitr-extra-row ]; then
    csql -C -u dba -c "INSERT INTO app_owner.region_list (id, code, label) VALUES (100002, 'N', 'after the mark')" "$DB" || return 1
  fi
  check pitr-snapshot
}

# backup_cycle <cycle>: online backup (-C, the server stays up). A cycle that is a multiple of KILL_EVERY (and cycle 0)
# starts a new chain directory with a level 0; the next cycle adds a level 1 and the later ones level 2 (each since that
# level 1, so a later level 2 replacing an earlier one in the directory keeps the chain whole: volume names are
# <db>_bk<level>v<unit>, file_io.c). One directory per chain: a level 0 in the same directory would overwrite the older
# chain's volume that a PITR to an earlier time still needs.
backup_cycle() {
  local c=$1 level dir
  if (( c % KILL_EVERY == 0 )); then
    level=0; CHAIN=$(printf 'chain-c%02d' "$c")
  elif (( c % KILL_EVERY == 1 )); then level=1
  else level=2; fi
  dir=$OUT/$SIDE/backup/$CHAIN
  mkdir -p "$dir"
  echo "level $level into $dir"
  cubrid backupdb -C -l "$level" -D "$dir" "$DB" || return 1
  printf 'backup\t%s\t%s\t%s\t%s\n' "$(date +%s)" "$c" "$level" "$CHAIN" >>"$OUT/$SIDE/times.tsv"
}


# statdump_to <file>: the server's counters over STAT_SECONDS. statdump counts from its own start (histo_start), so a
# single dump is all zeros; -i N -c prints at the start and after N seconds, and stat_of reads the last dump.
statdump_to() { timeout $(( STAT_SECONDS + 15 )) cubrid statdump -i "$STAT_SECONDS" -c "$DB" >"$1" || [ -s "$1" ]; }
stat_of() { awk -F'=' -v k="$2" '{ gsub(/[[:space:]]/, "", $1) } $1 == k { gsub(/[[:space:]]/, "", $2); v = $2 } END { print v + 0 }' "$1"; }
# checkpoints the side's servers finished since side_begin, from their error logs (any server lifetime, kill -9 included)
checkpoints() { cat /dev/null "$OUT/$SIDE"/server-log/"${DB}"_[0-9]*.err | grep -c '^Checkpoint finished'; }

# maintenance <cycle>: every KILL_EVERY cycles: kill -9 of the server, vacuumdb -S once before the restart (its SA boot
# runs crash recovery first; shows whether vacuum catches up after it), compactdb -S on the first such cycle only, then
# start.
maintenance() {
  local c=$1 n; n=$(printf 'c%02d' "$c")
  step "$n-kill9" 0 kill9_server || return 1
  step "$n-vacuumdb" 0 cubrid vacuumdb -S "$DB"
  (( c != KILL_EVERY )) || step "$n-compactdb" 0 cubrid compactdb -S "$DB"
  step "$n-start" 0 start || return 1
  step "check-$n-restart" 0 check "$n-restart"
}

# snapshot <cycle>: one row of <side>/hours.tsv (header in side_loop.sh): the server's RSS, STAT_SECONDS of its vacuum
# counters, checkpoints, archives, sizes, faults so far; the statdump kept in <side>/stat/
snapshot() {
  local c=$1 n f pid rss first last a vol bk e a2 co pl se failed
  n=$(printf 'c%02d' "$c"); f=$OUT/$SIDE/stat/$n.txt
  statdump_to "$f" || return 1
  pid=$(server_pid)
  rss=$(awk '/^VmRSS:/ { print $2 }' "/proc/$pid/status" 2>/dev/null)
  first=$(ls "$SIDE_DB" | sed -n "s/^${DB}_lgar\([0-9][0-9]*\)\$/\1/p" | sort -n | head -1 | awk '{ print $1 + 0 }')
  last=$(last_archive)
  a=$(ls "$SIDE_DB" | grep -c "^${DB}_lgar[0-9]")
  vol=$(ls "$SIDE_DB" | grep -E "^${DB}(_x[0-9]+)?\$" | sed "s#^#$SIDE_DB/#" | xargs du -cm | tail -1 | cut -f1)
  bk=$(du -sm "$OUT/$SIDE/backup" | cut -f1)
  read -r e a2 co pl se < <(side_faults)
  failed=$(awk -F'\t' -v s="$SIDE" '$1 == s && $4 != 0' "$OUT/steps.tsv" | wc -l)
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$c" "$(date -Is)" "$(( c * ROUNDS ))" \
    "$pid" "${rss:-}" "$(checkpoints)" "$(stat_of "$f" Num_vacuum_log_pages_vacuumed)" "$(stat_of "$f" Num_vacuum_log_pages_to_vacuum)" \
    "${first:-}" "$last" "$a" "$vol" "$bk" "$(df -m --output=avail /home | tail -1 | tr -d ' ')" "$e" "$a2" "$co" "$pl" "$se" "$failed" \
    >>"$OUT/$SIDE/hours.tsv"
}
