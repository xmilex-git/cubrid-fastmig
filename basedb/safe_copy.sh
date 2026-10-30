#!/usr/bin/env bash
# Copy a pristine baseline DB so the engine can open the copy without touching the registered DB.
#   bash safe_copy.sh <pristine dir> <new dest dir>
# A reflink copy keeps the registered DB's absolute paths in its _vinf and volume headers; an engine run on it
# opens the registered volumes, and `cubrid deletedb` deletes them (workspace#515). This rewrites every _vinf line
# to the copy's directory (the engine reads _vinf before the headers: boot_sr.c boot_find_rest_permanent_volumes;
# a header path that differs only gives a -114 notification), writes <dest>/databases.txt for the copy, and prints
# the rewritten _vinf. The volume headers keep the old paths: read the copy with cubrid-fastmig
# --volumes-in-first-dir (plan and migrate refuse it otherwise).
# Use: CUBRID_DATABASES=<dest> for the engine. Remove the copy by its literal path with rm -rf, never deletedb.
set -euo pipefail
src="${1:?pristine dir}"; dest="${2:?new dest dir}"
src=$(cd "$src" && pwd)
[ -e "$dest" ] && { echo "error: $dest exists" >&2; exit 2; }
vinfs=("$src"/*_vinf)
[ ${#vinfs[@]} -eq 1 ] && [ -f "${vinfs[0]}" ] || { echo "error: expected one *_vinf in $src" >&2; exit 2; }
db=$(basename "${vinfs[0]}" _vinf)
cp -a --reflink=always "$src" "$dest"
dest=$(cd "$dest" && pwd)
vinf="$dest/${db}_vinf"
# "%4d %s" per line, as the engine writes it (log_page_buffer.c logpb_add_volume)
awk -v d="$dest" '{ n = split($2, p, "/"); printf "%4d %s/%s\n", $1, d, p[n] }' "${vinfs[0]}" > "$vinf.new"
mv "$vinf.new" "$vinf"
while read -r id path; do
  # _bkvinf exists only after a backup
  [ "$id" = -3 ] || [ -e "$path" ] || { echo "error: $vinf names a missing $path" >&2; exit 1; }
done < "$vinf"
if grep -qv "^ *-\?[0-9]* $dest/" "$vinf"; then echo "error: $vinf still names another directory" >&2; exit 1; fi
printf '%s\t%s\tlocalhost\t%s\tfile:%s/lob\n' "$db" "$dest" "$dest" "$dest" > "$dest/databases.txt"
cat "$vinf"
echo "CUBRID_DATABASES=$dest  db=$db"
