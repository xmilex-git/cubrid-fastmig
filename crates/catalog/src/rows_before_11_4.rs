//! Brings the same-slot rows of a 10.2, 11.2 or 11.3 source to the 11.4 shape the rest of regeneration reads: grants gain
//! their object type and the grantor 11.4 would record, stored procedures gain their unique name and split target,
//! arguments point to their procedure, the serial's `att_name` becomes `attr_name`.
//!
//! Input: the source version, the same-slot rows by develop table name (as the source holds them), the source
//! classes. Output: the rows rewritten in place; an 11.4 or develop source is left alone.
//! Source: R12 workspace#521 § 5.1 (research/fastmig-r12-same-slot-mapping bacaa22), mapping order db_user →
//! procedures → arguments → grants → serial:
//! - grants {class, grantor, cache} → {0 (`DB_OBJECT_CLASS`), class, grantor, cache}; a grantor in the DBA group,
//!   in the class owner's group or deleted becomes the owner (11.4 `au_compare_grantor_and_return`,
//!   authenticate_grant.cpp:1944@release/11.4; `get_grants` filter); entries of one (class, grantor) merge their
//!   cache by OR. A third user who granted with grant option stays the grantor (map supervisor decision (b): the
//!   engine's unload → load path makes it the owner; this is a reported intended difference).
//! - `_db_stored_procedure`: `unique_name` = lower(owner name) + "." + `sp_name` (`change_sp_owner`), `target` split
//!   by [`split_target_signature`], `pkg_name` NULL, `is_system_generated` 0, `directive` 0.
//! - `_db_stored_procedure_args`: `sp_of` = the procedure whose `args` holds the row, `sp_name` dropped,
//!   `is_system_generated` 0, `default_value` NULL, `is_optional` 0.
//! - `db_serial.att_name` → `attr_name` (b85c07b27 #5346).
//!
//! 10.2 rows have the same old shape (T15 workspace#538 measured 10.2 → develop load against this mapping; R10
//! workspace#511 the SP `unique_name` rule; workspace#544).

use std::collections::HashMap;

use fastmig_ondisk::value::{DecodeError, DecodeResult, SetElement, Value};
use fastmig_ondisk::{Oid, SourceVersion};

use crate::class_rows::Row;
use crate::source_classes::SourceClass;
use crate::sp_target_signature::split_target_signature;
use crate::system_rows::{authorization, object, stored_procedure, user};
use crate::system_table::SystemTable;

/// `DB_OBJECT_CLASS`.
const OBJECT_CLASS: i32 = 0;
/// Elements of one grant entry before 11.4 (`GRANT_ENTRY_LENGTH` 3, authenticate.c:131@release/11.3).
const OLD_GRANT_ENTRY_LENGTH: usize = 3;

fn err(what: String) -> DecodeError {
    DecodeError { at: 0, what }
}

/// The source users: name and every group (`groups` holds the closure, `au_add_member`).
struct Users {
    dba: Option<Oid>,
    by_oid: HashMap<Oid, (Vec<u8>, Vec<Oid>)>,
}

impl Users {
    fn of(rows: &[Row]) -> Self {
        let by_oid: HashMap<Oid, (Vec<u8>, Vec<Oid>)> = rows
            .iter()
            .map(|r| {
                let groups = match user::groups(&r.values) {
                    Some(Value::Set(s)) => s
                        .elements
                        .iter()
                        .filter_map(|e| object(Some(&e.value)))
                        .collect(),
                    _ => Vec::new(),
                };
                (
                    r.oid,
                    (user::name(&r.values).unwrap_or_default().to_vec(), groups),
                )
            })
            .collect();
        let dba = by_oid
            .iter()
            .find(|(_, (n, _))| n == b"DBA")
            .map(|(o, _)| *o);
        Users { dba, by_oid }
    }

    /// Engine `au_is_user_group_member (group, user)`.
    fn is_member(&self, group: Oid, user: Oid) -> bool {
        user == group
            || self
                .by_oid
                .get(&user)
                .is_some_and(|(_, g)| g.contains(&group))
    }
}

pub fn to_11_4_shape(
    version: SourceVersion,
    rows: &mut HashMap<SystemTable, Vec<Row>>,
    classes: &[SourceClass],
) -> DecodeResult<()> {
    if !matches!(
        version,
        SourceVersion::V10_2 | SourceVersion::V11_0 | SourceVersion::V11_2 | SourceVersion::V11_3
    ) {
        return Ok(());
    }
    let users = Users::of(
        rows.get(&SystemTable::DbUser)
            .map(Vec::as_slice)
            .unwrap_or_default(),
    );
    let owner_of: HashMap<Oid, Oid> = classes.iter().map(|c| (c.oid, c.class.owner)).collect();
    let mut sp_of: HashMap<Oid, Oid> = HashMap::new();
    for r in rows
        .get_mut(&SystemTable::DbStoredProcedure)
        .into_iter()
        .flatten()
    {
        procedure(r, &users, &mut sp_of)?;
    }
    for r in rows
        .get_mut(&SystemTable::DbStoredProcedureArgs)
        .into_iter()
        .flatten()
    {
        argument(r, &sp_of)?;
    }
    for r in rows
        .get_mut(&SystemTable::DbAuthorization)
        .into_iter()
        .flatten()
    {
        grants(r, &users, &owner_of)?;
    }
    for r in rows.get_mut(&SystemTable::DbSerial).into_iter().flatten() {
        if let Some(v) = r.values.remove("att_name") {
            r.values.insert("attr_name".into(), v);
        }
    }
    Ok(())
}

fn procedure(r: &mut Row, users: &Users, sp_of: &mut HashMap<Oid, Oid>) -> DecodeResult<()> {
    let sp_name = stored_procedure::sp_name(&r.values)
        .ok_or_else(|| err(format!("procedure {:?} without sp_name", r.oid)))?;
    let owner = object(stored_procedure::owner(&r.values))
        .and_then(|o| users.by_oid.get(&o))
        .ok_or_else(|| {
            err(format!(
                "procedure {} without an owner user",
                String::from_utf8_lossy(sp_name)
            ))
        })?;
    let unique_name = [owner.0.to_ascii_lowercase().as_slice(), b".", sp_name].concat();
    let target = stored_procedure::target(&r.values).unwrap_or_default();
    let (class, method) = split_target_signature(target).ok_or_else(|| {
        err(format!(
            "procedure {} target {:?} has no Class.method(..)",
            String::from_utf8_lossy(sp_name),
            String::from_utf8_lossy(target)
        ))
    })?;
    if let Some(Value::Set(s)) = stored_procedure::args(&r.values) {
        for e in &s.elements {
            if let Value::Object(a) = e.value {
                sp_of.insert(a, r.oid);
            }
        }
    }
    let v = &mut r.values;
    v.remove("target");
    v.insert("unique_name".into(), Value::Varchar(unique_name));
    v.insert("target_class".into(), Value::Varchar(class));
    v.insert("target_method".into(), Value::Varchar(method));
    v.insert("pkg_name".into(), Value::Null);
    v.insert("is_system_generated".into(), Value::Integer(0));
    v.insert("directive".into(), Value::Integer(0));
    Ok(())
}

fn argument(r: &mut Row, sp_of: &HashMap<Oid, Oid>) -> DecodeResult<()> {
    // an argument no procedure lists is not something the engine makes (R12 § 4.4 V5)
    let sp = sp_of.get(&r.oid).ok_or_else(|| {
        err(format!(
            "stored procedure argument {:?} belongs to no procedure",
            r.oid
        ))
    })?;
    let v = &mut r.values;
    v.remove("sp_name");
    v.insert("sp_of".into(), Value::Object(*sp));
    v.insert("is_system_generated".into(), Value::Integer(0));
    v.insert("default_value".into(), Value::Null);
    v.insert("is_optional".into(), Value::Integer(0));
    Ok(())
}

fn grants(r: &mut Row, users: &Users, owner_of: &HashMap<Oid, Oid>) -> DecodeResult<()> {
    let Some(Value::Set(s)) = authorization::grants_mut(&mut r.values) else {
        return Ok(());
    };
    if !s.elements.len().is_multiple_of(OLD_GRANT_ENTRY_LENGTH) {
        return Err(err(format!(
            "db_authorization.grants: {} elements",
            s.elements.len()
        )));
    }
    // (class, grantor) → index in `entries`, in first-seen order
    let mut at: HashMap<(Oid, Oid), usize> = HashMap::new();
    let mut entries: Vec<[SetElement; 4]> = Vec::new();
    for e in s.elements.chunks(OLD_GRANT_ENTRY_LENGTH) {
        let [class, grantor, cache] = e else {
            unreachable!("chunks of 3")
        };
        let (Some(c), Some(g), Value::Integer(bits)) = (
            object(Some(&class.value)),
            object(Some(&grantor.value)),
            &cache.value,
        ) else {
            return Err(err(format!("db_authorization.grants entry {e:?}")));
        };
        let g = match owner_of.get(&c) {
            Some(&owner)
                if !users.by_oid.contains_key(&g)
                    || users.dba.is_some_and(|d| users.is_member(d, g))
                    || users.is_member(owner, g) =>
            {
                owner
            }
            _ => g,
        };
        match at.get(&(c, g)) {
            Some(&i) => {
                if let Value::Integer(b) = &mut entries[i][3].value {
                    *b |= bits;
                }
            }
            None => {
                at.insert((c, g), entries.len());
                let ty = SetElement {
                    tag: cache.tag.clone(),
                    value: Value::Integer(OBJECT_CLASS),
                };
                let grantor = SetElement {
                    tag: grantor.tag.clone(),
                    value: Value::Object(g),
                };
                entries.push([ty, class.clone(), grantor, cache.clone()]);
            }
        }
    }
    s.elements = entries.into_iter().flatten().collect();
    Ok(())
}
