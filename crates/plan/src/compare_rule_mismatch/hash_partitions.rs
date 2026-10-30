//! HASH partitions of a 10.2 source whose rows develop pruning looks for in another partition: a CHAR/VARCHAR key
//! of a collation whose hash weighs the space (1, 3, 4, 5, 6, 7) with a space left once the trailing ones are cut.
//!
//! Input: one HASH [`Partitioning`] of a 10.2 source. Output: a [`Finding`] when rows sit apart from where develop
//! hashes them, or when the key is an expression this tool does not evaluate.
//! Source: R14 § 6 (workspace#524); decision G10 workspace#541 (warn, do not refuse; the fix re-partitions on the
//! target, which re-inserts every row under develop's hash).

use std::io;

use fastmig_ondisk::VersionLine;
use fastmig_ondisk::value::DbType;
use fastmig_ondisk::value::partition_hash::{hash_partition, space_weight_changes_hash};
use fastmig_ondisk::volume_set::VolumeSet;

use super::Finding;
use super::key_rows::{OwnerNames, attr_domain, for_each_key, show, string, target_class};
use super::partitioning::{PartitionType, Partitioning};

fn fix_sql(p: &Partitioning, table: &str) -> String {
    format!(
        "ALTER TABLE {table} REMOVE PARTITIONING;\nALTER TABLE {table} PARTITION BY HASH ({}) PARTITIONS {};\n",
        p.expression, p.hash_count
    )
}

pub fn check(vs: &VolumeSet, p: &Partitioning, owners: &OwnerNames) -> io::Result<Option<Finding>> {
    if p.ty != PartitionType::Hash {
        return Ok(None);
    }
    let Some(d) = attr_domain(&p.root.class, &p.key_column) else {
        return Ok(None);
    };
    let string_key = d.type_id == DbType::Varchar as i32 || d.type_id == DbType::Char as i32;
    if !string_key || !space_weight_changes_hash(d.collation_id) {
        return Ok(None);
    }
    let table = target_class(&p.root.class, owners);
    if !p.key_is_column {
        return Ok(Some(Finding {
            warning: format!(
                "compare rule mismatch: HASH partitioned {table} has the expression key {} over a column of \
                 collation id {}; 10.2 hashed a space inside the result as 0, develop as 32, so pruning may miss \
                 rows; not checked row by row; fix: fastmig/post-migration-fixes.sql (workspace#541)",
                p.expression, d.collation_id
            ),
            fix_sql: format!(
                "-- {table}: expression key not checked; re-partitioning re-inserts every row under develop's hash\n{}",
                fix_sql(p, &table)
            ),
        }));
    }
    let (collation, count) = (d.collation_id, p.hash_count);
    let mut moved = 0u64;
    let mut sample = None;
    for part in &p.parts {
        for_each_key(vs, VersionLine::V10_2, part.class, &[&p.key_column], |k| {
            let Some(s) = string(&k[0]) else { return };
            let cut = s
                .iter()
                .rposition(|&b| b != b' ')
                .map_or(&s[..0], |i| &s[..=i]);
            if !cut.contains(&b' ') {
                return;
            }
            let old = hash_partition(VersionLine::V10_2, collation, s, count);
            let new = hash_partition(VersionLine::Develop, collation, s, count);
            if old != new {
                moved += 1;
                sample.get_or_insert_with(|| show(s));
            }
        })?;
    }
    if moved == 0 {
        return Ok(None);
    }
    Ok(Some(Finding {
        warning: format!(
            "compare rule mismatch: HASH partitioned {table}: {moved} rows (e.g. {}) sit in the partition the 10.2 \
             hash chose, develop hashes them to another, so `WHERE {} = ...` pruning misses them; \
             fix: fastmig/post-migration-fixes.sql (workspace#541)",
            sample.unwrap_or_default(),
            String::from_utf8_lossy(&p.key_column)
        ),
        fix_sql: format!(
            "-- {table}: {moved} rows hashed by 10.2 rules; re-partitioning re-inserts every row under develop's hash\n{}",
            fix_sql(p, &table)
        ),
    }))
}
