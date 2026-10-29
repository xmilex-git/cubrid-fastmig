//! The permanent volumes of one database opened together: page reads and writes addressed by VPID.
//!
//! Input: the first volume's path (or a databases.txt entry) and how to find the next volumes. Output: a
//! [`VolumeSet`] whose `read_page`/`write_page` take `&self` (positioned I/O), so threads share it freely.
//! Source: volumes are enumerated by the header chain (`next_volid`, `next_vol_fullname`), not by `_vinf`
//! (G1, workspace#481); engine `disk_manager.c` volume header, `boot_find_rest_permanent_volumes`.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::databases_txt::DatabaseEntry;
use crate::ids::Vpid;
use crate::page;
use crate::volume_header::VolumeHeader;

/// How to find volume n+1 from volume n's header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Locate {
    /// The path recorded in `next_vol_fullname`.
    Recorded,
    /// The recorded file name, in the first volume's directory: for a database copied to another directory whose
    /// headers still carry the old paths (reflink copies of the T1 pristine databases, a target before
    /// `set_paths`).
    FirstVolumeDir,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    ReadOnly,
    ReadWrite,
}

/// One open volume.
#[derive(Debug)]
pub struct Volume {
    pub volid: i16,
    pub path: PathBuf,
    /// The header as read at open time.
    pub header: VolumeHeader,
    file: File,
}

/// The permanent volumes of a database, indexed by volid (0..n, contiguous as the engine assigns them).
#[derive(Debug)]
pub struct VolumeSet {
    io_pagesize: usize,
    volumes: Vec<Volume>,
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

impl VolumeSet {
    /// Opens the database registered as `entry` (first volume `<vol_path>/<name>`).
    pub fn open_entry(entry: &DatabaseEntry, locate: Locate, access: Access) -> io::Result<Self> {
        Self::open(&entry.first_volume(), locate, access)
    }

    /// Opens volume 0 at `first` and follows the header chain. Checks every volume's magic, its volid against its
    /// place in the chain, and its page size against volume 0's.
    pub fn open(first: &Path, locate: Locate, access: Access) -> io::Result<Self> {
        let dir = first.parent().unwrap_or(Path::new(".")).to_path_buf();
        let mut volumes: Vec<Volume> = Vec::new();
        let mut path = first.to_path_buf();
        let mut io_pagesize = 0usize;
        loop {
            let file = OpenOptions::new()
                .read(true)
                .write(access == Access::ReadWrite)
                .open(&path)
                .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
            // the volume header fits in the smallest page size (4 KiB); read that much, then the real size
            let mut head = vec![0u8; 4096];
            file.read_exact_at(&mut head, 0)?;
            let probe = VolumeHeader::read(&head)?;
            let size = probe.iopagesize as usize;
            if !(4096..=65536).contains(&size) || !size.is_power_of_two() {
                return Err(invalid(format!("{}: iopagesize {size}", path.display())));
            }
            let mut p0 = vec![0u8; size];
            file.read_exact_at(&mut p0, 0)?;
            let header = VolumeHeader::read(&p0)?;
            if volumes.is_empty() {
                io_pagesize = size;
            } else if size != io_pagesize {
                return Err(invalid(format!(
                    "{}: iopagesize {size} != {io_pagesize}",
                    path.display()
                )));
            }
            if header.volid as usize != volumes.len() {
                return Err(invalid(format!(
                    "{}: volid {} where {} expected",
                    path.display(),
                    header.volid,
                    volumes.len()
                )));
            }
            let next = (header.next_volid >= 0).then(|| PathBuf::from(&header.next_vol_fullname));
            volumes.push(Volume {
                volid: header.volid,
                path: path.clone(),
                header,
                file,
            });
            match next {
                None => break,
                Some(n) => {
                    path = match locate {
                        Locate::Recorded => n,
                        Locate::FirstVolumeDir => dir.join(n.file_name().ok_or_else(|| {
                            invalid(format!(
                                "next_vol_fullname {} has no file name",
                                n.display()
                            ))
                        })?),
                    }
                }
            }
        }
        Ok(VolumeSet {
            io_pagesize,
            volumes,
        })
    }

    /// `IO_PAGESIZE` of this database (every page buffer passed in or out has this length).
    pub fn io_pagesize(&self) -> usize {
        self.io_pagesize
    }

    pub fn volumes(&self) -> &[Volume] {
        &self.volumes
    }

    pub fn volume(&self, volid: i16) -> io::Result<&Volume> {
        usize::try_from(volid)
            .ok()
            .and_then(|i| self.volumes.get(i))
            .ok_or_else(|| invalid(format!("no volume {volid}")))
    }

    fn offset(&self, vpid: Vpid) -> io::Result<(&Volume, u64)> {
        let v = self.volume(vpid.volid)?;
        let npages = v.header.nsect_total as i64 * v.header.sect_npgs as i64;
        if vpid.pageid < 0 || vpid.pageid as i64 >= npages {
            return Err(invalid(format!(
                "page {}|{} outside the volume",
                vpid.volid, vpid.pageid
            )));
        }
        Ok((v, vpid.pageid as u64 * self.io_pagesize as u64))
    }

    /// Reads page `vpid` into `buf` (length [`Self::io_pagesize`]).
    pub fn read_page_into(&self, vpid: Vpid, buf: &mut [u8]) -> io::Result<()> {
        let (v, off) = self.offset(vpid)?;
        v.file.read_exact_at(buf, off)
    }

    /// Reads page `vpid` into a new buffer.
    pub fn read_page(&self, vpid: Vpid) -> io::Result<Box<[u8]>> {
        let mut buf = vec![0u8; self.io_pagesize].into_boxed_slice();
        self.read_page_into(vpid, &mut buf)?;
        Ok(buf)
    }

    /// Reads page `vpid` and checks that it is sane (header LSA = watermark) and that its header names `vpid`.
    /// Pages never written since the volume was formatted fail the second check; use [`Self::read_page`] for them.
    pub fn read_page_checked(&self, vpid: Vpid) -> io::Result<Box<[u8]>> {
        let p = self.read_page(vpid)?;
        if !page::is_sane(&p) {
            return Err(invalid(format!(
                "page {}|{}: LSA != watermark",
                vpid.volid, vpid.pageid
            )));
        }
        if page::vpid(&p) != vpid {
            let got = page::vpid(&p);
            return Err(invalid(format!(
                "page {}|{} claims {}|{}",
                vpid.volid, vpid.pageid, got.volid, got.pageid
            )));
        }
        Ok(p)
    }

    /// Writes page `vpid` (opened [`Access::ReadWrite`] only). No fsync; the caller syncs with [`Self::sync`].
    pub fn write_page(&self, vpid: Vpid, buf: &[u8]) -> io::Result<()> {
        assert_eq!(buf.len(), self.io_pagesize, "page buffer of the wrong size");
        let (v, off) = self.offset(vpid)?;
        v.file.write_all_at(buf, off)
    }

    /// fsyncs every volume.
    pub fn sync(&self) -> io::Result<()> {
        self.volumes.iter().try_for_each(|v| v.file.sync_all())
    }
}
