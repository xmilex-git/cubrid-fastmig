//! Volume expansion: a permanent data volume grows by whole STAB units of sectors, up to its `nsect_max`.
//!
//! Input: the volume set (read-write), a volid and the sectors wanted. Output: the volume file grown with
//! unwritten pages, `nsect_total` raised in its header, the set's copy of the header reloaded; the sectors added.
//! The STAB already covers `nsect_max` (it is sized at format), so no bitmap page changes.
//! Source: engine `src/storage/disk_manager.c:disk_volume_expand` (:1904-2020), `file_io.c:fileio_expand_to`
//! (develop 682b080b2 = release/11.4 for these functions). Decision: grow a target like the engine's own
//! auto-extension (workspace#500, ADR 0013).

use std::io;

use crate::ids::Vpid;
use crate::sector_table::{SECTOR_NPAGES, round_up_sectors};
use crate::volume_header;
use crate::volume_set::VolumeSet;

/// Engine `disk_volume_expand`: adds `nsect` sectors rounded up to a STAB unit; returns the sectors added.
/// Refuses growth past `nsect_max`.
pub fn expand_volume(vols: &VolumeSet, volid: i16, nsect: i32) -> io::Result<i32> {
    let nsect = round_up_sectors(nsect);
    let vh = vols.volume(volid)?.header.clone();
    let total = vh.nsect_total + nsect;
    if total > vh.nsect_max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "volume {volid}: {} + {nsect} sectors exceeds nsect_max {}",
                vh.nsect_total, vh.nsect_max
            ),
        ));
    }
    // the file grows before the header names the new pages
    vols.extend_file(volid, total as i64 * SECTOR_NPAGES as i64)?;
    let hv = Vpid::new(volid, 0);
    let mut p = vols.read_page(hv)?;
    volume_header::set_nsect_total(&mut p, total);
    vols.write_page(hv, &p)?;
    vols.reload_header(volid)?;
    Ok(nsect)
}
