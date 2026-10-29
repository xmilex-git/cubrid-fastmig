//! Derives the `_db_auth` rows from the target's `_db_authorization` rows: one row per granted privilege bit.
//!
//! Input: the target `_db_authorization` rows (`owner` = grantee, `grants` = flat `{type, object, grantor, cache}`
//! entries), the migrate start time, the row allocator. Output: `("_db_auth", Row)`s.
//! Source: engine develop @682b080b2 `authenticate_grant.cpp:au_force_write_new_auth` (walk every entry),
//! `authenticate_access_auth.cpp` `au_auth_accessor::insert_auth` (bits from EXECUTE down to SELECT, one row each),
//! `set_new_auth` (`auth_type` = `AU_TYPE_SET` name, `object_of` = the class OID or the `_db_stored_procedure` row),
//! `authenticate_grant.cpp:au_grant` (`is_grantable` = the bit's grant-option bit `cache >> AU_GRANT_SHIFT`);
//! `GRANT_ENTRY_*` (`authenticate_grant.hpp`); R4 § 5 C; workspace#482 decision 8 (times).
//!
//! `au_force_write_new_auth` passes `auth & AU_GRANT_MASK` where `insert_auth` tests the unshifted bit, so it
//! writes `is_grantable` 0 for every row; this follows `au_grant`, which a database built by GRANT statements
//! shows (T1 develop: APP_GROUP's grantable SELECT/INSERT/UPDATE rows have 1).

use fastmig_ondisk::Oid;
use fastmig_ondisk::value::{DecodeError, DecodeResult, Value};

use crate::class_rows::Row;
use crate::ct_rows::{Sink, int, text};

/// Engine `AU_TYPE_SET`, by bit position (`DB_AUTH_SELECT` = 1 .. `DB_AUTH_EXECUTE` = 64).
const AUTH_NAMES: [&[u8]; 7] = [
    b"SELECT", b"INSERT", b"UPDATE", b"DELETE", b"ALTER", b"INDEX", b"EXECUTE",
];
/// Engine `GRANT_ENTRY_LENGTH`, `AU_GRANT_SHIFT`.
const GRANT_ENTRY_LENGTH: usize = 4;
const GRANT_SHIFT: i32 = 8;

fn err(what: String) -> DecodeError {
    DecodeError { at: 0, what }
}

pub fn auth_rows(
    authorizations: &[&Row],
    now: &Value,
    alloc: &mut dyn FnMut(&'static str) -> Oid,
) -> DecodeResult<Vec<(&'static str, Row)>> {
    let mut s = Sink {
        rows: Vec::new(),
        alloc,
    };
    for a in authorizations {
        let grantee = a.values.get("owner").cloned().unwrap_or(Value::Null);
        let grants = match a.values.get("grants") {
            Some(Value::Set(g)) => g.elements.iter().map(|e| &e.value).collect::<Vec<_>>(),
            Some(Value::Null) | None => Vec::new(),
            Some(v) => return Err(err(format!("_db_authorization {:?} grants {v:?}", a.oid))),
        };
        if !grants.len().is_multiple_of(GRANT_ENTRY_LENGTH) {
            return Err(err(format!(
                "_db_authorization {:?}: {} grant elements",
                a.oid,
                grants.len()
            )));
        }
        for g in grants.chunks(GRANT_ENTRY_LENGTH) {
            let (Value::Integer(obj_type), Value::Integer(cache)) = (g[0], g[3]) else {
                return Err(err(format!(
                    "_db_authorization {:?}: grant entry {g:?}",
                    a.oid
                )));
            };
            for bit in (0..AUTH_NAMES.len()).rev() {
                if cache & (1 << bit) == 0 {
                    continue;
                }
                let me = (s.alloc)("_db_auth");
                s.emit(
                    "_db_auth",
                    me,
                    vec![
                        ("grantor", g[2].clone()),
                        ("grantee", grantee.clone()),
                        ("object_type", int(*obj_type)),
                        ("object_of", g[1].clone()),
                        ("auth_type", text(AUTH_NAMES[bit])),
                        ("is_grantable", int((cache >> GRANT_SHIFT >> bit) & 1)),
                        ("created_time", now.clone()),
                        ("updated_time", now.clone()),
                    ],
                );
            }
        }
    }
    Ok(s.rows)
}
