//! Gives a source written before user schema (10.2, 11.0) the owner-qualified names (GLOSSARY) a database created by
//! 11.2 or later has, as the engine's official path (unloaddb → develop `loaddb --no-user-specified-name`) does:
//! every user class (partitions included), serial and trigger is named `lower(owner).name`, and every stored text
//! that names them or a stored procedure names them as `[owner.name]`. A source before 11.4 (10.2..11.3) gives
//! every stored procedure its `unique_name` `lower(owner).sp_name`, and a view or trigger text of an 11.2 or 11.3
//! source, whose classes and serials are already qualified, names its procedures as `[owner.sp]`.
//!
//! Input: the source version, the source classes, the same-slot rows in the 11.4 shape, the database codeset. Output: in place — each user
//! class object's `name`, its view query specs and partition expressions, `unique_name` of every `db_serial`,
//! `db_trigger` and `_db_stored_procedure` row and the trigger texts (11.2, 11.3: procedure `unique_name` and the
//! procedure calls in view and trigger texts only); `name`, `db_serial.class_name` and `attr_name` stay without owner. Returns
//! one warning per view or trigger that calls a stored procedure of another owner (develop requires EXECUTE on it,
//! which a source before 11.4 never granted; G11 workspace#545 decides what the tool grants).
//! Names the plan pre-check refuses (`owner_qualified_names` P1-P6) never reach here.
//! Source: R10 workspace#511 § 5 (`lower(owner).name`); T6 workspace#516 engine dumps
//! (basedb/owner-qualified-names-102 expected/*-develop-engine-load.txt: `_db_class.unique_name` `u1.t1` and
//! `dba.tdp__p__p0`, `_db_serial` `u2.s2` with `name` `s2` and `class_name` `t2_log`, `_db_trigger` `u1.trg1_ai`);
//! user decision G8 workspace#517 (2026-10-30, texts as the official path; an 11.3 source unloaded and loaded by develop
//! stops at its first view calling `sp2(..)`, "Function sp2 is undefined", so an 11.2/11.3 text takes the 10.2 path's
//! `[u2.sp2]`, measured in the workspace#517 resolution); workspace#544.

use std::collections::HashMap;

use fastmig_ondisk::value::{DecodeError, DecodeResult, Value};
use fastmig_ondisk::{Oid, SourceVersion};

use crate::class_rows::Row;
use crate::identifier_lower::IdentifierCasing;
use crate::owner_qualified_texts::{Owned, OwnerNames, qualify_text};
use crate::source_classes::SourceClass;
use crate::system_rows::{object, serial, stored_procedure, text, trigger, user};
use crate::system_table::SystemTable;

fn err(what: String) -> DecodeError {
    DecodeError { at: 0, what }
}

fn show(s: &[u8]) -> String {
    String::from_utf8_lossy(s).into_owned()
}

/// Qualifies the names and texts (see the module header).
pub fn qualify_names(
    version: SourceVersion,
    classes: &mut [SourceClass],
    rows: &mut HashMap<SystemTable, Vec<Row>>,
    charset: u8,
) -> DecodeResult<Vec<String>> {
    let before_user_schema = match version {
        SourceVersion::V10_2 | SourceVersion::V11_0 => true,
        SourceVersion::V11_2 | SourceVersion::V11_3 => false,
        SourceVersion::V11_4 | SourceVersion::Develop => return Ok(Vec::new()),
    };
    fn table(rows: &HashMap<SystemTable, Vec<Row>>, t: SystemTable) -> &[Row] {
        rows.get(&t).map(Vec::as_slice).unwrap_or_default()
    }
    let lang = table(rows, SystemTable::DbRoot)
        .first()
        .and_then(|r| text(r.values.get("lang")))
        .unwrap_or_default()
        .to_vec();
    let casing = IdentifierCasing::of(charset, &lang);
    let mut prefix: HashMap<Oid, Vec<u8>> = HashMap::new();
    let mut user_name: HashMap<Oid, Vec<u8>> = HashMap::new();
    for u in table(rows, SystemTable::DbUser) {
        let n = user::name(&u.values).unwrap_or_default();
        user_name.insert(u.oid, n.to_vec());
        prefix.insert(u.oid, casing.lower(n));
    }
    let owned = |o: Option<Oid>, what: &dyn Fn() -> String| -> DecodeResult<Owned> {
        let o = o.ok_or_else(|| err(format!("{} has no owner", what())))?;
        let p = prefix.get(&o).ok_or_else(|| {
            err(format!(
                "{} has an owner that is not a user ({o:?})",
                what()
            ))
        })?;
        Ok(Owned {
            owner: o,
            prefix: p.clone(),
        })
    };

    // every name a text can use, before any is changed; 11.2 and 11.3 texts already qualify classes and serials
    let mut names = OwnerNames::default();
    if before_user_schema {
        for c in classes.iter().filter(|c| !c.class.is_system_class()) {
            let o = owned(Some(c.class.owner), &|| {
                format!("class {}", show(&c.class.name))
            })?;
            names.classes.insert(c.class.name.to_ascii_lowercase(), o);
        }
        for r in table(rows, SystemTable::DbSerial) {
            let n = serial::name(&r.values).unwrap_or_default();
            let o = owned(object(r.values.get("owner")), &|| {
                format!("serial {}", show(n))
            })?;
            names.serials.insert(n.to_ascii_lowercase(), o);
        }
    }
    for r in table(rows, SystemTable::DbStoredProcedure) {
        let n = stored_procedure::sp_name(&r.values).unwrap_or_default();
        let o = owned(object(stored_procedure::owner(&r.values)), &|| {
            format!("stored procedure {}", show(n))
        })?;
        names.procedures.insert(n.to_ascii_lowercase(), o);
    }

    let mut warnings = Vec::new();
    let mut warn = |object: String, owner: Oid, calls: Vec<String>| {
        for p in calls {
            let (sp_owner, sp) = p.split_once('.').unwrap_or(("", &p));
            warnings.push(format!(
                "{object} calls stored procedure {p} of another owner; develop runs it only with EXECUTE, which the \
                 source never granted; fix: GRANT EXECUTE ON PROCEDURE [{sp_owner}].[{sp}] TO [{}]; (workspace#545)",
                show(user_name.get(&owner).map(Vec::as_slice).unwrap_or_default())
            ));
        }
    };
    for c in classes.iter_mut().filter(|c| !c.class.is_system_class()) {
        let k = &mut c.class;
        if before_user_schema {
            let short = k.name.clone();
            let o = &names.classes[&short.to_ascii_lowercase()];
            k.name = [o.prefix.as_slice(), b".", &short].concat();
        }
        let object = format!("view {}", show(&k.name));
        for q in &mut k.query_specs {
            let t = qualify_text(q, k.owner, &names);
            if let Some(r) = t.rewritten {
                *q = r;
            }
            warn(object.clone(), k.owner, t.other_owner_procedures);
        }
        for p in &mut k.partitions {
            if let Some(e) = &mut p.pexpr
                && let Some(r) = qualify_text(e, k.owner, &names).rewritten
            {
                *e = r;
            }
        }
    }
    for r in rows
        .get_mut(&SystemTable::DbStoredProcedure)
        .into_iter()
        .flatten()
    {
        let n = stored_procedure::sp_name(&r.values)
            .unwrap_or_default()
            .to_vec();
        let un = [
            names.procedures[&n.to_ascii_lowercase()].prefix.as_slice(),
            b".",
            &n,
        ]
        .concat();
        r.values.insert("unique_name".into(), Value::Varchar(un));
    }
    if before_user_schema {
        for r in rows.get_mut(&SystemTable::DbSerial).into_iter().flatten() {
            let n = serial::name(&r.values).unwrap_or_default().to_vec();
            let un = [
                names.serials[&n.to_ascii_lowercase()].prefix.as_slice(),
                b".",
                &n,
            ]
            .concat();
            r.values.insert("unique_name".into(), Value::Varchar(un));
        }
    }
    for r in rows.get_mut(&SystemTable::DbTrigger).into_iter().flatten() {
        let n = trigger::name(&r.values).unwrap_or_default().to_vec();
        let o = owned(trigger::owner(&r.values), &|| {
            format!("trigger {}", show(&n))
        })?;
        // an 11.2 or 11.3 trigger already has its unique_name
        let un = if before_user_schema {
            [o.prefix.as_slice(), b".", &n].concat()
        } else {
            text(r.values.get("unique_name")).unwrap_or(&n).to_vec()
        };
        let object = format!("trigger {}", show(&un));
        for col in ["condition", "action_definition"] {
            if let Some(Value::Varchar(t) | Value::Char(t)) = r.values.get_mut(col) {
                let q = qualify_text(t, o.owner, &names);
                if let Some(x) = q.rewritten {
                    *t = x;
                }
                warn(object.clone(), o.owner, q.other_owner_procedures);
            }
        }
        if before_user_schema {
            r.values.insert("unique_name".into(), Value::Varchar(un));
        }
    }
    Ok(warnings)
}
