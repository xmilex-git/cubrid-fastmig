#!/usr/bin/env bash
# Median-of-reps table of results/same.tsv (or $1): one row per method and stage, "method stage n median min max rc".
# A rep whose total rc is not 0 is left out of every stage of that method and counted in "failed"; reps listed in
# $2 ("method<TAB>rep" lines, e.g. results/same.disturbed: overlapped by another job) are left out too.
set -uo pipefail
f=${1:-/home/cubrid/dev/workspace/.git_ignored_dir/scratch/484/results/same.tsv}
x=${2:-/dev/null}
awk -F'\t' -v xf="$x" '
  FILENAME == xf { bad[$1 "\t" $2] = 1; next }
  $3 == "total" && $5 != 0 { bad[$1 "\t" $2] = 1 }
  { rows[++nr] = $0 }
  END {
    for (i = 1; i <= nr; i++) {
      split(rows[i], r, "\t"); if ((r[1] "\t" r[2]) in bad) { failed[r[1] " r" r[2]] = 1; continue }
      k = r[1] "\t" r[3]; if (!(k in n)) order[++m] = k; v[k, ++n[k]] = r[4]
    }
    printf "method\tstage\tn\tmedian_s\tmin_s\tmax_s\n"
    for (j = 1; j <= m; j++) {
      k = order[j]; c = n[k]
      for (a = 1; a <= c; a++) s[a] = v[k, a]
      for (a = 1; a <= c; a++) for (b = a + 1; b <= c; b++) if (s[b] + 0 < s[a] + 0) { t = s[a]; s[a] = s[b]; s[b] = t }
      med = (c % 2) ? s[(c + 1) / 2] : (s[c / 2] + s[c / 2 + 1]) / 2
      printf "%s\t%d\t%.3f\t%.3f\t%.3f\n", k, c, med, s[1], s[c]
    }
    for (x in failed) printf "failed reps excluded: %s\n", x
  }' "$x" "$f"
