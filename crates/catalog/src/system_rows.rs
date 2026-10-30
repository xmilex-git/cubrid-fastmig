//! Typed reads of the columns regeneration and the plan take from system table rows: the one place a column of a
//! system table is named by its string.
//!
//! Input: one row's values by column name (`class_rows::Row::values`, or a subset decoded by name). Output: the
//! column's value, as bytes for a text column and an OID for an object column. Rows read generically (every column
//! re-encoded into the develop layout, template rows) stay maps; only the columns some step looks at are here.
//! Source: column names of the 11.4 and develop catalogs (R4 § 2; `schema_system_catalog_install.cpp`, develop).

use std::collections::HashMap;

use fastmig_ondisk::Oid;
use fastmig_ondisk::value::Value;

/// A row's values by column name.
pub type Values = HashMap<String, Value>;

/// A text column: the bytes of a VARCHAR or CHAR value.
pub fn text(v: Option<&Value>) -> Option<&[u8]> {
    match v? {
        Value::Varchar(s) | Value::Char(s) => Some(s),
        _ => None,
    }
}

/// An object column: the OID it points at.
pub fn object(v: Option<&Value>) -> Option<Oid> {
    match v? {
        Value::Object(o) => Some(*o),
        _ => None,
    }
}

/// `_db_user` (11.4 `db_user`).
pub mod user {
    use super::{Values, text};
    use fastmig_ondisk::value::Value;

    pub fn name(r: &Values) -> Option<&[u8]> {
        text(r.get("name"))
    }

    pub fn groups(r: &Values) -> Option<&Value> {
        r.get("groups")
    }

    pub fn direct_groups(r: &Values) -> Option<&Value> {
        r.get("direct_groups")
    }
}

/// `_db_authorization` (11.4 `db_authorization`).
pub mod authorization {
    use super::Values;
    use fastmig_ondisk::value::Value;

    pub fn owner(r: &Values) -> Option<&Value> {
        r.get("owner")
    }

    pub fn grants(r: &Values) -> Option<&Value> {
        r.get("grants")
    }

    pub fn grants_mut(r: &mut Values) -> Option<&mut Value> {
        r.get_mut("grants")
    }
}

/// `_db_stored_procedure`.
pub mod stored_procedure {
    use super::{Values, text};
    use fastmig_ondisk::value::Value;

    pub fn sp_name(r: &Values) -> Option<&[u8]> {
        text(r.get("sp_name"))
    }

    pub fn unique_name(r: &Values) -> Option<&[u8]> {
        text(r.get("unique_name"))
    }

    /// The column as stored, for a reader that accepts only a VARCHAR.
    pub fn unique_name_value(r: &Values) -> Option<&Value> {
        r.get("unique_name")
    }

    pub fn owner(r: &Values) -> Option<&Value> {
        r.get("owner")
    }

    pub fn target(r: &Values) -> Option<&[u8]> {
        text(r.get("target"))
    }

    pub fn args(r: &Values) -> Option<&Value> {
        r.get("args")
    }

    pub fn args_mut(r: &mut Values) -> Option<&mut Value> {
        r.get_mut("args")
    }

    pub fn lang(r: &Values) -> Option<&Value> {
        r.get("lang")
    }

    pub fn sp_type(r: &Values) -> Option<&Value> {
        r.get("sp_type")
    }

    pub fn is_system_generated(r: &Values) -> Option<&Value> {
        r.get("is_system_generated")
    }
}

/// `_db_stored_procedure_code`.
pub mod stored_procedure_code {
    use super::{Values, text};

    pub fn name(r: &Values) -> Option<&[u8]> {
        text(r.get("name"))
    }

    pub fn scode(r: &Values) -> Option<&[u8]> {
        text(r.get("scode"))
    }
}

/// `_db_serial` (11.4 `db_serial`).
pub mod serial {
    use super::{Values, text};
    use fastmig_ondisk::value::Value;

    pub fn name(r: &Values) -> Option<&[u8]> {
        text(r.get("name"))
    }

    pub fn unique_name(r: &Values) -> Option<&[u8]> {
        text(r.get("unique_name"))
    }

    pub fn class_name(r: &Values) -> Option<&[u8]> {
        text(r.get("class_name"))
    }

    pub fn attr_name(r: &Values) -> Option<&[u8]> {
        text(r.get("attr_name"))
    }

    pub fn started(r: &Values) -> Option<&Value> {
        r.get("started")
    }

    pub fn current_val(r: &Values) -> Option<&Value> {
        r.get("current_val")
    }
}

/// `_db_trigger` (11.4 `db_trigger`).
pub mod trigger {
    use super::{Values, object, text};
    use fastmig_ondisk::Oid;

    pub fn owner(r: &Values) -> Option<Oid> {
        object(r.get("owner"))
    }

    pub fn name(r: &Values) -> Option<&[u8]> {
        text(r.get("name"))
    }

    pub fn condition(r: &Values) -> Option<&[u8]> {
        text(r.get("condition"))
    }

    pub fn action_definition(r: &Values) -> Option<&[u8]> {
        text(r.get("action_definition"))
    }
}

/// `_db_collation`.
pub mod collation {
    use super::Values;
    use fastmig_ondisk::value::Value;

    pub fn coll_id(r: &Values) -> Option<&Value> {
        r.get("coll_id")
    }

    pub fn coll_name(r: &Values) -> Option<&[u8]> {
        super::text(r.get("coll_name"))
    }

    pub fn built_in(r: &Values) -> Option<&Value> {
        r.get("built_in")
    }
}

/// `_db_class`.
pub mod class {
    use super::{Values, object};
    use fastmig_ondisk::Oid;
    use fastmig_ondisk::value::Value;

    pub fn class_of(r: &Values) -> Option<Oid> {
        object(r.get("class_of"))
    }

    pub fn indexes(r: &Values) -> Option<&Value> {
        r.get("indexes")
    }
}

/// `_db_index`.
pub mod index {
    use super::{Values, object, text};
    use fastmig_ondisk::Oid;

    pub fn class_of(r: &Values) -> Option<Oid> {
        object(r.get("class_of"))
    }

    pub fn index_name(r: &Values) -> Option<&[u8]> {
        text(r.get("index_name"))
    }
}

/// `_db_index_key`.
pub mod index_key {
    use super::{Values, object};
    use fastmig_ondisk::Oid;

    pub fn index_of(r: &Values) -> Option<Oid> {
        object(r.get("index_of"))
    }
}

/// `_db_server` (11.2 on).
pub mod server {
    use super::{Values, object, text};
    use fastmig_ondisk::Oid;
    use fastmig_ondisk::value::Value;

    pub fn link_name(r: &Values) -> Option<&[u8]> {
        text(r.get("link_name"))
    }

    pub fn owner(r: &Values) -> Option<Oid> {
        object(r.get("owner"))
    }

    pub fn password(r: &Values) -> Option<&[u8]> {
        text(r.get("password"))
    }

    pub fn set_password(r: &mut Values, password: Vec<u8>) {
        r.insert("password".into(), Value::Varchar(password));
    }
}
