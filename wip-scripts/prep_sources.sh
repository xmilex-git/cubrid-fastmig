#!/usr/bin/env bash
# Sources of #561 on the three devices: the pristine ~2 GB DBs without log archives, read with --volumes-in-first-dir.
# NVMe: reflink copies (same filesystem as the pristine); SATA SSD and HDD: byte copies. Ends with "PREP DONE".
set -euo pipefail
. "$(dirname "$0")/env.sh"
mk() {   # mk <dev> <src name> <pristine> <db>
  local dir; dir=$(devdir $1)/$2
  [ ! -e "$dir" ] || { echo "$dir exists" >&2; return 1; }
  mkdir -p "$dir/db"
  mapfile -t items < <(ls -A "$3" | /usr/bin/grep -v _lgar)
  if [ "$1" = nvme ]; then cp -a --reflink=always "${items[@]/#/$3/}" "$dir/db/"; else taskset -c 0-15 cp -a "${items[@]/#/$3/}" "$dir/db/"; fi
  printf '%s\t%s\tlocalhost\t%s\tfile:%s/lob\n' $4 "$dir/db" "$dir/db" "$dir/db" > "$dir/databases.txt"
  echo "$1 $2: $(du -sm --apparent-size "$dir/db" | cut -f1) MB apparent"
}
df -h /home /bench/ssd /bench/hdd
for dev in nvme ssd hdd; do
  mkdir -p "$(devdir $dev)/meas"
  mk $dev src114 $P114 fm114_clean
  mk $dev srcdev $PDEV fmdev_clean
done
sync
echo "PREP DONE"
