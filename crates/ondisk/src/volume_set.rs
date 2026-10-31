//! The permanent volumes of one database opened together: page reads and writes addressed by VPID.
//!
//! Input: the first volume's path (or a databases.txt entry) and how to find the next volumes. Output: a
//! [`VolumeSet`] whose `read_page`/`write_page` take `&self` (positioned I/O), so threads share it freely.
//! The set grows in place when the space manager expands or adds a volume (workspace#500): the volume list sits
//! behind a lock and [`VolumeSet::volume`] hands out a shared snapshot of one volume.
//! Source: volumes are enumerated by the header chain (`next_volid`, `next_vol_fullname`), not by `_vinf`
//! (G1, workspace#481); engine `disk_manager.c` volume header, `boot_find_rest_permanent_volumes`;
//! `file_io.c:fileio_expand_to`, `fileio_initialize_res` (the bytes of a page no one has written yet).

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::databases_txt::DatabaseEntry;
use crate::ids::Vpid;
use crate::lsa::Lsa;
use crate::page::{self, IO_TRAILER_SIZE};
use crate::source_version::HeaderLayout;
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
    access: Access,
    /// The header layout of every volume in the set (a 10.2..11.3 source is opened read-only in its own).
    layout: HeaderLayout,
    volumes: RwLock<Vec<Arc<Volume>>>,
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

impl VolumeSet {
    /// Opens the database registered as `entry` (first volume `<vol_path>/<name>`).
    pub fn open_entry(
        entry: &DatabaseEntry,
        locate: Locate,
        access: Access,
        layout: HeaderLayout,
    ) -> io::Result<Self> {
        Self::open(&entry.first_volume(), locate, access, layout)
    }

    /// Opens volume 0 at `first` and follows the header chain. Checks every volume's magic, its volid against its
    /// place in the chain, and its page size against volume 0's. Headers are read in `layout`: the source version's
    /// ([`crate::SourceVersion::header_layout`]); a target is always [`HeaderLayout::Since11_4`].
    pub fn open(
        first: &Path,
        locate: Locate,
        access: Access,
        layout: HeaderLayout,
    ) -> io::Result<Self> {
        let dir = first.parent().unwrap_or(Path::new(".")).to_path_buf();
        let mut volumes: Vec<Arc<Volume>> = Vec::new();
        let mut path = first.to_path_buf();
        let mut io_pagesize = 0usize;
        loop {
            let (file, header) = open_volume(&path, access, layout)?;
            let size = header.iopagesize as usize;
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
            volumes.push(Arc::new(Volume {
                volid: header.volid,
                path: path.clone(),
                header,
                file,
            }));
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
            access,
            layout,
            volumes: RwLock::new(volumes),
        })
    }

    /// `IO_PAGESIZE` of this database (every page buffer passed in or out has this length).
    pub fn io_pagesize(&self) -> usize {
        self.io_pagesize
    }

    /// A snapshot of the volumes in volid order.
    pub fn volumes(&self) -> Vec<Arc<Volume>> {
        self.volumes.read().unwrap().clone()
    }

    pub fn nvols(&self) -> usize {
        self.volumes.read().unwrap().len()
    }

    /// A snapshot of one volume (its header as of the last change this process made).
    pub fn volume(&self, volid: i16) -> io::Result<Arc<Volume>> {
        usize::try_from(volid)
            .ok()
            .and_then(|i| self.volumes.read().unwrap().get(i).cloned())
            .ok_or_else(|| invalid(format!("no volume {volid}")))
    }

    fn offset(&self, vpid: Vpid) -> io::Result<(Arc<Volume>, u64)> {
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

    /// Reads page `vpid` into `buf` (length [`Self::io_pagesize`], or less for a prefix such as the header).
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
        self.volumes().iter().try_for_each(|v| v.file.sync_all())
    }

    /// Grows volume `volid`'s file to `npages` pages, the new pages as `fileio_expand_to` writes them
    /// ([`unwritten_page`]). The header is not touched; see [`Self::reload_header`].
    pub(crate) fn extend_file(&self, volid: i16, npages: i64) -> io::Result<()> {
        let v = self.volume(volid)?;
        let have = v.file.metadata()?.len() / self.io_pagesize as u64;
        write_unwritten_pages(&v.file, self.io_pagesize, have, npages as u64)
    }

    /// Re-reads volume `volid`'s header after this process rewrote page 0 (a new `nsect_total`, a new link).
    pub(crate) fn reload_header(&self, volid: i16) -> io::Result<()> {
        let mut p0 = vec![0u8; self.io_pagesize];
        let v = self.volume(volid)?;
        v.file.read_exact_at(&mut p0, 0)?;
        let header = VolumeHeader::read_layout(&p0, self.layout)?;
        let fresh = Arc::new(Volume {
            volid,
            path: v.path.clone(),
            header,
            file: v.file.try_clone()?,
        });
        self.volumes.write().unwrap()[volid as usize] = fresh;
        Ok(())
    }

    /// Opens the volume just formatted at `path` and appends it; its volid must be the next one.
    pub(crate) fn attach(&self, path: &Path) -> io::Result<i16> {
        let (file, header) = open_volume(path, self.access, self.layout)?;
        let mut vols = self.volumes.write().unwrap();
        if header.volid as usize != vols.len() || header.iopagesize as usize != self.io_pagesize {
            return Err(invalid(format!(
                "{}: volid {} / iopagesize {} does not continue the set",
                path.display(),
                header.volid,
                header.iopagesize
            )));
        }
        vols.push(Arc::new(Volume {
            volid: header.volid,
            path: path.to_path_buf(),
            header,
            file,
        }));
        Ok(vols.len() as i16 - 1)
    }
}

/// Opens one volume file and reads its header at the page size the header names.
fn open_volume(
    path: &Path,
    access: Access,
    layout: HeaderLayout,
) -> io::Result<(File, VolumeHeader)> {
    let file = OpenOptions::new()
        .read(true)
        .write(access == Access::ReadWrite)
        .open(path)
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
    // the volume header fits in the smallest page size (4 KiB); read that much, then the real size
    let mut head = vec![0u8; 4096];
    file.read_exact_at(&mut head, 0)?;
    let size = VolumeHeader::read_layout(&head, layout)?.iopagesize as usize;
    if !(4096..=65536).contains(&size) || !size.is_power_of_two() {
        return Err(invalid(format!("{}: iopagesize {size}", path.display())));
    }
    let mut p0 = vec![0u8; size];
    file.read_exact_at(&mut p0, 0)?;
    Ok((file, VolumeHeader::read_layout(&p0, layout)?))
}

/// Engine `fileio_initialize_res`: the bytes of a volume page formatted but never written — zero, NULL LSA in the
/// header and the trailer, pageid -1, volid -1.
pub fn unwritten_page(io_pagesize: usize) -> Vec<u8> {
    let mut p = vec![0u8; io_pagesize];
    let r = page::reserved_mut(&mut p);
    r.lsa.set(Lsa::NULL);
    r.pageid.set(-1);
    r.volid.set(-1);
    Lsa::NULL.write(&mut p, io_pagesize - IO_TRAILER_SIZE);
    p
}

/// Writes [`unwritten_page`] over pages `from..to` of `file` (engine `fileio_initialize_pages`), 1 MiB per write.
pub(crate) fn write_unwritten_pages(
    file: &File,
    io_pagesize: usize,
    from: u64,
    to: u64,
) -> io::Result<()> {
    let per = (1 << 20) / io_pagesize as u64;
    let buf = unwritten_page(io_pagesize).repeat(per as usize);
    let mut p = from;
    while p < to {
        let n = per.min(to - p);
        file.write_all_at(&buf[..(n as usize) * io_pagesize], p * io_pagesize as u64)?;
        p += n;
    }
    Ok(())
}
