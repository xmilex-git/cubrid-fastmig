//! A page sink over one plain file: page `n` of the file is VPID (`volid`, n). For building a B-tree outside a
//! database (workspace#493 acceptance run); the target database uses the volume sink of workspace#490.
//!
//! Input: pages from the builder. Output: the file, pages in VPID order.
//! Source: none from the engine; pages are what `PageSink` receives.

use crate::page_sink::PageSink;
use fastmig_ondisk::{Vfid, Vpid};
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicI32, Ordering};

pub struct ScratchFileSink {
    file: File,
    volid: i16,
    io_pagesize: usize,
    next: AtomicI32,
}

impl ScratchFileSink {
    pub fn create(path: &std::path::Path, volid: i16, io_pagesize: usize) -> io::Result<Self> {
        Ok(Self {
            file: File::options()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)?,
            volid,
            io_pagesize,
            next: AtomicI32::new(0),
        })
    }

    pub fn pages(&self) -> usize {
        self.next.load(Ordering::Relaxed) as usize
    }

    /// Reads back a page written earlier.
    pub fn read_page(&self, vpid: Vpid) -> io::Result<Box<[u8]>> {
        let mut buf = vec![0u8; self.io_pagesize].into_boxed_slice();
        self.file
            .read_exact_at(&mut buf, vpid.pageid as u64 * self.io_pagesize as u64)?;
        Ok(buf)
    }
}

impl PageSink for ScratchFileSink {
    fn io_pagesize(&self) -> usize {
        self.io_pagesize
    }

    fn vfid(&self) -> Vfid {
        Vfid::new(self.volid, 0)
    }

    fn alloc_page(&self) -> io::Result<Vpid> {
        Ok(Vpid::new(
            self.volid,
            self.next.fetch_add(1, Ordering::Relaxed),
        ))
    }

    fn write_page(&self, vpid: Vpid, page: &[u8]) -> io::Result<()> {
        self.file
            .write_all_at(page, vpid.pageid as u64 * self.io_pagesize as u64)
    }
}
