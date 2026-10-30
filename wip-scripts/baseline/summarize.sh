#!/usr/bin/env bash
# Markdown tables from results/ul.tsv and results/cp.tsv: per cell (input, mode, W) the median of each stage over
# its reps (n = number of reps; n = 1 is a single value, not a median). total = unload + createdb + schema + data +
# index + trigger; server start/stop are listed apart and not in the total. Runs listed in results/disturbed.txt
# (another job overlapped them) stay out of the medians and are listed with their values below the tables.
set -euo pipefail
R=/home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/results
for in in small big; do
  echo "### $in: unload -> load (seconds)"
  echo
  echo "| mode | W | n | unload | createdb | schema | data | index | trigger | total | server start+stop |"
  echo "|---|---|---|---|---|---|---|---|---|---|---|"
  awk -F'\t' -v want="$in" '
    function med(key,   n, i, j, t, a) { n = cnt[key]; for (i = 1; i <= n; i++) a[i] = v[key, i]
      for (i = 1; i <= n; i++) for (j = i + 1; j <= n; j++) if (a[j] < a[i]) { t = a[i]; a[i] = a[j]; a[j] = t }
      return n % 2 ? a[(n + 1) / 2] : (a[n / 2] + a[n / 2 + 1]) / 2 }
    NR == FNR { dist[$1 SUBSEP $2 SUBSEP $3 SUBSEP $4] = 1; next }
    $1 == want && $7 == 0 && !(($1 SUBSEP $2 SUBSEP $3 SUBSEP $4) in dist) {
      cell = $2 SUBSEP $3; st = $5; k = cell SUBSEP st
      if ($5 ~ /^srv/) { st = "srv"; k = cell SUBSEP st; rk = cell SUBSEP $4; if (!(rk in srvsum)) { cnt[k]++; idx[rk] = cnt[k] } srvsum[rk] += $6; v[k, idx[rk]] = srvsum[rk]; cells[cell] = 1; next }
      if (!((cell, $4) in seenrep)) { seenrep[cell, $4] = 1; reps[cell]++ }
      cnt[k]++; v[k, cnt[k]] = $6; cells[cell] = 1
      tk = cell SUBSEP "total"; rt = cell SUBSEP "rep" $4; tot[rt] += $6; trep[rt] = 1 }
    END {
      for (rt in trep) { split(rt, p, SUBSEP); tk = p[1] SUBSEP p[2] SUBSEP "total"; cnt[tk]++; v[tk, cnt[tk]] = tot[rt] }
      for (cell in cells) { split(cell, p, SUBSEP); line = "| " p[1] " | " p[2] " | " reps[cell]
        n = split("unload createdb schema data index trigger total srv", s, " ")
        for (i = 1; i <= n; i++) { k = cell SUBSEP s[i]; line = line " | " (cnt[k] ? sprintf("%.1f", med(k)) : "-") }
        print line " |" } }' $R/disturbed.txt $R/ul.tsv | sort -t'|' -k2,2r -k3,3n
  echo
done
echo "### cp / copydb (seconds; cp methods leave out log archives *_lgar*)"
echo
echo "| input | method | stage | n | median |"
echo "|---|---|---|---|---|"
awk -F'\t' '$6 == 0 { k = $1 "|" $2 "|" $4; c[k]++; v[k, c[k]] = $5 }
  END { for (k in c) { n = c[k]; for (i = 1; i <= n; i++) a[i] = v[k, i]
    for (i = 1; i <= n; i++) for (j = i + 1; j <= n; j++) if (a[j] < a[i]) { t = a[i]; a[i] = a[j]; a[j] = t }
    m = n % 2 ? a[(n + 1) / 2] : (a[n / 2] + a[n / 2 + 1]) / 2; split(k, p, "|")
    printf "| %s | %s | %s | %d | %.3f |\n", p[1], p[2], p[3], n, m } }' $R/cp.tsv | sort
echo
echo "### Runs: start, end, gate waits, overlap (disturbed runs are out of the medians)"
echo
echo "| input | run | start | end | gate retries | stages (s) | overlap |"
echo "|---|---|---|---|---|---|---|"
for d in /home/cubrid/dev/workspace/.git_ignored_dir/scratch/baseline/runs/*/*/; do
  in=$(basename "$(dirname "$d")"); run=$(basename "$d")
  start=$(grep -m1 '^pre:' "$d/meta.txt" | awk '{print $2}' | cut -c12-19)
  end=$(grep -m1 '^post-clean:' "$d/meta.txt" | awk '{print $2}' | cut -c12-19)
  retries=$(grep -c '^gate try=[12]' "$d/meta.txt" || true)
  mode=${run%%-*}; w=${run#*-w}; w=${w%%-*}; rep=${run##*-r}
  stages=$(awk -F'\t' -v i="$in" -v m="$mode" -v w="$w" -v r="$rep" '$1==i && $2==m && $3==w && $4==r && $5 !~ /^srv/ {printf "%s %.1f; ", $5, $6}' $R/ul.tsv)
  note=$(awk -F'\t' -v i="$in" -v m="$mode" -v w="$w" -v r="$rep" '$1==i && $2==m && $3==w && $4==r {print $5}' $R/disturbed.txt)
  echo "| $in | $run | $start | $end | $retries | $stages | ${note:--} |"
done
