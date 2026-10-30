#!/usr/bin/env bash
# Build the catalog templates (ADR 0011): one fresh DB per charset with the target engine's createdb, then
# `cubrid-fastmig template-extract` writes <line>-<charset>.tpl next to this script.
# TEMPLATE_LINE (default develop; 11.4 for an 11.4 install) is the target line the install builds.
#   bash crates/catalog/template/build.sh <work dir>
# <work dir> must not exist; it holds the fresh DBs and is not needed afterwards.
# TEMPLATE_INSTALL (default /home/cubrid/release/CUBRID-fastmig-dev) is the engine install to create the DBs with.
set -euo pipefail
work="${1:?work dir required (must not exist)}"
inst="${TEMPLATE_INSTALL:-/home/cubrid/release/CUBRID-fastmig-dev}"
line="${TEMPLATE_LINE:-develop}"
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
[ ! -e "$work" ] || { echo "$work exists; remove it by literal path first" >&2; exit 1; }

export CUBRID="$inst" CUBRID_DATABASES="$work" PATH="$inst/bin:$PATH" LD_LIBRARY_PATH="$inst/lib"
[ "$(command -v cubrid)" = "$inst/bin/cubrid" ] || { echo "cubrid resolves to $(command -v cubrid)" >&2; exit 1; }
release="$(cubrid_rel | grep -o 'CUBRID [0-9.]* ([^)]*)' | head -1)"   # e.g. CUBRID 11.5.0 (11.5.0.2781-682b080)
mkdir -p "$work"
touch "$work/databases.txt"

# the three charsets a CUBRID DB can have; the system catalog's string domains take the DB charset (ADR 0011)
for locale in en_US.iso88591 ko_KR.euckr en_US.utf8; do
  cs="${locale##*.}"
  db="tpl_$cs"
  mkdir -p "$work/$db"
  ( cd "$work/$db" && cubrid createdb --db-volume-size=20M --log-volume-size=20M \
      --file-path="$work/$db" --log-path="$work/$db" "$db" "$locale" ) > "$work/$db.createdb.out" 2>&1 \
    || { echo "createdb $locale failed:"; tail -20 "$work/$db.createdb.out"; exit 1; }
  ( cd "$repo" && just run template-extract "$work/$db/$db" --out "$here/$line-$cs.tpl" --line "$line" \
      --install "$inst" --release "$release" --locale "$locale" )
done
