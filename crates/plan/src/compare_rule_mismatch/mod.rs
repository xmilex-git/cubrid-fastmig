//! The compare rule mismatch pre-check: rows a source placed or matched under its older string rules that
//! develop's rules place or match differently — HASH partitions (10.2), VARCHAR foreign keys and LIST/RANGE
//! partitions (10.2, or a source run with `ignore_trailing_space=yes`). Each is a warning, and the SQL that fixes
//! it on the target goes to `<dest>/fastmig/post-migration-fixes.sql`.
//!
//! Input: the source volume set and line, its class objects and owner names, and which rules it was written
//! under. Output: [`CompareRuleMismatch`].
//! Source: R14 workspace#524 (what differs and the cheapest scan for each); decision G10 workspace#541 (warn and
//! go on, write the fixes SQL; refusing is kept only for a target run with `ignore_trailing_space=yes`; ADR 0016).

mod foreign_keys;
mod hash_partitions;
mod key_rows;
mod list_range_partitions;
mod partitioning;

use std::io;

use fastmig_catalog::source_classes::SourceClass;
use fastmig_ondisk::VersionLine;
use fastmig_ondisk::class_object::{ConstraintKind, class_properties};
use fastmig_ondisk::volume_set::VolumeSet;

pub use key_rows::OwnerNames;

use foreign_keys::Table;
use partitioning::partitionings;

/// One mismatch: the plan's warning and its part of the fixes SQL.
pub struct Finding {
    pub warning: String,
    pub fix_sql: String,
}

/// Which old rules the source was written under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OldRules {
    /// 10.2: the space weighs 0 in the hash (HASH partitions).
    pub hash_space_weight_0: bool,
    /// 10.2, or `ignore_trailing_space=yes`: VARCHAR equality ignores trailing spaces (foreign keys, LIST/RANGE).
    pub trailing_space_ignored: bool,
}

#[derive(Default)]
pub struct CompareRuleMismatch {
    pub warnings: Vec<String>,
    /// Its section of `<dest>/fastmig/post-migration-fixes.sql`; empty when nothing mismatches.
    pub post_migration_fixes_sql: String,
}

const SQL_HEAD: &str =
    "-- Rows the source placed or matched under its older string rules (workspace#541).\n";

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

fn is_partition(c: &SourceClass) -> bool {
    c.class.partitions.iter().any(|p| p.pname.is_some())
}

/// A table and its partitions' heaps.
fn table<'c>(c: &'c SourceClass, classes: &'c [SourceClass]) -> Table<'c> {
    let heaps = std::iter::once(c)
        .chain(
            classes
                .iter()
                .filter(|s| c.class.sub_classes.contains(&s.oid) && is_partition(s)),
        )
        .collect();
    Table { class: c, heaps }
}

pub fn check(
    vs: &VolumeSet,
    line: VersionLine,
    classes: &[SourceClass],
    owners: &OwnerNames,
    rules: OldRules,
) -> io::Result<CompareRuleMismatch> {
    let mut found = Vec::new();
    let parts =
        partitionings(classes, line).map_err(|e| invalid(format!("partition info: {e}")))?;
    for p in &parts {
        if rules.hash_space_weight_0 {
            found.extend(hash_partitions::check(vs, p, owners)?);
        }
        if rules.trailing_space_ignored {
            found.extend(list_range_partitions::check(vs, line, p, owners)?);
        }
    }
    if rules.trailing_space_ignored {
        for c in classes
            .iter()
            .filter(|c| !c.class.is_system_class() && !is_partition(c))
        {
            let props = class_properties(&c.class.properties, line).map_err(|e| {
                invalid(format!(
                    "{} properties: {e}",
                    String::from_utf8_lossy(&c.class.name)
                ))
            })?;
            for fk in props
                .constraints
                .iter()
                .filter(|k| k.kind == ConstraintKind::ForeignKey)
            {
                let Some(info) = &fk.foreign_key else {
                    continue;
                };
                let Some(parent) = classes.iter().find(|p| p.oid == info.ref_class) else {
                    continue;
                };
                let pprops = class_properties(&parent.class.properties, line).map_err(|e| {
                    invalid(format!(
                        "{} properties: {e}",
                        String::from_utf8_lossy(&parent.class.name)
                    ))
                })?;
                let Some(pk) = pprops.constraints.iter().find(|k| {
                    matches!(k.kind, ConstraintKind::PrimaryKey | ConstraintKind::Unique)
                        && k.btid.is_some()
                        && k.btid == info.ref_pk_btid
                }) else {
                    continue;
                };
                found.extend(foreign_keys::check(
                    vs,
                    line,
                    &table(c, classes),
                    fk,
                    &table(parent, classes),
                    pk,
                    owners,
                )?);
            }
        }
    }
    let mut out = CompareRuleMismatch::default();
    if !found.is_empty() {
        out.post_migration_fixes_sql.push_str(SQL_HEAD);
    }
    for f in found {
        out.warnings.push(f.warning);
        out.post_migration_fixes_sql.push('\n');
        out.post_migration_fixes_sql.push_str(&f.fix_sql);
    }
    Ok(out)
}
