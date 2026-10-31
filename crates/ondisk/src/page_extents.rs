//! Pages of many owners (heaps, B-trees) cut into extents in physical order, each read with one call.
//!
//! Input: every page to read with its owner tag, in any order, and the IO page size. Output: extents, ascending by
//! volume and page: runs of pages of one volume read with one `VolumeSet::read_pages_into`, a gap of up to
//! [`MAX_GAP_BYTES`] between two listed pages read through (one seek on a rotating disk costs more), at most
//! [`MAX_EXTENT_BYTES`] each. Workers that take the extents in order keep the disk reading forward: the heaps of
//! many classes and the leaves of many B-trees interleave in a volume, and reading them owner by owner, or in key
//! order, seeks between short runs (an HDD source read at a sixth of its sequential rate, workspace#596).
//! Source: workspace#596 (T31 workspace#561 round 1).

use std::io;

use crate::ids::Vpid;
use crate::volume_set::VolumeSet;

/// The longest run of unlisted pages an extent reads through: about a seek's worth of sequential reading.
pub const MAX_GAP_BYTES: usize = 1 << 20;

/// The largest extent: one read, one worker's buffer.
pub const MAX_EXTENT_BYTES: usize = 4 << 20;

/// Consecutive pages of one volume read together, and the listed ones among them.
pub struct Extent<T> {
    pub first: Vpid,
    pub npages: usize,
    /// (page position from `first`, owner tag), ascending; positions not listed are a gap read through.
    pub pages: Vec<(usize, T)>,
}

thread_local! {
    /// The calling worker's extent buffer, kept for its next extent: a new 4 MiB buffer per extent is a fresh mapping
    /// to fault in and unmap each time (ALLOC-01).
    static EXTENT_BUF: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl<T> Extent<T> {
    /// Reads the extent into the calling thread's buffer and passes it, or the read error, to `f`.
    pub fn read<R>(&self, vols: &VolumeSet, f: impl FnOnce(io::Result<&[u8]>) -> R) -> R {
        EXTENT_BUF.with_borrow_mut(|buf| {
            buf.resize(self.npages * vols.io_pagesize(), 0);
            match vols.read_pages_into(self.first, buf) {
                Ok(()) => f(Ok(buf)),
                Err(e) => f(Err(e)),
            }
        })
    }

    /// The listed pages as (VPID, page bytes of `buf`, tag); `buf` holds the extent as read.
    pub fn listed<'b>(
        &'b self,
        buf: &'b [u8],
        io_pagesize: usize,
    ) -> impl Iterator<Item = (Vpid, &'b [u8], &'b T)> {
        self.pages.iter().map(move |(at, tag)| {
            let vpid = Vpid::new(self.first.volid, self.first.pageid + *at as i32);
            (vpid, &buf[at * io_pagesize..(at + 1) * io_pagesize], tag)
        })
    }
}

/// `pages` cut into extents, ascending. A page listed twice is listed twice in its extent (each owner reads it).
pub fn extents<T>(mut pages: Vec<(Vpid, T)>, io_pagesize: usize) -> Vec<Extent<T>> {
    let max_gap = (MAX_GAP_BYTES / io_pagesize) as i64;
    let max_pages = (MAX_EXTENT_BYTES / io_pagesize).max(1) as i64;
    pages.sort_by_key(|(v, _)| *v);
    let mut out: Vec<Extent<T>> = Vec::new();
    for (v, tag) in pages {
        let joins = out.last().is_some_and(|e| {
            let last = e.first.pageid as i64 + e.npages as i64 - 1;
            e.first.volid == v.volid
                && v.pageid as i64 - last - 1 <= max_gap
                && (v.pageid as i64 - e.first.pageid as i64) < max_pages
        });
        if joins {
            let e = out.last_mut().unwrap();
            let at = (v.pageid - e.first.pageid) as usize;
            e.npages = e.npages.max(at + 1);
            e.pages.push((at, tag));
        } else {
            out.push(Extent {
                first: v,
                npages: 1,
                pages: vec![(0, tag)],
            });
        }
    }
    out
}
