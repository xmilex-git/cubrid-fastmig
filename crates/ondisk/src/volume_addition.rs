//! Volume addition: a new permanent data volume `<db>_x<NNN>` formatted next to the others and linked at the end
//! of the volume chain.
//!
//! Input: the volume set (read-write), the new volume's size and maximum in sectors, where it goes.
//! Output: the volume file (every page written as unwritten, then the header and the STAB with its system sectors
//! set), the previous last volume's header linked to it, a `_vinf` line, `BOOT_DB_PARM` `nvols`/`last_volid`
//! raised, the volume attached to the set; its volid and free sectors.
//! New pages carry a NULL LSA, like every page this tool writes (G2, workspace#482); `db_creation`, `chkpt_lsa`
//! and the charset come from volume 0, as the engine takes them from the log and the database.
//! Source: engine `src/storage/disk_manager.c:disk_add_volume` (:2117-2324), `disk_format` (:512-820),
//! `disk_volume_header_set_stab`, `disk_stab_init`, `disk_set_link`; `boot_sr.c:boot_get_new_volume_name_and_id`,
//! `boot_dbparm_save_volume`; `log_page_buffer.c:logpb_add_volume`; `file_io.c:fileio_format`,
//! `fileio_make_volume_ext_name` (develop 682b080b2 = release/11.4 for these functions). Decision: grow a target
//! like the engine's own auto-extension (workspace#500, ADR 0013).

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::boot_parm::BootDbParm;
use crate::ids::Vpid;
use crate::page::{self, PageType};
use crate::sector_table::{self, SECTOR_NPAGES};
use crate::volume_header::{self, NewVolume};
use crate::volume_set::{self, VolumeSet};

/// Engine `disk_add_volume` comment for an automatic volume.
const REMARKS: &str = "Automatic Volume Extension";
/// Engine `LOG_MAX_DBVOLID`: the highest permanent volid.
const MAX_VOLID: i16 = i16::MAX - 1;

/// Where added volumes go and which `_vinf` names them.
#[derive(Clone, Debug)]
pub struct VolumePlace {
    /// Engine `io_volume_ext_path`, else the directory of volume 0.
    pub dir: PathBuf,
    /// The database name (volume 0's file name).
    pub db_name: String,
    /// The `_vinf` file to append to; `None` when the caller writes `_vinf` from the final volume list.
    pub vinf: Option<PathBuf>,
}

impl VolumePlace {
    /// Next to volume 0, under volume 0's file name, no `_vinf`.
    pub fn beside_first(vols: &VolumeSet) -> io::Result<Self> {
        let p = vols.volume(0)?.path.clone();
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| io::Error::other(format!("{}: no file name", p.display())))?;
        Ok(VolumePlace {
            dir: p.parent().unwrap_or(Path::new(".")).to_path_buf(),
            db_name: name.to_string(),
            vinf: None,
        })
    }
}

/// A volume just added.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddedVolume {
    pub volid: i16,
    /// Sectors it has free (its system sectors excluded).
    pub free_sectors: i32,
}

/// Engine `disk_add_volume` + `disk_format` of a permanent data volume of `nsect_total` sectors that may grow to
/// `nsect_max` (both rounded up to a STAB unit). Refuses an existing file.
pub fn add_volume(
    vols: &VolumeSet,
    nsect_total: i32,
    nsect_max: i32,
    place: &VolumePlace,
) -> io::Result<AddedVolume> {
    let io_size = vols.io_pagesize();
    let nsect_total = sector_table::round_up_sectors(nsect_total);
    let nsect_max = sector_table::round_up_sectors(nsect_max);
    let volid = vols.nvols() as i16;
    let (_, parm) = BootDbParm::read(vols)?;
    if volid > MAX_VOLID || parm.last_volid + 1 != volid || parm.nvols != volid {
        return Err(io::Error::other(format!(
            "new volume {volid}: BOOT_DB_PARM has nvols {} last_volid {}",
            parm.nvols, parm.last_volid
        )));
    }
    // the engine records full names (boot_Db_full_name's directory is absolute)
    let path = place
        .dir
        .canonicalize()?
        .join(format!("{}_x{:03}", place.db_name, volid));
    let fullname = path
        .to_str()
        .ok_or_else(|| io::Error::other("volume path is not UTF-8"))?;
    let v0 = vols.volume(0)?.header.clone();
    let stab_npages = sector_table::stab_npages(io_size, nsect_max);
    let nsys = sector_table::sys_nsect(io_size, nsect_max);
    if nsys >= nsect_total {
        return Err(io::Error::other(format!(
            "new volume of {nsect_total} sectors has no room past its {nsys} system sectors"
        )));
    }

    // shortcut: no partition free-space precheck (engine fileio_get_number_of_partition_free_sectors); a full disk
    // fails on write instead, upgrade if an early refusal is wanted (ADR 0013).
    // fileio_format with sweep: every page written as unwritten
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
    let npages = nsect_total as u64 * SECTOR_NPAGES as u64;
    volume_set::write_unwritten_pages(&file, io_size, 0, npages)?;

    let mut hp = vec![0u8; io_size];
    page::init_header(&mut hp, Vpid::new(volid, 0), PageType::VolHeader);
    volume_header::format(
        &mut hp,
        &NewVolume {
            volid,
            db_charset: v0.db_charset,
            nsect_total,
            nsect_max,
            stab_npages,
            db_creation: v0.db_creation,
            vol_creation: now_secs(),
            chkpt_lsa: v0.chkpt_lsa,
            vol_fullname: fullname,
            vol_remarks: REMARKS,
        },
    )?;
    file.write_all_at(&hp, 0)?;
    // disk_stab_init: zeroed bitmap pages, the system sectors set in the first
    let vh = volume_header::VolumeHeader::read(&hp)?;
    for i in 0..stab_npages {
        let pageid = 1 + i;
        let mut sp = vec![0u8; io_size];
        page::init_header(&mut sp, Vpid::new(volid, pageid), PageType::VolBitmap);
        if i == 0 {
            for s in 0..nsys {
                let at = sector_table::bit_position(&vh, io_size, s);
                sector_table::set_bit(&mut sp, at, true);
            }
        }
        file.write_all_at(&sp, pageid as u64 * io_size as u64)?;
    }
    drop(file);
    vols.attach(&path)?;

    // disk_set_link on the previous last volume
    let prev = Vpid::new(volid - 1, 0);
    let mut pp = vols.read_page(prev)?;
    volume_header::set_link(&mut pp, volid, fullname)?;
    vols.write_page(prev, &pp)?;
    vols.reload_header(volid - 1)?;

    if let Some(vinf) = &place.vinf {
        let mut f = OpenOptions::new().append(true).open(vinf)?;
        writeln!(f, "{volid:4} {fullname}")?;
    }
    BootDbParm::write_volume_counts(vols, volid + 1, volid)?;
    Ok(AddedVolume {
        volid,
        free_sectors: nsect_total - nsys,
    })
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
