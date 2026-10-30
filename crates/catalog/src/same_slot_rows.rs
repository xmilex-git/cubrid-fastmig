//! Rewrites the rows of the original-data system tables in the layout of the target line (the template's: develop or
//! 11.4), each staying in its slot, and adds the rows a fresh target database has that the source lacks (develop:
//! the `INFORMATION_SCHEMA` user and its authorization; from a pre-11.4 source: the built-in procedures).
//!
//! Input: the source rows of each same-slot table keyed by develop name (`class_rows::read_rows`), the target
//! classes, the catalog template, the template-user → target-user map (the same one `target_classes` placed the
//! new users with), the database charset, the migrate start time, the row allocator.
//! Output: `(table, Row)`s: one per source row with the source OID, then the new rows.
//! One mapping for both target lines (workspace#534): an 11.4 target keeps the source values (11.4 encoding) and
//! names; develop alone re-encodes values, fills its new columns and renames auto_increment serials (R12 § 5.3).
//! Source: workspace#482 decisions 1 (same slot, row OIDs kept), 2 (system users and grants from the template),
//! 8 (new columns: created/updated time = migrate start; invalidated/expire time NULL; `start_val` = `current_val`
//! when not started, else NULL; `is_loginable` 1; `is_system_created` 1 for DBA, PUBLIC, INFORMATION_SCHEMA only;
//! `sql_data_access` -1) and its auto_increment serial rename; R4 § 2 (columns per table), § 7 R8-R12 (grants on
//! 11.4 system classes dropped, develop's system grants added); T1 develop @682b080b2 (`db_root` and `_db_user`
//! lost `triggers`).

use std::collections::HashMap;

use fastmig_ondisk::class_object::{ClassObject, decode};
use fastmig_ondisk::record::RecordHeader;
use fastmig_ondisk::record::{RecordLayout, decode_record};
use fastmig_ondisk::value::{
    DbType, DecodeError, DecodeResult, Domain, SetElement, SetValue, Value, value_to_develop,
};
use fastmig_ondisk::{Oid, VersionLine};

use crate::class_rows::Row;
use crate::serial_name::name_develop;
use crate::system_classes::SAME_SLOT;
use crate::system_rows::{authorization, serial, stored_procedure, user};
use crate::system_table::SystemTable;
use crate::target_classes::{Origin, TargetClass};
use crate::template::CatalogTemplate;

/// Users develop marks `is_system_created` (`authenticate_context.hpp:get_system_users`).
const SYSTEM_USERS: [&[u8]; 3] = [b"DBA", b"PUBLIC", b"INFORMATION_SCHEMA"];
/// Source columns develop no longer has; any other missing column is an error.
const DROPPED_COLUMNS: [(SystemTable, &str); 2] = [
    (SystemTable::DbRoot, "triggers"),
    (SystemTable::DbUser, "triggers"),
];
/// `DB_OBJECT_CLASS` in a grant entry.
const OBJECT_CLASS: i32 = 0;
/// Engine `GRANT_ENTRY_LENGTH`.
const GRANT_ENTRY_LENGTH: usize = 4;

pub struct SameSlotInputs<'a> {
    /// Source rows by develop table name.
    pub source: &'a HashMap<SystemTable, Vec<Row>>,
    pub targets: &'a [TargetClass],
    pub template: &'a CatalogTemplate,
    /// Template user row → target user row (`Placement::user`).
    pub user: &'a dyn Fn(Oid) -> Oid,
    /// Volume header `db_charset`.
    pub charset: u8,
    /// The migrate start time (`Value::Datetime`).
    pub now: Value,
}

fn err(what: String) -> DecodeError {
    DecodeError { at: 0, what }
}

/// The value of a develop column the source row lacks (decision 8).
fn new_column(
    table: SystemTable,
    col: &str,
    row: &HashMap<String, Value>,
    now: &Value,
) -> DecodeResult<Value> {
    Ok(match col {
        "created_time" | "updated_time" => now.clone(),
        "invalidated_time" | "expire_time" => Value::Null,
        "is_loginable" => Value::Integer(1),
        "is_system_created" => {
            Value::Integer(SYSTEM_USERS.contains(&user::name(row).unwrap_or_default()) as i32)
        }
        "sql_data_access" => Value::Integer(-1),
        "start_val" => match serial::started(row) {
            Some(Value::Integer(0)) => serial::current_val(row).cloned().unwrap_or(Value::Null),
            _ => Value::Null,
        },
        _ => {
            return Err(err(format!(
                "{table}.{col}: no rule for a column the source lacks"
            )));
        }
    })
}

/// A target row from a source row: shared columns re-encoded for develop, new columns by rule, dropped columns
/// checked.
pub(crate) fn rewrite(
    line: VersionLine,
    table: SystemTable,
    t: &ClassObject,
    r: &Row,
    now: &Value,
) -> DecodeResult<Row> {
    let names: Vec<String> = t
        .attributes
        .iter()
        .map(|a| String::from_utf8_lossy(&a.name).into_owned())
        .collect();
    for n in r.values.keys() {
        if !names.contains(n) && !DROPPED_COLUMNS.contains(&(table, n.as_str())) {
            let src_name = table.source_name().unwrap_or(table.name());
            return Err(err(format!("{src_name}.{n} has no target column")));
        }
    }
    let mut values = HashMap::new();
    for n in &names {
        let v = match r.values.get(n) {
            Some(v) if line == VersionLine::Develop => value_to_develop(v.clone()),
            Some(v) => v.clone(),
            None => new_column(table, n, &r.values, now)?,
        };
        values.insert(n.clone(), v);
    }
    Ok(Row { oid: r.oid, values })
}

/// The template's target line.
pub(crate) fn template_line(template: &CatalogTemplate) -> DecodeResult<VersionLine> {
    template
        .line()
        .ok_or_else(|| err(format!("template line {:?}", template.meta("line"))))
}

/// The name of `table` in the template's line.
pub(crate) fn name_in(line: VersionLine, table: SystemTable) -> DecodeResult<&'static str> {
    table
        .name_in(line)
        .ok_or_else(|| err(format!("{table} is not a table of the target line")))
}

/// The rows of a template class, decoded with the template's class object.
pub fn template_rows(template: &CatalogTemplate, table: SystemTable) -> DecodeResult<Vec<Row>> {
    let line = template_line(template)?;
    let name = name_in(line, table)?;
    let rec = template
        .classes
        .iter()
        .find_map(|r| {
            let c = decode(&r.bytes, line).ok()?;
            (c.name == name.as_bytes()).then_some(c)
        })
        .ok_or_else(|| err(format!("template has no class {name}")))?;
    template
        .rows_of(name)
        .map(|r| {
            let h = RecordHeader::read(&r.bytes)?;
            let layout = RecordLayout::for_repid(line, &rec, h.repid)?;
            let d = decode_record(line, &layout, &r.bytes)?;
            let values = layout
                .attrs
                .iter()
                .zip(d.values)
                .filter_map(|(a, v)| {
                    let at = rec.attributes.iter().find(|x| x.id == a.id)?;
                    Some((String::from_utf8_lossy(&at.name).into_owned(), v))
                })
                .collect();
            Ok(Row { oid: r.oid, values })
        })
        .collect()
}

fn grant_entries(v: Option<&Value>) -> DecodeResult<Vec<Vec<Value>>> {
    match v {
        Some(Value::Set(s)) if s.elements.len().is_multiple_of(GRANT_ENTRY_LENGTH) => Ok(s
            .elements
            .chunks(GRANT_ENTRY_LENGTH)
            .map(|c| c.iter().map(|e| e.value.clone()).collect())
            .collect()),
        Some(Value::Null) | None => Ok(Vec::new()),
        v => Err(err(format!("grants {v:?}"))),
    }
}

/// The `grants` sequence of `entries`, each element tagged with its type (integers and objects mix).
fn grant_value(line: VersionLine, entries: Vec<Vec<Value>>) -> Value {
    Value::Set(SetValue {
        ty: DbType::Sequence,
        domain: Some(Domain::builtin(line, DbType::Sequence)),
        elements: entries
            .into_iter()
            .flatten()
            .map(|value| SetElement {
                tag: Some(Domain::builtin(
                    line,
                    if matches!(value, Value::Integer(_)) {
                        DbType::Integer
                    } else {
                        DbType::Object
                    },
                )),
                value,
            })
            .collect(),
    })
}

pub fn same_slot_rows(
    i: &SameSlotInputs,
    alloc: &mut dyn FnMut(SystemTable) -> Oid,
) -> DecodeResult<Vec<(SystemTable, Row)>> {
    let line = template_line(i.template)?;
    let target = |table: SystemTable| {
        let name = name_in(line, table)?;
        i.targets
            .iter()
            .find(|t| t.class.name == name.as_bytes())
            .ok_or_else(|| err(format!("no target class {name}")))
    };
    let mut out = Vec::new();
    for table in SAME_SLOT {
        let t = &target(table)?.class;
        for r in i.source.get(&table).map(Vec::as_slice).unwrap_or_default() {
            let mut row = rewrite(line, table, t, r, &i.now)?;
            if line == VersionLine::Develop && table == SystemTable::DbSerial {
                rename_serial(&mut row, i.charset);
            }
            out.push((table, row));
        }
    }

    // template class OID -> target class OID, by name (system classes only)
    let mut class_of: HashMap<Oid, Oid> = HashMap::new();
    for r in &i.template.classes {
        let c = decode(&r.bytes, line)?;
        if let Some(t) = i.targets.iter().find(|t| t.class.name == c.name) {
            class_of.insert(r.oid, t.oid);
        }
    }
    let user_classes: Vec<Oid> = i
        .targets
        .iter()
        .filter(|t| t.origin == Origin::User)
        .map(|t| t.oid)
        .collect();

    // new users (INFORMATION_SCHEMA) with their authorization rows
    let tusers = template_rows(i.template, SystemTable::DbUser)?;
    let tauths = template_rows(i.template, SystemTable::DbAuthorization)?;
    let source_users: Vec<Oid> = out
        .iter()
        .filter(|(t, _)| *t == SystemTable::DbUser)
        .map(|(_, r)| r.oid)
        .collect();
    let user_seq = |v: Option<&Value>| -> Value {
        match v {
            Some(Value::Set(s)) => {
                let mut s = s.clone();
                for e in &mut s.elements {
                    if let Value::Object(o) = e.value {
                        e.value = Value::Object((i.user)(o));
                    }
                }
                Value::Set(s)
            }
            v => v.cloned().unwrap_or(Value::Null),
        }
    };
    let ut = target(SystemTable::DbUser)?;
    let at = target(SystemTable::DbAuthorization)?;
    // authorization rows created here hold only the template's grants
    let mut new_auths = Vec::new();
    for u in &tusers {
        let oid = (i.user)(u.oid);
        if source_users.contains(&oid) {
            continue;
        }
        let auth = alloc(SystemTable::DbAuthorization);
        new_auths.push(auth);
        let mut values = rewrite(line, SystemTable::DbUser, &ut.class, u, &i.now)?.values;
        values.insert("authorization".into(), Value::Object(auth));
        values.insert("groups".into(), user_seq(user::groups(&u.values)));
        values.insert(
            "direct_groups".into(),
            user_seq(user::direct_groups(&u.values)),
        );
        // the template's times are its createdb's; a target line with these columns gets the migrate start
        for col in ["created_time", "updated_time"] {
            if let Some(v) = values.get_mut(col) {
                *v = i.now.clone();
            }
        }
        out.push((SystemTable::DbUser, Row { oid, values }));
        let ta = tauths
            .iter()
            .find(|a| authorization::owner(&a.values) == Some(&Value::Object(u.oid)))
            .ok_or_else(|| err("template user without authorization row".into()))?;
        let mut values = rewrite(line, SystemTable::DbAuthorization, &at.class, ta, &i.now)?.values;
        values.insert("owner".into(), Value::Object(oid));
        out.push((SystemTable::DbAuthorization, Row { oid: auth, values }));
    }

    // procedures develop's createdb installs (`sp_builtin_install`, public.dbms_output.*) that the source lacks — a
    // pre-11.4 source (R12 § 4.6, workspace#529) — with their arguments, in the template's `args` order
    let have: Vec<Vec<u8>> = out
        .iter()
        .filter(|(t, _)| *t == SystemTable::DbStoredProcedure)
        .filter_map(|(_, r)| stored_procedure::unique_name(&r.values).map(<[u8]>::to_vec))
        .collect();
    let (spt, argt) = (
        target(SystemTable::DbStoredProcedure)?,
        target(SystemTable::DbStoredProcedureArgs)?,
    );
    let targs = template_rows(i.template, SystemTable::DbStoredProcedureArgs)?;
    for sp in template_rows(i.template, SystemTable::DbStoredProcedure)? {
        if stored_procedure::unique_name(&sp.values).is_none_or(|n| have.iter().any(|h| h == n)) {
            continue;
        }
        let oid = alloc(SystemTable::DbStoredProcedure);
        let mut values = rewrite(
            line,
            SystemTable::DbStoredProcedure,
            &spt.class,
            &sp,
            &i.now,
        )?
        .values;
        if let Some(Value::Object(o)) = stored_procedure::owner(&values) {
            values.insert("owner".into(), Value::Object((i.user)(*o)));
        }
        if let Some(Value::Set(s)) = stored_procedure::args_mut(&mut values) {
            for e in &mut s.elements {
                let Value::Object(a) = e.value else { continue };
                let ta = targs
                    .iter()
                    .find(|r| r.oid == a)
                    .ok_or_else(|| err(format!("template argument {a:?} missing")))?;
                let aoid = alloc(SystemTable::DbStoredProcedureArgs);
                let mut av = rewrite(
                    line,
                    SystemTable::DbStoredProcedureArgs,
                    &argt.class,
                    ta,
                    &i.now,
                )?
                .values;
                av.insert("sp_of".into(), Value::Object(oid));
                out.push((
                    SystemTable::DbStoredProcedureArgs,
                    Row {
                        oid: aoid,
                        values: av,
                    },
                ));
                e.value = Value::Object(aoid);
            }
        }
        out.push((SystemTable::DbStoredProcedure, Row { oid, values }));
    }

    // grants: the template's system grants, then the source's grants on user classes and procedures
    let tgrants: HashMap<Oid, Vec<Vec<Value>>> = tauths
        .iter()
        .filter_map(|a| match authorization::owner(&a.values) {
            Some(Value::Object(o)) => Some(((i.user)(*o), authorization::grants(&a.values))),
            _ => None,
        })
        .map(|(o, g)| Ok((o, grant_entries(g)?)))
        .collect::<DecodeResult<_>>()?;
    for (_, r) in out
        .iter_mut()
        .filter(|(t, _)| *t == SystemTable::DbAuthorization)
    {
        let owner = match authorization::owner(&r.values) {
            Some(Value::Object(o)) => *o,
            v => return Err(err(format!("_db_authorization owner {v:?}"))),
        };
        let mut entries = Vec::new();
        for mut e in tgrants.get(&owner).cloned().unwrap_or_default() {
            let (Value::Object(cls), Value::Object(grantor)) = (e[1].clone(), e[2].clone()) else {
                return Err(err(format!("template grant entry {e:?}")));
            };
            let cls = *class_of
                .get(&cls)
                .ok_or_else(|| err(format!("template grant on {cls:?}, not a template class")))?;
            e[1] = Value::Object(cls);
            e[2] = Value::Object((i.user)(grantor));
            entries.push(e);
        }
        let source_entries = if new_auths.contains(&r.oid) {
            Vec::new()
        } else {
            grant_entries(authorization::grants(&r.values))?
        };
        for e in source_entries {
            let keep = match (&e[0], &e[1]) {
                (Value::Integer(OBJECT_CLASS), Value::Object(o)) => user_classes.contains(o),
                _ => true,
            };
            if keep {
                entries.push(e);
            }
        }
        r.values.insert("grants".into(), grant_value(line, entries));
    }
    Ok(out)
}

/// An auto_increment serial takes develop's name (`unique_name` and the part after the owner in `name`).
fn rename_serial(row: &mut Row, charset: u8) {
    let (Some(un), Some(cn), Some(an)) = (
        serial::unique_name(&row.values).map(<[u8]>::to_vec),
        serial::class_name(&row.values).map(<[u8]>::to_vec),
        serial::attr_name(&row.values).map(<[u8]>::to_vec),
    ) else {
        return;
    };
    let owner = un.split(|&b| b == b'.').next().unwrap_or_default();
    let class = [owner, b".", &cn].concat();
    let new = name_develop(&class, &an, charset);
    let short = new[new.iter().position(|&b| b == b'.').map_or(0, |d| d + 1)..].to_vec();
    row.values.insert("unique_name".into(), Value::Varchar(new));
    row.values.insert("name".into(), Value::Varchar(short));
}
