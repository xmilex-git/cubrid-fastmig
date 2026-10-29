//! The SQL files a migration leaves next to the target for the user to run on the target engine:
//! `deferred-indexes.sql`, `recompile-plcsql.sql`, `update-statistics.sql`.
//!
//! Input: the deferred (function/filter) indexes with their class objects; the PL/CSQL routine names.
//! Output: the files' text.
//! Source: workspace#486 (deferred indexes: `CREATE INDEX` from the class object's `expr_str`/`pred_string`),
//! workspace#488 (PL/CSQL routines to recompile), workspace#482 7 (`UPDATE STATISTICS ON ALL CLASSES;`);
//! DDL shape of release/11.4 `unload_schema.c:emit_index_def` (function key at `col_id`, `attr_index_start + 1`
//! keys, `) where <pred>`, `DESC`, `INVISIBLE`, `COMMENT`); function info `{expr_str, expr_stream, col_id,
//! attr_index_start, fi_domain}` and filter info `{pred_string, pred_stream, att_ids}` (R7 § 3.5-3.6).

use fastmig_ondisk::VersionLine;
use fastmig_ondisk::class_object::{ClassObject, Constraint, ConstraintKind};
use fastmig_ondisk::value::{OrReader, Value, decode_packed_domain};

/// Engine `SM_INVISIBLE_INDEX`.
const INVISIBLE_INDEX: i32 = 2;

pub const UPDATE_STATISTICS_SQL: &str = "UPDATE STATISTICS ON ALL CLASSES;\n";

fn ident(s: &[u8]) -> String {
    format!("[{}]", String::from_utf8_lossy(s).replace(']', "]]"))
}

fn quoted(s: &[u8]) -> String {
    format!("'{}'", String::from_utf8_lossy(s).replace('\'', "''"))
}

/// `owner.class` → `[owner].[class]`.
fn class_ident(unique_name: &[u8]) -> String {
    match unique_name.iter().position(|&b| b == b'.') {
        Some(p) => format!(
            "{}.{}",
            ident(&unique_name[..p]),
            ident(&unique_name[p + 1..])
        ),
        None => ident(unique_name),
    }
}

/// The `{name, info}` entry of an index's optional sequence.
fn named_info<'a>(
    c: &'a Constraint,
    name: &[u8],
) -> Option<&'a [fastmig_ondisk::value::SetElement]> {
    let Some(Value::Set(s)) = &c.optional else {
        return None;
    };
    s.elements.iter().find_map(|e| match &e.value {
        Value::Set(p) if matches!(p.elements.first().map(|n| &n.value), Some(Value::Varchar(n)) if n == name) => {
            match p.elements.get(1).map(|i| &i.value) {
                Some(Value::Set(info)) => Some(info.elements.as_slice()),
                _ => None,
            }
        }
        _ => None,
    })
}

fn text(v: Option<&Value>) -> Option<Vec<u8>> {
    match v? {
        Value::Varchar(s) | Value::Char(s) => Some(s.clone()),
        _ => None,
    }
}

fn int(v: Option<&Value>) -> Option<i32> {
    match v? {
        Value::Integer(i) => Some(*i),
        _ => None,
    }
}

/// `CREATE INDEX` for one function or filter index of `class` (source line `line`).
pub fn create_index(
    class: &ClassObject,
    c: &Constraint,
    line: VersionLine,
) -> Result<String, String> {
    let kind = match c.kind {
        ConstraintKind::Index => "",
        ConstraintKind::ReverseIndex => "REVERSE ",
        ConstraintKind::Unique => "UNIQUE ",
        ConstraintKind::ReverseUnique => "REVERSE UNIQUE ",
        k => return Err(format!("{k:?} is not an index")),
    };
    let att = |id: i32| {
        class
            .attributes
            .iter()
            .find(|a| a.id == id)
            .map(|a| a.name.clone())
            .ok_or_else(|| {
                format!(
                    "index {}: key attribute {id} not in the class",
                    String::from_utf8_lossy(&c.name)
                )
            })
    };
    let func = named_info(c, b"*FI*");
    let mut keys: Vec<String> = Vec::new();
    let mut plain = c.keys.iter();
    let n_keys = match func {
        Some(f) => {
            int(f.get(3).map(|e| &e.value)).ok_or("function info without attr_index_start")?
                as usize
                + 1
        }
        None => c.keys.len(),
    };
    for k in 0..n_keys {
        if let Some(f) = func
            && int(f.get(2).map(|e| &e.value)) == Some(k as i32)
        {
            let expr = text(f.first().map(|e| &e.value)).ok_or("function info without expr_str")?;
            let desc = match text(f.get(4).map(|e| &e.value)) {
                Some(d) => decode_packed_domain(line, &mut OrReader::new(&d))
                    .map(|(ds, _)| ds.first().is_some_and(|d| d.is_desc))
                    .unwrap_or(false),
                None => false,
            };
            keys.push(format!(
                "{}{}",
                String::from_utf8_lossy(&expr),
                if desc { " DESC" } else { "" }
            ));
            continue;
        }
        let (id, asc_desc) = plain
            .next()
            .ok_or("fewer keys than the function info says")?;
        keys.push(format!(
            "{}{}",
            ident(&att(*id)?),
            if *asc_desc == 1 { " DESC" } else { "" }
        ));
    }
    let mut s = format!(
        "CREATE {kind}INDEX {} ON {} ({})",
        ident(&c.name),
        class_ident(&class.name),
        keys.join(", ")
    );
    if let Some(f) = named_info(c, b"*FP*") {
        let pred = text(f.first().map(|e| &e.value)).ok_or("filter info without pred_string")?;
        s.push_str(&format!(" WHERE {}", String::from_utf8_lossy(&pred)));
    }
    if c.status == INVISIBLE_INDEX {
        s.push_str(" INVISIBLE");
    }
    if let Some(cm) = c.comment.as_ref().filter(|cm| !cm.is_empty()) {
        s.push_str(&format!(" COMMENT {}", quoted(cm)));
    }
    s.push_str(";\n");
    Ok(s)
}

/// `deferred-indexes.sql`: one `CREATE INDEX` per deferred index. Indexes of a partition (a class whose partition
/// element has a name) are skipped: the engine creates them with the partitioned class's index (as unloaddb does).
pub fn deferred_indexes_sql(
    items: &[(&ClassObject, &Constraint)],
    line: VersionLine,
) -> Result<String, String> {
    let mut s = String::from(
        "-- Function and filter indexes cubrid-fastmig did not build (workspace#486). Run on the target after the\n\
         -- migration verified: csql -u dba <db> -i deferred-indexes.sql\n",
    );
    for (class, c) in items {
        if class.partitions.iter().any(|p| p.pname.is_some()) {
            continue;
        }
        s.push_str(&create_index(class, c, line)?);
    }
    Ok(s)
}

/// `recompile-plcsql.sql`: `ALTER {PROCEDURE|FUNCTION} [owner].[name] COMPILE;` per PL/CSQL routine.
/// `routines` are `(unique name owner.name, is_function)`.
pub fn recompile_plcsql_sql(routines: &[(Vec<u8>, bool)]) -> String {
    let mut s = String::from(
        "-- PL/CSQL routines whose compiled code was copied as is (workspace#488). Run on the target if a call fails\n\
         -- with a PL/CSQL load error: csql -u dba <db> -i recompile-plcsql.sql\n",
    );
    for (name, is_function) in routines {
        s.push_str(&format!(
            "ALTER {} {} COMPILE;\n",
            if *is_function {
                "FUNCTION"
            } else {
                "PROCEDURE"
            },
            class_ident(name)
        ));
    }
    s
}

/// The user PL/CSQL routines among `_db_stored_procedure` rows: `lang` = `SP_LANG_PLCSQL` (0) and not
/// `is_system_generated`, as `(unique_name, sp_type == SP_TYPE_FUNCTION)` (engine `sp_constants.hpp`, same values in
/// 11.4 and develop).
pub fn plcsql_routines(rows: &[crate::class_rows::Row]) -> Vec<(Vec<u8>, bool)> {
    let int = |r: &crate::class_rows::Row, k: &str| match r.values.get(k) {
        Some(Value::Integer(i)) => Some(*i),
        _ => None,
    };
    rows.iter()
        .filter(|r| int(r, "lang") == Some(0) && int(r, "is_system_generated") != Some(1))
        .filter_map(|r| match r.values.get("unique_name") {
            Some(Value::Varchar(n)) => Some((n.clone(), int(r, "sp_type") == Some(2))),
            _ => None,
        })
        .collect()
}
