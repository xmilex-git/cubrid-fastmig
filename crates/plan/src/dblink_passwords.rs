//! The DBLink server passwords an 11.2..11.4 source hands a develop target: each stored password re-wrapped from the
//! source's key into the form develop reads ([`fastmig_ondisk::dblink_password`]), so the moved server logs in with the
//! same password, as after the engine's official path (unloaddb writes it re-encrypted, loaddb stores it again).
//! A password no key opens is stored empty; the plan warns and names its `ALTER SERVER` in post-migration-fixes.sql
//! without a value. No password, key or re-wrapped string is printed anywhere.
//!
//! Input: the source volumes, codec line and classes, its `<db>_keys` file. Output: [`ServerPasswords`].
//! Source: workspace#556 (11.2..11.4 store the password under the TDE log key, develop cannot read it: optdebug csql
//! asserts in `crypt_dblink_decrypt` on a moved server; decision 2026-10-30: re-wrap, warn when no key opens it).
//! Not TDE support: an encrypted source is still refused (workspace#510).

use std::collections::HashMap;
use std::io;
use std::path::Path;

use fastmig_catalog::class_rows::Row;
use fastmig_catalog::system_rows::{server, user};
use fastmig_ondisk::boot_parm::BootDbParm;
use fastmig_ondisk::dblink_password::{Rewrap, to_develop};
use fastmig_ondisk::record::heap_records;
use fastmig_ondisk::tde_keys::{LogKey, wipe};
use fastmig_ondisk::volume_set::VolumeSet;
use fastmig_ondisk::{Oid, VersionLine};

#[derive(Default)]
pub struct ServerPasswords {
    /// `_db_server` row → its develop password value (empty for one no key opens). Rows kept as is are absent.
    pub values: HashMap<Oid, Vec<u8>>,
    pub warning: Option<String>,
    /// The post-migration-fixes.sql section; empty when every password moved.
    pub fix_sql: String,
}

/// The source's log key: `None` without a `_keys` file or keyinfo record, or when they do not match.
fn source_log_key(vs: &VolumeSet, keys_file: &Path) -> io::Result<Option<LogKey>> {
    let mut keys = match std::fs::read(keys_file) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let (_, boot) = BootDbParm::read(vs)?;
    let key = if boot.tde_keyinfo_hfid.is_null() {
        None
    } else {
        match heap_records(vs, boot.tde_keyinfo_hfid).next() {
            Some(r) => LogKey::read(&keys, &r?.bytes),
            None => None,
        }
    };
    wipe(&mut keys);
    Ok(key)
}

/// `servers` and `users` are the source's `_db_server` and `db_user` rows; `keys_file` its `<db>_keys`.
pub fn server_passwords(
    vs: &VolumeSet,
    target: VersionLine,
    servers: &[Row],
    users: &[Row],
    keys_file: &Path,
) -> io::Result<ServerPasswords> {
    let mut out = ServerPasswords::default();
    if target != VersionLine::Develop || servers.is_empty() {
        return Ok(out);
    }
    let log_key = source_log_key(vs, keys_file)?;
    let zero = LogKey::zero();
    let keys: Vec<&LogKey> = log_key.iter().chain([&zero]).collect();
    let mut unreadable = Vec::new();
    for r in servers {
        let Some(stored) = server::password(&r.values) else {
            continue;
        };
        match to_develop(stored, &keys)? {
            Rewrap::Kept => {}
            Rewrap::Rewrapped(v) => {
                out.values.insert(r.oid, v);
            }
            Rewrap::Unreadable => {
                out.values.insert(r.oid, Vec::new());
                let owner = server::owner(&r.values)
                    .and_then(|o| users.iter().find(|u| u.oid == o))
                    .and_then(|u| user::name(&u.values))
                    .unwrap_or_default();
                let name = server::link_name(&r.values).unwrap_or_default();
                unreadable.push((
                    String::from_utf8_lossy(owner).to_lowercase(),
                    String::from_utf8_lossy(name).into_owned(),
                ));
            }
        }
    }
    if unreadable.is_empty() {
        return Ok(out);
    }
    out.warning = Some(format!(
        "{} DBLink servers ({}) have a stored password neither the source's TDE log key ({}) nor the zero key opens; \
         the target stores it empty: set it with the ALTER SERVER statements of fastmig/post-migration-fixes.sql \
         (workspace#556)",
        unreadable.len(),
        unreadable
            .iter()
            .map(|(o, n)| format!("{o}.{n}"))
            .collect::<Vec<_>>()
            .join(", "),
        if log_key.is_some() {
            "read from its _keys file"
        } else {
            "no _keys file or keyinfo record"
        }
    ));
    out.fix_sql = String::from(
        "-- DBLink servers whose stored password could not be moved (workspace#556): fill in each password, then run.\n",
    );
    for (owner, name) in unreadable {
        out.fix_sql.push_str(&format!(
            "-- ALTER SERVER [{owner}].[{name}] CHANGE PASSWORD = '<password>';\n"
        ));
    }
    Ok(out)
}
