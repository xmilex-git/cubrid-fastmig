//! Writes the empty TDE metadata into a target whose source had none (10.2): the keyinfo heap with its record, the
//! `tde_keyinfo_hfid` of `BOOT_DB_PARM` (the 124 B record grows to 136 B), and the `<db>_keys` file.
//!
//! Input: a [`SpaceManager`] over the target and the `_keys` path next to its volume 0. Output: the three written.
//! Source: engine develop @682b080b2 `boot_sr.c:boot_create_all_volumes` (:5019 `xheap_create (.., NULL, false)`,
//! :5145 `tde_initialize`), `tde.c:tde_initialize`, `tde_create_keys_file` (mode 0600); the keyinfo heap of T1
//! fmdev_clean read back (class OID NULL, one bestspace shard, one 156 B home record, no MVCC header).
//! Without these develop boots but never loads its TDE module, and its vacuum master then dispatches no job at all
//! (`vacuum.c:3060`, CBRD-26176), so MVCC garbage is never reclaimed in CS mode (measured, workspace#515).
//! This is not TDE support (user decision 2026-10-30): no page is encrypted; that `ENCRYPT=` tables then work in the
//! target is the engine's own behavior.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::boot_parm::BootDbParm;
use crate::file_alloc::SpaceManager;
use crate::heap_rewrite::heap_writer::{HeapWriter, create_heap};
use crate::tde_keys::{TdeKeySet, wipe};
use crate::{Hfid, Oid};

/// Creates the keyinfo heap and its record, points `BOOT_DB_PARM` at it, and writes `keys_path` (new file, 0600).
/// `now` is the creation time the engine would take from `time(NULL)`. The keys are wiped from memory on return.
///
/// The heap must lie in volume 0: the server loads the keys right after mounting volume 0 and before the others
/// (`boot_restart_server`, boot_sr.c:2306-2335), so a keyinfo heap elsewhere fails the TDE load in release and the
/// `disk_is_page_sector_reserved` assert in debug (measured, workspace#515). Sectors are reserved volume 0 first, so
/// call this while volume 0 has a free sector (after the old indexes are destroyed: every source keeps its system
/// indexes there); otherwise the heap is destroyed again and an error returned.
pub fn create(space: &SpaceManager, keys_path: &Path, now: i64) -> io::Result<Hfid> {
    // class OID NULL like `xheap_create (.., NULL, ..)`; one shard, as the engine builds it (T1 fmdev_clean)
    let hfid = create_heap(space, Oid::NULL, true)?;
    if hfid.vfid.volid != 0 {
        space.destroy_file(hfid.vfid)?;
        return Err(io::Error::other(
            "TDE keyinfo heap: volume 0 has no free sector (the engine reads it before mounting other volumes)",
        ));
    }
    let keys = TdeKeySet::generate(now)?;
    let mut w = HeapWriter::open(space, hfid)?;
    w.insert(&keys.keyinfo_record(now), false)?;
    w.finish()?;
    BootDbParm::write_tde_keyinfo_hfid(space.volumes(), hfid)?;

    let mut bytes = keys.keys_file();
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(keys_path)
        .and_then(|mut f| {
            f.write_all(&bytes)?;
            f.sync_all()
        });
    wipe(&mut bytes);
    written.map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", keys_path.display())))?;
    Ok(hfid)
}
