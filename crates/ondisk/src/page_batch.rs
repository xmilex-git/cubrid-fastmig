//! The pages one space operation edits, held in memory until the operation ends.
//!
//! Input: the volume set. Output: page buffers read on first use (or started blank), written back together by
//! [`PageBatch::flush`]. A file header page that one engine step edits several times (alloc, then move to the full
//! table, then a nested table page alloc) is read once and written once.
//! Source: the role the engine's page buffer plays inside one system operation of `file_manager.c`.

use std::collections::HashMap;
use std::io;

use crate::ids::Vpid;
use crate::volume_set::VolumeSet;

pub struct PageBatch<'v> {
    vols: &'v VolumeSet,
    pages: HashMap<Vpid, Box<[u8]>>,
}

impl<'v> PageBatch<'v> {
    pub fn new(vols: &'v VolumeSet) -> Self {
        PageBatch {
            vols,
            pages: HashMap::new(),
        }
    }

    pub fn volumes(&self) -> &'v VolumeSet {
        self.vols
    }

    /// The page, read from disk the first time.
    pub fn get(&mut self, vpid: Vpid) -> io::Result<&mut [u8]> {
        if !self.pages.contains_key(&vpid) {
            let p = self.vols.read_page(vpid)?;
            self.pages.insert(vpid, p);
        }
        Ok(self.pages.get_mut(&vpid).unwrap())
    }

    /// The page as zeros, whatever is on disk (a page this operation formats from scratch).
    pub fn blank(&mut self, vpid: Vpid) -> &mut [u8] {
        let io = self.vols.io_pagesize();
        let p = self
            .pages
            .entry(vpid)
            .or_insert_with(|| vec![0u8; io].into_boxed_slice());
        p.fill(0);
        p
    }

    /// Two different pages at once (merging one table component into another).
    pub fn get2(&mut self, a: Vpid, b: Vpid) -> io::Result<(&mut [u8], &mut [u8])> {
        assert_ne!(a, b);
        self.get(a)?;
        self.get(b)?;
        let [pa, pb] = self.pages.get_disjoint_mut([&a, &b]);
        Ok((pa.unwrap(), pb.unwrap()))
    }

    /// Writes every page held, in VPID order.
    pub fn flush(self) -> io::Result<()> {
        let mut v: Vec<_> = self.pages.iter().collect();
        v.sort_by_key(|(k, _)| **k);
        v.into_iter()
            .try_for_each(|(k, p)| self.vols.write_page(*k, p))
    }
}
