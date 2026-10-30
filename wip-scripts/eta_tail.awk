# workspace#560: the running ETA in the last 30% of a run against the time actually left.
# awk -v t_start=<epoch s> -f eta_tail.awk runs/<tag>/err.txt
# err.txt lines: "<epoch s.ns> [phase i/n name] ... ETA phase X total ~Y done ~HH:MM ..."; the run ends at the
# "[migrate] finished" line. Prints per tail line: elapsed, ETA shown, time left, ETA / left.
function secs(s,   n, a) { n = split(s, a, ":"); return n == 3 ? a[1]*3600 + a[2]*60 + a[3] : a[1]*60 + a[2] }
{
  t = $1 + 0
  if ($0 ~ /\[migrate\] finished/) { t_end = t; next }
  if (match($0, /total ~[0-9:.]+/)) { n++; T[n] = t; E[n] = secs(substr($0, RSTART + 7, RLENGTH - 7)) }
}
END {
  total = t_end - t_start
  for (i = 1; i <= n; i++) {
    el = T[i] - t_start; left = t_end - T[i]
    if (el < 0.7 * total || left <= 0) continue
    printf "el=%.1f eta=%.1f left=%.1f ratio=%.2f\n", el, E[i], left, E[i] / left
  }
  printf "total=%.1f\n", total
}
