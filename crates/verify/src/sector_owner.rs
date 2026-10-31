//! Check 4: every sector of the permanent volumes belongs to exactly one file or to the volume's system area, and a
//! sector is marked in its volume's sector table exactly when it does.
//!
//! Input: the database (its file tracker from `BOOT_DB_PARM.trk_vfid`, every tracked file's table, each volume's
//! STAB). Output: check 4 defects — `sector-shared` (two files reserve it), `sector-leaked` (STAB bit set, no owner),
//! `sector-unmarked` (owned, bit clear), `unallocated-page-header` (an unallocated page of a file's sector names
//! neither NULL nor itself, workspace#591), `page-unreadable`, `file-header` (a tracked file's header or tables
//! inconsistent, `FileTable::check`), `file-unreadable`; and the tracked files for the catalog check.
//! O(s log s) in the sector count, plus a 32 B read per unallocated page of an owned sector.
//! Source: G3 workspace#483 Q8; engine `file_manager.c` (`file_header_sanity_check`, file tracker), `disk_manager.c`
//! STAB (ondisk `sector_table`, workspace#490); `file_io.c:fileio_initialize_res` and
//! `page_buffer.c:pgbuf_set_bcb_page_vpid` for the unallocated-page header (develop @682b080b2, workspace#591).

use std::collections::BTreeMap;

use fastmig_ondisk::file_header::FileType;
use fastmig_ondisk::file_table::{FileSector, FileTable};
use fastmig_ondisk::file_tracker::{self, TrackItem};
use fastmig_ondisk::ids::{NULL_ID, NULL_VOLID};
use fastmig_ondisk::page::{self, IO_RESERVED_SIZE};
use fastmig_ondisk::sector_table::{SECTOR_NPAGES, system_sectors, used_sectors};
use fastmig_ondisk::{Vfid, Vpid, Vsid};

use crate::database::Database;
use crate::defect::{Check, Defect};

/// The tracked files whose tables could be read, by VFID.
pub type Files = BTreeMap<Vfid, (TrackItem, FileTable)>;

fn vfid_text(v: Vfid) -> String {
    format!("file {}|{}", v.volid, v.fileid)
}

/// The unallocated pages of a file's sector whose header names neither NULL (never written, `unwritten_page`) nor
/// the page itself: the engine asserts on such a page when it first fixes it (`pgbuf_set_bcb_page_vpid`), e.g. after a
/// crash, in recovery redo (workspace#591). Reads the 32 B header of each.
/// shortcut: the pages of free sectors are not read (a read per free page, up to a volume's free space); the run
/// writes only headers of the engine's format there (`preallocate::unallocated_pages`); read them if that changes.
fn unallocated_headers(db: &Database, s: &FileSector) -> Vec<Defect> {
    let base = s.first_page();
    let null = Vpid::new(NULL_VOLID, NULL_ID);
    let text = |v: Vpid| format!("{}|{}", v.volid, v.pageid);
    let mut head = [0u8; IO_RESERVED_SIZE];
    (0..SECTOR_NPAGES)
        .filter(|i| s.page_bitmap >> i & 1 == 0)
        .map(|i| Vpid::new(base.volid, base.pageid + i))
        .filter_map(|p| {
            let got = match db.vols.read_page_into(p, &mut head) {
                Ok(()) => page::vpid(&head),
                Err(e) => {
                    return Some(
                        Defect::new(Check::Sector, "page-unreadable")
                            .object(text(p))
                            .actual(e),
                    );
                }
            };
            (got != p && got != null).then(|| {
                Defect::new(Check::Sector, "unallocated-page-header")
                    .object(text(p))
                    .expected(format!("{} or {}", text(p), text(null)))
                    .actual(text(got))
            })
        })
        .collect()
}

pub fn check_sectors(db: &Database) -> std::io::Result<(Files, Vec<Defect>)> {
    let mut out = Vec::new();
    let (mut items, _) = file_tracker::read(&db.vols, db.boot.trk_vfid)?;
    // the tracker does not list itself (T1: its sector 0|1 is reserved by no tracked file)
    if !items.iter().any(|i| i.vfid == db.boot.trk_vfid) {
        items.push(TrackItem {
            vfid: db.boot.trk_vfid,
            file_type_raw: FileType::Tracker as i16,
            metadata: 0,
        });
    }
    let mut files = Files::new();
    let mut owner: BTreeMap<Vsid, Vfid> = BTreeMap::new();
    for it in items {
        let ft = match FileTable::read(&db.vols, it.vfid) {
            Ok(ft) => ft,
            Err(e) => {
                out.push(
                    Defect::new(Check::Sector, "file-unreadable")
                        .object(vfid_text(it.vfid))
                        .actual(e),
                );
                continue;
            }
        };
        for bad in ft.check() {
            out.push(
                Defect::new(Check::Sector, "file-header")
                    .object(vfid_text(it.vfid))
                    .actual(bad),
            );
        }
        for s in &ft.sectors {
            out.extend(unallocated_headers(db, s));
            if let Some(prev) = owner.insert(s.vsid, it.vfid) {
                out.push(
                    Defect::new(Check::Sector, "sector-shared")
                        .object(format!("sector {}|{}", s.vsid.volid, s.vsid.sectid))
                        .actual(format!("{} and {}", vfid_text(prev), vfid_text(it.vfid))),
                );
            }
        }
        files.insert(it.vfid, (it, ft));
    }
    for v in db.vols.volumes() {
        let sys = system_sectors(&v.header);
        let marked = used_sectors(&db.vols, v.volid)?;
        let owned: Vec<i32> = owner
            .range(Vsid::new(v.volid, i32::MIN)..=Vsid::new(v.volid, i32::MAX))
            .map(|(s, _)| s.sectid)
            .collect();
        let (mut i, mut j) = (0, 0);
        while i < marked.len() || j < owned.len() {
            let sect = |s: i32| format!("sector {}|{s}", v.volid);
            match (marked.get(i), owned.get(j)) {
                (Some(a), Some(b)) if a == b => {
                    i += 1;
                    j += 1;
                }
                (Some(a), b) if b.is_none_or(|b| a < b) => {
                    if !sys.contains(a) {
                        out.push(Defect::new(Check::Sector, "sector-leaked").object(sect(*a)));
                    }
                    i += 1;
                }
                (_, Some(b)) => {
                    out.push(Defect::new(Check::Sector, "sector-unmarked").object(sect(*b)));
                    j += 1;
                }
                _ => unreachable!(),
            }
        }
    }
    Ok((files, out))
}
