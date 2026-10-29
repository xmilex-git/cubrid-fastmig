//! Byte copy of one range of a volume, with the page treatment applied inline: the copy method copy's single pass reads a
//! chunk of whole pages, lets the page treatment patch them in the buffer, and writes the buffer to the target.
//!
//! Input: the source and target files, the byte range (whole IO pages), a reusable buffer and the treatment.
//! Output: the target range written. The source is never written.
//! Source: decision G1 workspace#481 (copy treats pages inline in the copying pass; clone patches the target in
//! place afterwards, outside this module).

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;

/// A page treatment applied by the copy method copy to pages on their way to the target.
/// (The page treatment grade itself is `fastmig_ondisk::PageTreatment`; this trait is the function that applies it
/// to a chunk inside the copy pass.)
pub trait InlineTreatment: Sync {
    /// `pages` holds whole IO pages of volume `volid`; the first one is page `first_pageid`.
    fn patch(
        &self,
        volid: i16,
        first_pageid: i32,
        io_pagesize: usize,
        pages: &mut [u8],
    ) -> io::Result<()>;
}

/// The "as is" treatment: pages are copied byte for byte.
pub struct AsIs;

impl InlineTreatment for AsIs {
    fn patch(&self, _: i16, _: i32, _: usize, _: &mut [u8]) -> io::Result<()> {
        Ok(())
    }
}

/// One range of one volume to copy.
pub struct CopyRange<'a> {
    pub src: &'a File,
    pub dst: &'a File,
    pub volid: i16,
    pub io_pagesize: usize,
    pub start: u64,
    pub end: u64,
}

/// Copies `range` through `buf` (a multiple of the IO page size), patching each chunk with `treatment`.
pub fn copy_range(
    range: &CopyRange,
    buf: &mut [u8],
    treatment: &dyn InlineTreatment,
) -> io::Result<()> {
    let mut pos = range.start;
    while pos < range.end {
        let len = buf.len().min((range.end - pos) as usize);
        let chunk = &mut buf[..len];
        range.src.read_exact_at(chunk, pos)?;
        let first_pageid = (pos / range.io_pagesize as u64) as i32;
        treatment.patch(range.volid, first_pageid, range.io_pagesize, chunk)?;
        range.dst.write_all_at(chunk, pos)?;
        pos += len as u64;
    }
    Ok(())
}
