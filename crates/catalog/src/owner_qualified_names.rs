//! The pre-check of a source written before user schema (10.2, 11.0): every class, serial, trigger and stored
//! procedure name must still be valid once it becomes an owner-qualified name in the target.
//!
//! Input: the source volume set and its source version. Output: one refusal per offending name (rules P1-P6), or an
//! error when the names cannot be read. Only the naming fields are decoded (class heads, the name and owner
//! attributes of four system classes), so values this tool cannot read yet (10.2 LZO strings, workspace#520) do not
//! stop the check.
//! Source: R10 § 5 (workspace#511) P1-P6 and its develop sites (`parser_support.c:10987` 222 B identifier,
//! `execute_schema.c:5101-5106` partition name, `csql_grammar.y:20633` no `.`, `schema_system_catalog.cpp:40-125`
//! system names, `authenticate_context.cpp:984-1008` INFORMATION_SCHEMA, `install.cpp:917` SP unique_name
//! varchar(255)); 10.2 system classes `db_user`, `db_serial`, `db_trigger` (`name`, `owner`) and
//! `_db_stored_procedure` (`sp_name`, `owner`) per release/10.2 `boot_cl.c:1841,3062,3105,3244-3250`,
//! `transform.h:133-140`, `authenticate.c:108`; workspace#518.

use std::collections::HashMap;
use std::io;

use fastmig_ondisk::boot_parm::BootDbParm;
use fastmig_ondisk::class_object::{self, ClassHead, ClassObject};
use fastmig_ondisk::record::{RecordHeader, RecordLayout, decode_record_attrs, heap_records};
use fastmig_ondisk::value::Value;
use fastmig_ondisk::volume_set::VolumeSet;
use fastmig_ondisk::{Oid, SourceVersion, VersionLine};

use crate::system_rows::{serial, stored_procedure, trigger, user};
use crate::system_table::SystemTable;

/// develop's longest class, serial or trigger name without its owner (P1, P2).
const NAME_MAX: usize = 222;
/// develop `_db_stored_procedure.unique_name varchar(255)` (P6).
const SP_UNIQUE_NAME_MAX: usize = 254;
/// develop system classes and vclasses a 10.2 user class may already be named (P4; R10 § 5, `dual` is a system
/// vclass from 11.0 on).
const DEVELOP_SYSTEM_NAMES: &[&str] = &[
    "_db_authorization",
    "_db_global_tran",
    "_db_ha_apply_info",
    "_db_histogram",
    "_db_password",
    "_db_serial",
    "_db_server",
    "_db_stored_procedure_code",
    "_db_synonym",
    "_db_trigger",
    "_db_user",
    "db_histogram",
    "db_server",
    "db_synonym",
    "dual",
];
const TICKET: &str = "workspace#518";
/// Values below the 255 B compression threshold read as 11.4 (R8 § 2.8).
const LINE: VersionLine = VersionLine::V11_4;

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

fn show(s: &[u8]) -> String {
    String::from_utf8_lossy(s).into_owned()
}

/// The live rows of system table `table`, with only the attributes in `attrs` decoded, by attribute name.
fn rows(
    vs: &VolumeSet,
    classes: &[(Oid, ClassObject)],
    table: SystemTable,
    attrs: &[&str],
) -> io::Result<Vec<(Oid, HashMap<String, Value>)>> {
    let name = table.source_name().unwrap_or_default();
    let (_, c) = classes
        .iter()
        .find(|(_, c)| c.name == name.as_bytes())
        .ok_or_else(|| invalid(format!("source has no system class {name}")))?;
    let ids: HashMap<i32, &str> = c
        .attributes
        .iter()
        .filter_map(|a| {
            attrs
                .iter()
                .find(|n| n.as_bytes() == a.name.as_slice())
                .map(|n| (a.id, *n))
        })
        .collect();
    let mut out = Vec::new();
    for r in heap_records(vs, c.heap) {
        let r = r?;
        let h = RecordHeader::read(&r.bytes).map_err(|e| invalid(format!("{name}: {e}")))?;
        if h.is_deleted() {
            continue;
        }
        let layout = RecordLayout::for_repid(LINE, c, h.repid)
            .map_err(|e| invalid(format!("{name} repid {}: {e}", h.repid)))?;
        let d = decode_record_attrs(LINE, &layout, &r.bytes, |id| ids.contains_key(&id))
            .map_err(|e| invalid(format!("{name} {:?}: {e}", r.oid)))?;
        let values = layout
            .attrs
            .iter()
            .zip(d.values)
            .filter_map(|(a, v)| ids.get(&a.id).map(|n| (n.to_string(), v)))
            .collect();
        out.push((r.oid, values));
    }
    Ok(out)
}

fn oid(v: Option<&Value>) -> Option<Oid> {
    match v {
        Some(Value::Object(o) | Value::Oid(o)) => Some(*o),
        _ => None,
    }
}

/// One partition of a partitioned class: its partition element names the partition (the partitioned class's own
/// element has no name).
fn is_partition(c: &ClassHead) -> bool {
    c.partitions.iter().any(|p| p.pname.is_some())
}

/// Refusals P1-P6 for a source written before user schema; empty for 11.2 and later, whose names are already
/// owner-qualified.
pub fn refusals(vs: &VolumeSet, version: SourceVersion) -> io::Result<Vec<String>> {
    if !matches!(version, SourceVersion::V10_2 | SourceVersion::V11_0) {
        return Ok(Vec::new());
    }
    let (_, boot) = BootDbParm::read(vs)?;
    let mut heads = Vec::new();
    let mut system = Vec::new();
    for r in heap_records(vs, boot.rootclass_hfid) {
        let r = r?;
        if r.oid == boot.rootclass_oid {
            continue;
        }
        let head = class_object::decode_head(&r.bytes, LINE)
            .map_err(|e| invalid(format!("class object {:?}: {e}", r.oid)))?;
        if [
            SystemTable::DbUser,
            SystemTable::DbSerial,
            SystemTable::DbTrigger,
            SystemTable::DbStoredProcedure,
        ]
        .iter()
        .any(|t| t.source_name().map(str::as_bytes) == Some(head.name.as_slice()))
        {
            // 11.0 has the 11.4 class object layout (R8 § 2.9)
            let full = match version {
                SourceVersion::V10_2 => class_object::decode_10_2(&r.bytes),
                _ => class_object::decode(&r.bytes, LINE),
            }
            .map_err(|e| invalid(format!("class {}: {e}", show(&head.name))))?;
            system.push((r.oid, full));
        }
        heads.push(head);
    }
    let users = rows(vs, &system, SystemTable::DbUser, &["name"])?;
    let user_name: HashMap<Oid, Vec<u8>> = users
        .iter()
        .map(|(o, v)| (*o, user::name(v).unwrap_or_default().to_vec()))
        .collect();
    let owner_of = |o: Option<Oid>| {
        o.and_then(|o| user_name.get(&o))
            .cloned()
            .unwrap_or_default()
    };
    let user_classes = || heads.iter().filter(|c| !c.is_system_class());

    let mut out = Vec::new();
    let mut name_rules = |kind: &str, name: &[u8]| {
        if name.len() > NAME_MAX {
            out.push(format!(
                "P1: {kind} {} is {} bytes; develop names hold at most {NAME_MAX} ({TICKET})",
                show(name),
                name.len()
            ));
        }
        if name.contains(&b'.') {
            out.push(format!(
                "P3: {kind} name {} contains '.' ({TICKET})",
                show(name)
            ));
        }
    };
    for c in user_classes() {
        name_rules("class", &c.name);
    }
    for (_, s) in rows(vs, &system, SystemTable::DbSerial, &["name"])? {
        name_rules("serial", serial::name(&s).unwrap_or_default());
    }
    for (_, t) in rows(vs, &system, SystemTable::DbTrigger, &["name"])? {
        name_rules("trigger", trigger::name(&t).unwrap_or_default());
    }
    for (_, u) in &users {
        let name = user::name(u).unwrap_or_default();
        if name.contains(&b'.') {
            out.push(format!(
                "P3: user name {} contains '.' ({TICKET})",
                show(name)
            ));
        }
        if name.eq_ignore_ascii_case(b"INFORMATION_SCHEMA") {
            out.push(format!(
                "P5: user {} collides with develop's system user INFORMATION_SCHEMA ({TICKET})",
                show(name)
            ));
        }
    }
    for c in user_classes() {
        let owner = owner_of(Some(c.owner));
        if is_partition(c) && owner.len() + 1 + c.name.len() > NAME_MAX {
            out.push(format!(
                "P2: partition {}.{} is {} bytes with its owner; develop holds at most {NAME_MAX} ({TICKET})",
                show(&owner).to_lowercase(),
                show(&c.name),
                owner.len() + 1 + c.name.len()
            ));
        }
        if DEVELOP_SYSTEM_NAMES.contains(&show(&c.name).as_str()) {
            out.push(format!(
                "P4: user class {} has the name of a develop system class ({TICKET})",
                show(&c.name)
            ));
        }
    }
    for (_, p) in rows(
        vs,
        &system,
        SystemTable::DbStoredProcedure,
        &["sp_name", "owner"],
    )? {
        let name = stored_procedure::sp_name(&p).unwrap_or_default();
        let owner = owner_of(oid(stored_procedure::owner(&p)));
        if owner.len() + 1 + name.len() > SP_UNIQUE_NAME_MAX {
            out.push(format!(
                "P6: stored procedure {}.{} is {} bytes; develop unique_name holds at most {SP_UNIQUE_NAME_MAX} \
                 ({TICKET})",
                show(&owner).to_lowercase(),
                show(name),
                owner.len() + 1 + name.len()
            ));
        }
    }
    Ok(out)
}
