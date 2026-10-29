//! Step 1 of regeneration: one decision per source class — its role, whether its records are re-encoded, and
//! what refuses or warns the migration.
//!
//! Input: the source (11.4) class objects. Output: a [`ClassPlan`] per class.
//! Source: decisions workspace#482 1 (system class roles; a user domain on a reallocated system class refuses),
//! 4 (re-encoded classes keep their repid), 5 (NCHAR(n) → CHAR(n), VARNCHAR(n) → VARCHAR(n), n > 2048 refuses);
//! R3 § 5 rule A1 (a class is re-encoded when any representation has a type whose encoding changed: CHAR,
//! NUMERIC, and NCHAR by decision 5); workspace#481 (pre-check refusals and warnings).

use fastmig_ondisk::Oid;
use fastmig_ondisk::class_object::{ClassType, SchemaDomain};
use fastmig_ondisk::value::DbType;

use crate::source_classes::SourceClass;
use crate::system_classes::{SystemClassRole, source_role};

/// Engine `SM_CLASSFLAG_SYSTEM`.
const CLASSFLAG_SYSTEM: i32 = 1;
/// Engine `DB_MAX_CHAR_PRECISION`, the same in 11.4 and develop.
const MAX_CHAR_PRECISION: i32 = 2048;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClassKind {
    User,
    System(SystemClassRole),
}

#[derive(Clone, Debug)]
pub struct ClassPlan {
    pub oid: Oid,
    pub name: String,
    pub kind: ClassKind,
    /// Every record is rewritten in the current representation (R3 rule C).
    pub reencode: bool,
    /// Reasons the migration must refuse.
    pub refusals: Vec<String>,
    /// Notices for the user.
    pub warnings: Vec<String>,
}

fn walk<'a>(d: &'a SchemaDomain, out: &mut Vec<&'a SchemaDomain>) {
    out.push(d);
    for s in &d.set_domain {
        walk(s, out);
    }
}

/// Every domain node of the class: current attributes (all three kinds) and every representation.
fn all_domains(c: &SourceClass) -> Vec<(i32, &SchemaDomain)> {
    let k = &c.class;
    let mut out = Vec::new();
    let atts = k
        .attributes
        .iter()
        .chain(&k.shared_attributes)
        .chain(&k.class_attributes);
    for a in atts {
        let mut v = Vec::new();
        a.domain.iter().for_each(|d| walk(d, &mut v));
        out.extend(v.into_iter().map(|d| (a.id, d)));
    }
    for r in &k.representations {
        for a in &r.attributes {
            let mut v = Vec::new();
            a.domain.iter().for_each(|d| walk(d, &mut v));
            out.extend(v.into_iter().map(|d| (a.id, d)));
        }
    }
    out
}

/// Classifies the source classes.
pub fn classify(classes: &[SourceClass]) -> Vec<ClassPlan> {
    let role_of = |oid: Oid| {
        classes.iter().find(|c| c.oid == oid).and_then(|c| {
            (c.class.flags & CLASSFLAG_SYSTEM != 0).then(|| {
                source_role(
                    &String::from_utf8_lossy(&c.class.name),
                    c.class.class_type == ClassType::Vclass,
                )
            })
        })
    };
    classes
        .iter()
        .map(|c| {
            let name = String::from_utf8_lossy(&c.class.name).into_owned();
            let mut refusals = Vec::new();
            let mut warnings = Vec::new();
            let kind = if c.class.flags & CLASSFLAG_SYSTEM != 0 {
                match source_role(&name, c.class.class_type == ClassType::Vclass) {
                    Some(r) => ClassKind::System(r),
                    None => {
                        refusals.push(format!("system class {name} is not in the 11.4 catalog this tool knows"));
                        ClassKind::User
                    }
                }
            } else {
                ClassKind::User
            };
            let mut reencode = false;
            for (att, d) in all_domains(c) {
                match d.type_id {
                    t if t == DbType::Char as i32 || t == DbType::Numeric as i32 => reencode = true,
                    t if t == DbType::Nchar as i32 => {
                        reencode = true;
                        if d.precision > MAX_CHAR_PRECISION {
                            refusals.push(format!(
                                "{name}: attribute id {att} is NCHAR({}), above CHAR's {MAX_CHAR_PRECISION}",
                                d.precision
                            ));
                        }
                    }
                    _ => {}
                }
                if kind == ClassKind::User
                    && !d.class_oid.is_null()
                    && let Some(Some(r)) = role_of(d.class_oid)
                    && !matches!(r, SystemClassRole::SameSlot { .. })
                {
                    refusals.push(format!(
                        "{name}: attribute id {att} has a domain on system class {:?} ({r:?}), whose rows get new OIDs",
                        d.class_oid
                    ));
                }
            }
            let nchar = all_domains(c)
                .iter()
                .any(|(_, d)| d.type_id == DbType::Nchar as i32 || d.type_id == DbType::Varnchar as i32);
            if nchar && kind == ClassKind::User {
                warnings.push(format!(
                    "{name}: NCHAR/VARNCHAR attributes become CHAR/VARCHAR (develop has no national character types)"
                ));
            }
            // a view stores no records; its domains are still rewritten with the class object
            reencode &= c.class.class_type != ClassType::Vclass;
            refusals.dedup();
            ClassPlan {
                oid: c.oid,
                name,
                kind,
                reencode,
                refusals,
                warnings,
            }
        })
        .collect()
}
