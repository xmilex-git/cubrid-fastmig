# The official side's copy of a 10.2 unload schema: the change_owner call of each partitioned table moves to right
# after the statement that partitions it, every other line stays where it is (workspace#589).
#
# Input: awk -f official_102_schema.awk <schema> <schema>   (the same file twice: pass 1 finds the partitioned tables)
# Output: the schema on stdout.
# Why: develop/11.4 loaddb --no-user-specified-name fails on "ALTER CLASS [t] PARTITION BY" once t belongs to a
#   non-DBA user ("Unknown class dba.t": the server's lock-hint lookup has no unqualified-name fallback, workspace#516).
#   Moving every change_owner to the end instead lets the views resolve their tables as dba.<t> (workspace#589, T40).
# pass 1 (NR == FNR): the classes a "PARTITION BY" follows; pass 2: their change_owner calls held back and printed
# right after the statement that partitions them
NR == FNR { if (/^PARTITION BY/ && prev ~ /^ALTER CLASS \[/) { c = prev; sub(/^ALTER CLASS \[/, "", c); sub(/\].*/, "", c); part[c] = 1 }
            prev = $0; next }
/^call \[change_owner\]\('/ { c = $0; sub(/^call \[change_owner\]\('/, "", c); sub(/'.*/, "", c)
                              if (c in part) { held[c] = $0; next } }
/^ALTER CLASS \[/ { cur = $0; sub(/^ALTER CLASS \[/, "", cur); sub(/\].*/, "", cur) }
{ print }
/^PARTITION BY/ { inpart = cur }
inpart != "" && /;[[:space:]]*$/ { if (inpart in held) print held[inpart]; inpart = "" }
