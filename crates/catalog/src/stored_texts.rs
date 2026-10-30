//! Applies the system name mapping to the user texts the source catalog stores: every user view's query specs (in
//! its class object; `_db_query_spec` rows are generated from them) and every trigger's condition and action
//! definition (`db_trigger` rows). A rewritten text replaces the stored one in place, so the class object, the
//! regenerated `_db_query_spec` row and the `_db_trigger` row all carry the same value.
//!
//! Input: the source version, the source classes, the source `db_user` rows and `db_trigger` rows. Output: the texts
//! rewritten in place and one `(object, judgement)` per text that names a mapped or checked system class.
//! Source: workspace#542 items 3-4; owner of a view = its class object `owner`, of a trigger = `db_trigger.owner`;
//! DBA group = `DBA` itself or a user whose `groups` (every group, direct or not) holds `DBA` (R11 workspace#519 § 5.4).

use std::collections::HashSet;

use fastmig_ondisk::class_object::ClassType;
use fastmig_ondisk::value::Value;
use fastmig_ondisk::{Oid, SourceVersion, VersionLine};

use crate::class_rows::Row;
use crate::source_classes::SourceClass;
use crate::system_name_mapping::{TextJudgement, judge};
use crate::system_rows::{trigger, user};

/// The `db_user` rows of the DBA group.
pub fn dba_group(users: &[Row]) -> HashSet<Oid> {
    let Some(dba) = users
        .iter()
        .find(|u| user::name(&u.values) == Some(b"DBA"))
        .map(|u| u.oid)
    else {
        return HashSet::new();
    };
    users
        .iter()
        .filter(|u| {
            u.oid == dba
                || matches!(user::groups(&u.values), Some(Value::Set(s))
                    if s.elements.iter().any(|e| e.value == Value::Object(dba)))
        })
        .map(|u| u.oid)
        .collect()
}

/// Judges `text`, replaces it with the rewrite when there is one and nothing refuses, and records the judgement.
fn map_one(
    version: SourceVersion,
    to: VersionLine,
    object: String,
    text: &mut Vec<u8>,
    owner_in_dba_group: bool,
    out: &mut Vec<(String, TextJudgement)>,
) {
    let j = judge(version, to, text, owner_in_dba_group);
    if j.renames.is_empty() && j.refusals.is_empty() && j.warnings.is_empty() {
        return;
    }
    if let Some(t) = &j.rewritten {
        text.clone_from(t);
    }
    out.push((object, j));
}

/// Maps every stored user text (see the module header).
pub fn map_stored_texts(
    version: SourceVersion,
    to: VersionLine,
    classes: &mut [SourceClass],
    users: &[Row],
    triggers: &mut [Row],
) -> Vec<(String, TextJudgement)> {
    let dba = dba_group(users);
    let mut out = Vec::new();
    for c in classes
        .iter_mut()
        .filter(|c| c.class.class_type == ClassType::Vclass && !c.class.is_system_class())
    {
        let object = format!("view {}", String::from_utf8_lossy(&c.class.name));
        let in_dba = dba.contains(&c.class.owner);
        for q in &mut c.class.query_specs {
            map_one(version, to, object.clone(), q, in_dba, &mut out);
        }
    }
    for r in triggers {
        let name =
            String::from_utf8_lossy(trigger::name(&r.values).unwrap_or_default()).into_owned();
        let in_dba = trigger::owner(&r.values).is_some_and(|o| dba.contains(&o));
        for col in ["condition", "action_definition"] {
            if let Some(Value::Varchar(t) | Value::Char(t)) = r.values.get_mut(col) {
                map_one(
                    version,
                    to,
                    format!("trigger {name} {col}"),
                    t,
                    in_dba,
                    &mut out,
                );
            }
        }
    }
    out
}
