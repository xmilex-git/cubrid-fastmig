#!/usr/bin/env bash
# Medians of 3 reps per cell from results/{ul114,same114}.tsv (rows with rc != 0 or a rep listed in
# results/disturbed.txt are left out and counted). fastmig stages: top-level report.json keys only (no "a.b" sub-keys).
#   bash summarize.sh
S=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/559
med() { sort -n | awk '{a[NR]=$1} END {if (NR == 0) print "-"; else if (NR % 2) printf "%.3f\n", a[(NR+1)/2]; else printf "%.3f\n", (a[NR/2]+a[NR/2+1])/2}'; }
dist=$S/results/disturbed.txt; [ -f $dist ] || : > $dist
if [ -f $S/results/ul114.tsv ]; then
  echo "### official path 11.4.6 unloaddb -> 11.4.6 loaddb (s, median; n = runs used)"
  echo "| mode | W | n | unload | createdb | schema | data | index | trigger | total (no server start/stop) | server start+stop |"
  echo "|---|---|---|---|---|---|---|---|---|---|---|"
  for cell in "CS 16" "SA 1"; do
    set -- $cell
    keep=$(awk -v m=$1 -v w=$2 '$1 == m && $2 == w {print $3}' $S/results/ul114.tsv | sort -u | while read -r r; do
      /usr/bin/grep -qx "ul-$1-w$2-r$r" $dist && continue
      awk -v m=$1 -v w=$2 -v r=$r '$1 == m && $2 == w && $3 == r && $6 != 0 {bad = 1} END {exit bad}' $S/results/ul114.tsv && echo $r
    done)
    [ -n "$keep" ] || continue
    n=$(echo "$keep" | wc -l); line="| $1 | $2 | $n"
    for st in unload createdb schema data index trigger; do
      line="$line | $(for r in $keep; do awk -v m=$1 -v w=$2 -v r=$r -v s=$st '$1 == m && $2 == w && $3 == r && $4 == s {print $5}' $S/results/ul114.tsv; done | med)"
    done
    tot=$(for r in $keep; do awk -v m=$1 -v w=$2 -v r=$r '$1 == m && $2 == w && $3 == r && $4 ~ /^(unload|createdb|schema|data|index|trigger)$/ {t += $5} END {print t}' $S/results/ul114.tsv; done | med)
    srv=$(for r in $keep; do awk -v m=$1 -v w=$2 -v r=$r '$1 == m && $2 == w && $3 == r && $4 ~ /srv_(start|stop)$/ {t += $5} END {print t}' $S/results/ul114.tsv; done | med)
    echo "$line | $tot | $srv |"
  done
fi
if [ -f $S/results/fm.tsv ]; then
  F=$S/results/fm.tsv
  echo
  echo "### fastmig and cp, W=16 (s; warm = median of reps 1-3 with rc 0 not in disturbed.txt; cold = rep 0)"
  echo "| path | method | stage | warm median | warm reps | cold r0 |"
  echo "|---|---|---|---|---|---|"
  for path in A B; do for m in cp_a cp_reflink fm_clone fm_copy; do
    keep=$(awk -v p=$path -v m=$m '$1 == p && $2 == m && $3 != 0 {print $3}' $F | sort -u | while read -r r; do
      /usr/bin/grep -qx "$path-$m-r$r" $dist && continue
      awk -v p=$path -v m=$m -v r=$r '$1 == p && $2 == m && $3 == r && $6 != 0 {bad = 1} END {exit bad}' $F && echo $r
    done)
    [ -n "$keep" ] || continue
    for st in $(awk -v p=$path -v m=$m '$1 == p && $2 == m && $4 !~ /\./ {print $4}' $F | awk '!seen[$0]++') copy_stage; do
      val() { if [ $st = copy_stage ]; then awk -v p=$path -v m=$m -v r=$1 '$1 == p && $2 == m && $3 == r && $4 ~ /^(volumes|fsync)$/ {t += $5; n++} END {if (n) print t}' $F
              else awk -v p=$path -v m=$m -v r=$1 -v s=$st '$1 == p && $2 == m && $3 == r && $4 == s {print $5}' $F; fi; }
      vals=$(for r in $keep; do val $r; done)
      [ -n "$vals" ] || continue
      cold=$(val 0); echo "| $path | $m | $st | $(echo "$vals" | med) | $(echo $vals | tr ' ' '/') | ${cold:--} |"
    done
  done; done
fi
