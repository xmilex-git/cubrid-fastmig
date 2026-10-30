//! Attributes of a user class or view whose domain is a same-slot system class (`db_user` → `_db_user`, `db_serial`
//! → `_db_serial`, ...) and whose owner is outside the DBA group. The class keeps the domain and every value (same
//! class and row OIDs, ADR 0002), but develop lets only DBA read those tables, so the owner's path expressions over
//! the attribute (`u.name`) fail on the target. Domains on a system class whose rows move are refused by `classify`.
//!
//! Input: the source classes and the source `db_user` rows. Output: one warning per such attribute.
//! Source: workspace#554 case 1, measured on 10.2.18, 11.2.9, 11.3.6 → develop 682b080b2: DBA's path expressions and
//! views work, U1's fail with "SELECT is not authorized on _db_user"; the official path fails earlier (10.2 → develop
//! `loaddb` "The domain of u cannot be a vclass", 11.2/11.3 `unloaddb` crashes in `emit_domain_def`). develop grants
//! on the `_db_*` tables: R11 workspace#519 § 3 (DBA and INFORMATION_SCHEMA only; `db_root` keeps PUBLIC SELECT).

use std::collections::HashMap;

use fastmig_ondisk::class_object::{ClassType, SchemaDomain};
use fastmig_ondisk::{Oid, VersionLine};

use crate::class_rows::Row;
use crate::source_classes::SourceClass;
use crate::stored_texts::dba_group;
use crate::system_classes::{SystemClassRole, source_role};
use crate::system_table::SystemTable;

fn names_class(d: &SchemaDomain, oid: Oid) -> bool {
    d.class_oid == oid || d.set_domain.iter().any(|s| names_class(s, oid))
}

/// The warnings (see the module header) for a target of line `to`; none for an 11.4 target, which keeps the 11.4 names
/// and their PUBLIC SELECT (R4 summary G: develop drops those grants).
pub fn read_denied_domains(classes: &[SourceClass], users: &[Row], to: VersionLine) -> Vec<String> {
    if to != VersionLine::Develop {
        return Vec::new();
    }
    let dba = dba_group(users);
    let tables: HashMap<Oid, SystemTable> = classes
        .iter()
        .filter(|c| c.class.is_system_class())
        .filter_map(|c| {
            match source_role(
                &String::from_utf8_lossy(&c.class.name),
                c.class.class_type == ClassType::Vclass,
                to,
            ) {
                Some(SystemClassRole::SameSlot { table }) if table != SystemTable::DbRoot => {
                    Some((c.oid, table))
                }
                _ => None,
            }
        })
        .collect();
    let mut out = Vec::new();
    for c in classes
        .iter()
        .filter(|c| !c.class.is_system_class() && !dba.contains(&c.class.owner))
    {
        let k = &c.class;
        for a in k
            .attributes
            .iter()
            .chain(&k.shared_attributes)
            .chain(&k.class_attributes)
        {
            for (oid, t) in &tables {
                if a.domain.iter().any(|d| names_class(d, *oid)) {
                    out.push(format!(
                        "{}.{} has a domain on system class {}, which develop names {} and lets only DBA read; \
                         its owner is not in the DBA group, so its path expressions over the attribute fail on the \
                         target; fix: GRANT SELECT ON {} to the owner on the target if it must read them \
                         (workspace#554)",
                        String::from_utf8_lossy(&k.name),
                        String::from_utf8_lossy(&a.name),
                        t.source_name().unwrap_or_default(),
                        t.name(),
                        t.name()
                    ));
                }
            }
        }
    }
    out
}
