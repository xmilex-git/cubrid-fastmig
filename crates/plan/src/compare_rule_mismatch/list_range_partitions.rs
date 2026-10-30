//! LIST and RANGE partitions whose rows develop would place elsewhere: a VARCHAR key whose value, or a bound, the
//! source took as equal to another once trailing spaces (and NULs) were ignored, while develop counts them.
//!
//! Input: one LIST or RANGE [`Partitioning`] of a source compared under the old rule (10.2, or a source run with
//! `ignore_trailing_space=yes`). Output: a [`Finding`] when such rows exist, or when the key is an expression.
//! Source: R14 § 7 (workspace#524: LIST — a row equal to no value of its partition under develop; RANGE — only a
//! row equal to a bound once cut and shorter than it, so a source with no bound ending in 0x20/0x00 is passed from
//! the catalog alone; on a develop target a misplaced RANGE row is missed by pruning, a misplaced LIST row is still
//! found but cannot be re-inserted, workspace#548 probe); decision G10 workspace#541 (warn; the fix reorganizes
//! every partition on the target in one statement, which re-inserts their rows under develop's compare).

use std::cmp::Ordering;
use std::io;

use fastmig_ondisk::VersionLine;
use fastmig_ondisk::value::{Collation, Value};
use fastmig_ondisk::volume_set::VolumeSet;

use super::Finding;
use super::key_rows::{
    OwnerNames, attr_domain, column, develop_eq, equal_in_10_2, for_each_key, has_trailing,
    literal, show, string, target_class, varchar_compare,
};
use super::partitioning::{PartitionType, Partitioning};

fn bound_sql(v: &Value) -> String {
    string(v).map_or_else(|| "NULL".into(), literal)
}

/// Placed by the old rule, misplaced under develop's: equal to a bound once cut, on the wrong side of it.
fn outside_range(c: Collation, v: &[u8], min: Option<&[u8]>, max: Option<&[u8]>) -> bool {
    let below_min =
        min.is_some_and(|m| equal_in_10_2(c, v, m) && c.compare(v, m, false) == Ordering::Less);
    let not_below_max =
        max.is_some_and(|m| equal_in_10_2(c, v, m) && c.compare(v, m, false) != Ordering::Less);
    below_min || not_below_max
}

fn range_fix(c: Collation, p: &Partitioning, table: &str) -> String {
    let mut parts: Vec<_> = p.parts.iter().collect();
    // ascending maximum, MAXVALUE (NULL) last: the order REORGANIZE ... INTO needs
    parts.sort_by(|a, b| {
        match (
            a.bounds.get(1).and_then(string),
            b.bounds.get(1).and_then(string),
        ) {
            (Some(x), Some(y)) => c.compare(x, y, false),
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (None, None) => Ordering::Equal,
        }
    });
    let names: Vec<String> = parts.iter().map(|q| column(&q.name)).collect();
    let defs: Vec<String> = parts
        .iter()
        .map(|q| match q.bounds.get(1).and_then(string) {
            Some(m) => format!(
                "PARTITION {} VALUES LESS THAN ({})",
                column(&q.name),
                literal(m)
            ),
            None => format!("PARTITION {} VALUES LESS THAN MAXVALUE", column(&q.name)),
        })
        .collect();
    format!(
        "ALTER TABLE {table} REORGANIZE PARTITION {} INTO ({});\n",
        names.join(", "),
        defs.join(", ")
    )
}

pub fn check(
    vs: &VolumeSet,
    line: VersionLine,
    p: &Partitioning,
    owners: &OwnerNames,
) -> io::Result<Option<Finding>> {
    if p.ty == PartitionType::Hash {
        return Ok(None);
    }
    let Some(c) = attr_domain(&p.root.class, &p.key_column).and_then(varchar_compare) else {
        return Ok(None);
    };
    let table = target_class(&p.root.class, owners);
    let kind = if p.ty == PartitionType::List {
        "LIST"
    } else {
        "RANGE"
    };
    let bounds = || p.parts.iter().flat_map(|q| &q.bounds).filter_map(string);
    // a RANGE row moves only across a bound ending in 0x20/0x00 (R14 § 7), whatever the key is
    if p.ty == PartitionType::Range && !bounds().any(has_trailing) {
        return Ok(None);
    }
    if !p.key_is_column {
        // the bounds have the expression's result type: non-string bounds compare as numbers, dates, ...
        if bounds().next().is_none() {
            return Ok(None);
        }
        return Ok(Some(Finding {
            warning: format!(
                "compare rule mismatch: {kind} partitioned {table} has the string expression key {} over a VARCHAR \
                 column; rows placed under the old trailing-space rule are not checked (workspace#541)",
                p.expression
            ),
            fix_sql: format!(
                "-- {table}: {kind} expression key not checked; compare each partition's rows with its bounds\n"
            ),
        }));
    }
    let mut moved = 0u64;
    let mut sample = None;
    // LIST: each partition's values, plus the values of its rows no value equals under develop
    let mut lists = Vec::new();
    for q in &p.parts {
        let mut extra: Vec<Vec<u8>> = Vec::new();
        for_each_key(vs, line, q.class, &[&p.key_column], |k| {
            let Some(v) = string(&k[0]) else { return };
            let misplaced = match p.ty {
                PartitionType::List => !q
                    .bounds
                    .iter()
                    .filter_map(string)
                    .any(|b| develop_eq(c, v, b)),
                _ => outside_range(
                    c,
                    v,
                    q.bounds.first().and_then(string),
                    q.bounds.get(1).and_then(string),
                ),
            };
            if misplaced {
                moved += 1;
                sample.get_or_insert_with(|| show(v));
                if !extra.iter().any(|e| develop_eq(c, e, v)) {
                    extra.push(v.to_vec());
                }
            }
        })?;
        let values: Vec<String> = q
            .bounds
            .iter()
            .map(bound_sql)
            .chain(extra.iter().map(|e| literal(e)))
            .collect();
        lists.push(format!(
            "PARTITION {} VALUES IN ({})",
            column(&q.name),
            values.join(", ")
        ));
    }
    if moved == 0 {
        return Ok(None);
    }
    // one statement over every partition: the engine re-checks the rows of the partitions it does not reorganize
    // too, so a partition-by-partition fix fails while another still holds a misplaced row (workspace#548)
    let (fix, effect) = match p.ty {
        PartitionType::Range => (
            range_fix(c, p, &table),
            format!("`WHERE {} = ...` pruning misses them", String::from_utf8_lossy(&p.key_column)),
        ),
        _ => (
            format!(
                "ALTER TABLE {table} REORGANIZE PARTITION {} INTO ({});\n",
                p.parts.iter().map(|q| column(&q.name)).collect::<Vec<_>>().join(", "),
                lists.join(", ")
            ),
            "no develop LIST value equals theirs, so a statement that re-inserts them (REORGANIZE PARTITION) fails \
             with \"Appropriate partition does not exist\""
                .to_string(),
        ),
    };
    Ok(Some(Finding {
        warning: format!(
            "compare rule mismatch: {kind} partitioned {table}: {moved} rows (e.g. {}) sit in a partition develop's \
             compare (trailing spaces count) does not choose for them; {effect}; \
             fix: fastmig/post-migration-fixes.sql (workspace#541)",
            sample.unwrap_or_default(),
        ),
        fix_sql: format!(
            "-- {table}: {moved} rows placed under the old trailing-space rule; reorganizing re-inserts them under \
             develop's compare{}\n{fix}",
            if p.ty == PartitionType::List {
                " (each LIST gets the values its rows hold)"
            } else {
                ""
            }
        ),
    }))
}
