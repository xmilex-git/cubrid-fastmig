//! Rewrites the rows of the original-data system tables in the develop layout, each staying in its slot, and adds
//! the rows a develop database has that 11.4 lacks (the `INFORMATION_SCHEMA` user and its authorization).
//!
//! Input: the source rows of each same-slot table keyed by develop name (`class_rows::read_rows`), the target
//! classes, the catalog template, the template-user → target-user map (the same one `target_classes` placed the
//! new users with), the database charset, the migrate start time, the row allocator.
//! Output: `(develop table, Row)`s: one per source row with the source OID, then the new rows.
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
use crate::system_table::SystemTable;
use crate::target_classes::{Origin, TargetClass};
use crate::template::CatalogTemplate;

const LINE: VersionLine = VersionLine::Develop;
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

fn str_of(v: Option<&Value>) -> Option<&[u8]> {
    match v {
        Some(Value::Varchar(s) | Value::Char(s)) => Some(s),
        _ => None,
    }
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
        "is_system_created" => Value::Integer(
            SYSTEM_USERS.contains(&str_of(row.get("name")).unwrap_or_default()) as i32,
        ),
        "sql_data_access" => Value::Integer(-1),
        "start_val" => match row.get("started") {
            Some(Value::Integer(0)) => row.get("current_val").cloned().unwrap_or(Value::Null),
            _ => Value::Null,
        },
        _ => {
            return Err(err(format!(
                "{table}.{col}: no rule for a column 11.4 lacks"
            )));
        }
    })
}

/// A develop row from a source row: shared columns re-encoded, new columns by rule, dropped columns checked.
pub(crate) fn rewrite(
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
            return Err(err(format!("{src_name}.{n} has no develop column")));
        }
    }
    let mut values = HashMap::new();
    for n in &names {
        let v = match r.values.get(n) {
            Some(v) => value_to_develop(v),
            None => new_column(table, n, &r.values, now)?,
        };
        values.insert(n.clone(), v);
    }
    Ok(Row { oid: r.oid, values })
}

/// The rows of a template class, decoded with the template's class object.
pub fn template_rows(template: &CatalogTemplate, table: SystemTable) -> DecodeResult<Vec<Row>> {
    let name = table.name();
    let rec = template
        .classes
        .iter()
        .find_map(|r| {
            let c = decode(&r.bytes, LINE).ok()?;
            (c.name == name.as_bytes()).then_some(c)
        })
        .ok_or_else(|| err(format!("template has no class {name}")))?;
    template
        .rows_of(name)
        .map(|r| {
            let h = RecordHeader::read(&r.bytes)?;
            let layout = RecordLayout::for_repid(LINE, &rec, h.repid)?;
            let d = decode_record(LINE, &layout, &r.bytes)?;
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
fn grant_value(entries: Vec<Vec<Value>>) -> Value {
    Value::Set(SetValue {
        ty: DbType::Sequence,
        domain: Some(Domain::builtin(LINE, DbType::Sequence)),
        elements: entries
            .into_iter()
            .flatten()
            .map(|value| SetElement {
                tag: Some(Domain::builtin(
                    LINE,
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
    let target = |table: SystemTable| {
        i.targets
            .iter()
            .find(|t| t.class.name == table.name().as_bytes())
            .ok_or_else(|| err(format!("no target class {table}")))
    };
    let mut out = Vec::new();
    for dev_name in SAME_SLOT {
        let t = &target(dev_name)?.class;
        for r in i
            .source
            .get(&dev_name)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let mut row = rewrite(dev_name, t, r, &i.now)?;
            if dev_name == SystemTable::DbSerial {
                rename_serial(&mut row, i.charset);
            }
            out.push((dev_name, row));
        }
    }

    // template class OID -> target class OID, by name (system classes only)
    let mut class_of: HashMap<Oid, Oid> = HashMap::new();
    for r in &i.template.classes {
        let c = decode(&r.bytes, LINE)?;
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
        let mut values = rewrite(SystemTable::DbUser, &ut.class, u, &i.now)?.values;
        values.insert("authorization".into(), Value::Object(auth));
        values.insert("groups".into(), user_seq(u.values.get("groups")));
        values.insert(
            "direct_groups".into(),
            user_seq(u.values.get("direct_groups")),
        );
        values.insert("created_time".into(), i.now.clone());
        values.insert("updated_time".into(), i.now.clone());
        out.push((SystemTable::DbUser, Row { oid, values }));
        let ta = tauths
            .iter()
            .find(|a| a.values.get("owner") == Some(&Value::Object(u.oid)))
            .ok_or_else(|| err("template user without authorization row".into()))?;
        let mut values = rewrite(SystemTable::DbAuthorization, &at.class, ta, &i.now)?.values;
        values.insert("owner".into(), Value::Object(oid));
        out.push((SystemTable::DbAuthorization, Row { oid: auth, values }));
    }

    // procedures develop's createdb installs (`sp_builtin_install`, public.dbms_output.*) that the source lacks — a
    // pre-11.4 source (R12 § 4.6, workspace#529) — with their arguments, in the template's `args` order
    let have: Vec<Vec<u8>> = out
        .iter()
        .filter(|(t, _)| *t == SystemTable::DbStoredProcedure)
        .filter_map(|(_, r)| str_of(r.values.get("unique_name")).map(<[u8]>::to_vec))
        .collect();
    let (spt, argt) = (
        target(SystemTable::DbStoredProcedure)?,
        target(SystemTable::DbStoredProcedureArgs)?,
    );
    let targs = template_rows(i.template, SystemTable::DbStoredProcedureArgs)?;
    for sp in template_rows(i.template, SystemTable::DbStoredProcedure)? {
        if str_of(sp.values.get("unique_name")).is_none_or(|n| have.iter().any(|h| h == n)) {
            continue;
        }
        let oid = alloc(SystemTable::DbStoredProcedure);
        let mut values = rewrite(SystemTable::DbStoredProcedure, &spt.class, &sp, &i.now)?.values;
        if let Some(Value::Object(o)) = values.get("owner") {
            values.insert("owner".into(), Value::Object((i.user)(*o)));
        }
        if let Some(Value::Set(s)) = values.get_mut("args") {
            for e in &mut s.elements {
                let Value::Object(a) = e.value else { continue };
                let ta = targs
                    .iter()
                    .find(|r| r.oid == a)
                    .ok_or_else(|| err(format!("template argument {a:?} missing")))?;
                let aoid = alloc(SystemTable::DbStoredProcedureArgs);
                let mut av =
                    rewrite(SystemTable::DbStoredProcedureArgs, &argt.class, ta, &i.now)?.values;
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
        .filter_map(|a| match a.values.get("owner") {
            Some(Value::Object(o)) => Some(((i.user)(*o), a.values.get("grants"))),
            _ => None,
        })
        .map(|(o, g)| Ok((o, grant_entries(g)?)))
        .collect::<DecodeResult<_>>()?;
    for (_, r) in out
        .iter_mut()
        .filter(|(t, _)| *t == SystemTable::DbAuthorization)
    {
        let owner = match r.values.get("owner") {
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
            grant_entries(r.values.get("grants"))?
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
        r.values.insert("grants".into(), grant_value(entries));
    }
    Ok(out)
}

/// An auto_increment serial takes develop's name (`unique_name` and the part after the owner in `name`).
fn rename_serial(row: &mut Row, charset: u8) {
    let (Some(un), Some(cn), Some(an)) = (
        str_of(row.values.get("unique_name")).map(<[u8]>::to_vec),
        str_of(row.values.get("class_name")).map(<[u8]>::to_vec),
        str_of(row.values.get("attr_name")).map(<[u8]>::to_vec),
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
