//! The set-comparison rewrites only the official-path comparison needs: the official path's target and the migrated
//! target, both unloaded by one target install (`set-comparison --rules official`). Each one masks a row of
//! docs/intended-differences.md and nothing else, and returns how many entries it changed.
//!
//! Input: the entries of one unload file (one per line outside `_objects`, in file order). Output: the rewritten entries.
//! Source: workspace#600 (official-unload-comparison), the rows measured there on the T37 pairs.

/// A started serial with a cache on an 11.4 target: loaddb on the official path runs the unload's
/// `SELECT [s].NEXT_VALUE`, which reserves a cache block and stores its last value, so the official side prints
/// `START WITH` (CACHE − 1) × INCREMENT ahead of the migrated side (s_invoice 2590 against 2545, CACHE 10, INCREMENT 5;
/// row "position of a started serial with a cache"). The official side's value is moved back by that much; a serial
/// with no `NEXT_VALUE` after it, or without a cache, is left alone.
/// shortcut: a block cut short by MAXVALUE stays a difference (none in the baseline DBs); read the cycle rule then.
pub fn serial_cache_position(entries: &mut [Vec<u8>]) -> usize {
    let mut changed = 0;
    for i in 0..entries.len() {
        let Some(name) = entries[i].strip_prefix(b"CREATE SERIAL ") else {
            continue;
        };
        let next = [b"SELECT ", name, b".NEXT_VALUE;"].concat();
        let Some(end) = entries[i + 1..]
            .iter()
            .position(|e| e == b";")
            .map(|n| i + 1 + n)
        else {
            continue;
        };
        if entries.get(end + 1) != Some(&next) {
            continue;
        }
        let clause = |prefix: &[u8]| {
            (i + 1..end).find_map(|j| {
                let v = entries[j].strip_prefix(prefix)?;
                Some((j, String::from_utf8_lossy(v).trim().parse::<i128>().ok()?))
            })
        };
        let (Some((at, start)), Some((_, step)), Some((_, cache))) = (
            clause(b"\t START WITH "),
            clause(b"\t INCREMENT BY "),
            clause(b"\t CACHE "),
        ) else {
            continue;
        };
        if cache > 1 {
            entries[at] = format!("\t START WITH {}", start - (cache - 1) * step).into_bytes();
            changed += 1;
        }
    }
    changed
}
