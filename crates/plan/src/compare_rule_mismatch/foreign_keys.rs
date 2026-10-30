//! Foreign keys whose children lose their parent under develop's compare: the source matched a child key to a
//! parent key that differs only in trailing spaces (or NULs); develop counts those bytes.
//!
//! Input: one class's foreign key, the source class objects. Output: a [`Finding`] with the count of children no
//! parent key equals under develop, and the query that lists them on the target.
//! Source: R14 § 7 F (workspace#524). Cost as R14 § fastmig 4 sets it: the parent's keys ending in 0x20/0x00 are
//! kept (only they can be the source's match of a child develop orphans); a child ending in such a byte is an
//! orphan unless one of them equals it under develop; a child without one is an orphan only if its key equals
//! one of them once cut and a second parent pass finds no exact parent. A develop-equal pair has equal weights
//! position by position, so a trailing child's equal is a trailing parent. Decision G10 workspace#541 (warn).

use std::collections::HashMap;
use std::io;

use fastmig_catalog::source_classes::SourceClass;
use fastmig_ondisk::VersionLine;
use fastmig_ondisk::class_object::Constraint;
use fastmig_ondisk::value::{Collation, Value};
use fastmig_ondisk::volume_set::VolumeSet;

use super::Finding;
use super::key_rows::{
    OwnerNames, attr_domain, column, cut_trailing, develop_eq, for_each_key, has_trailing, show,
    string, target_class, varchar_compare,
};

/// One table's rows: the class and its partitions' heaps.
pub struct Table<'c> {
    pub class: &'c SourceClass,
    pub heaps: Vec<&'c SourceClass>,
}

/// Per key column: the develop compare when it is a VARCHAR the rule change touches, `None` to compare values.
type Rules = [Option<Collation>];

/// Key groups by [`bucket`].
type Buckets<T> = HashMap<Vec<Vec<u8>>, Vec<T>>;

fn is_trailing(rules: &Rules, k: &[Value]) -> bool {
    rules
        .iter()
        .zip(k)
        .any(|(r, v)| r.is_some() && string(v).is_some_and(has_trailing))
}

/// Groups keys that may be equal under either rule: string columns cut and case folded, the rest as printed.
fn bucket(rules: &Rules, k: &[Value]) -> Vec<Vec<u8>> {
    rules
        .iter()
        .zip(k)
        .map(|(r, v)| match (r, string(v)) {
            (Some(_), Some(s)) => cut_trailing(s).to_ascii_lowercase(),
            _ => format!("{v:?}").into_bytes(),
        })
        .collect()
}

fn develop_equal(rules: &Rules, a: &[Value], b: &[Value]) -> bool {
    rules
        .iter()
        .zip(a.iter().zip(b))
        .all(|(r, (x, y))| match (r, string(x), string(y)) {
            (Some(c), Some(s), Some(t)) => develop_eq(*c, s, t),
            _ => x == y,
        })
}

fn keys(
    vs: &VolumeSet,
    line: VersionLine,
    t: &Table,
    names: &[&[u8]],
    mut f: impl FnMut(&[Value]),
) -> io::Result<()> {
    for h in &t.heaps {
        for_each_key(vs, line, h, names, |k| {
            if !k.iter().any(|v| matches!(v, Value::Null)) {
                f(k)
            }
        })?;
    }
    Ok(())
}

fn attr_names<'c>(c: &'c SourceClass, k: &Constraint) -> Option<Vec<&'c [u8]>> {
    k.keys
        .iter()
        .map(|(id, _)| {
            c.class
                .attributes
                .iter()
                .find(|a| a.id == *id)
                .map(|a| a.name.as_slice())
        })
        .collect()
}

pub fn check(
    vs: &VolumeSet,
    line: VersionLine,
    child: &Table,
    fk: &Constraint,
    parent: &Table,
    parent_key: &Constraint,
    owners: &OwnerNames,
) -> io::Result<Option<Finding>> {
    let (Some(cnames), Some(pnames)) = (
        attr_names(child.class, fk),
        attr_names(parent.class, parent_key),
    ) else {
        return Ok(None);
    };
    if cnames.len() != pnames.len() {
        return Ok(None);
    }
    let rules: Vec<Option<Collation>> = cnames
        .iter()
        .map(|n| attr_domain(&child.class.class, n).and_then(varchar_compare))
        .collect();
    if rules.iter().all(Option::is_none) {
        return Ok(None);
    }
    let mut trailing_parents: Buckets<Vec<Value>> = HashMap::new();
    keys(vs, line, parent, &pnames, |k| {
        if is_trailing(&rules, k) {
            trailing_parents
                .entry(bucket(&rules, k))
                .or_default()
                .push(k.to_vec());
        }
    })?;
    let mut orphans = 0u64;
    let mut sample = None;
    let mut candidates: Buckets<(Vec<Value>, u64)> = HashMap::new();
    keys(vs, line, child, &cnames, |k| {
        let b = bucket(&rules, k);
        let Some(ps) = trailing_parents.get(&b) else {
            if is_trailing(&rules, k) {
                orphans += 1;
                sample.get_or_insert_with(|| show_key(k));
            }
            return;
        };
        if ps.iter().any(|p| develop_equal(&rules, p, k)) {
            return;
        }
        if is_trailing(&rules, k) {
            orphans += 1;
            sample.get_or_insert_with(|| show_key(k));
            return;
        }
        let group = candidates.entry(b).or_default();
        match group.iter_mut().find(|(c, _)| develop_equal(&rules, c, k)) {
            Some((_, n)) => *n += 1,
            None => group.push((k.to_vec(), 1)),
        }
    })?;
    if !candidates.is_empty() {
        keys(vs, line, parent, &pnames, |k| {
            if let Some(group) = candidates.get_mut(&bucket(&rules, k)) {
                group.retain(|(c, _)| !develop_equal(&rules, c, k));
            }
        })?;
        for (k, n) in candidates.values().flatten() {
            orphans += n;
            sample.get_or_insert_with(|| show_key(k));
        }
    }
    if orphans == 0 {
        return Ok(None);
    }
    let ct = target_class(&child.class.class, owners);
    let pt = target_class(&parent.class.class, owners);
    let fk_name = String::from_utf8_lossy(&fk.name);
    let not_null: Vec<String> = cnames
        .iter()
        .map(|n| format!("c.{} IS NOT NULL", column(n)))
        .collect();
    let join: Vec<String> = cnames
        .iter()
        .zip(&pnames)
        .map(|(c, p)| format!("p.{} = c.{}", column(p), column(c)))
        .collect();
    Ok(Some(Finding {
        warning: format!(
            "compare rule mismatch: foreign key {fk_name} of {ct}: {orphans} rows (e.g. {}) match a {pt} key only \
             with trailing spaces and NULs ignored; develop finds no parent for them (cascades and checks skip them); \
             fix: fastmig/post-migration-fixes.sql (workspace#541)",
            sample.unwrap_or_default()
        ),
        fix_sql: format!(
            "-- {ct} foreign key {fk_name}: {orphans} rows have no {pt} parent under develop's compare; set each \
             to its parent's exact value or delete it\nSELECT c.* FROM {ct} c WHERE {} AND NOT EXISTS (SELECT 1 FROM \
             {pt} p WHERE {});\n",
            not_null.join(" AND "),
            join.join(" AND ")
        ),
    }))
}

fn show_key(k: &[Value]) -> String {
    let parts: Vec<String> = k
        .iter()
        .map(|v| string(v).map_or_else(|| format!("{v:?}"), show))
        .collect();
    parts.join(", ")
}
