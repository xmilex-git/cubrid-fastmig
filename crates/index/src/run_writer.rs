//! Collects one producer's (key, OID) pairs for one index inside a fixed memory share; past it the run is sorted
//! and spilled, and collecting starts over.
//!
//! Input: pairs, the key order, the share in bytes, the spill directory. Output: the last in-memory run and the
//! spilled runs (`IndexRuns`).
//! Source: workspace#493 (memory limit and spill); engine counterpart `external_sort.c` (sorted runs written to
//! temporary files when the sort buffer fills).

use crate::key_order::KeyOrder;
use crate::memory_limit::{MemoryGrant, MemoryLimit};
use crate::sort_run::Run;
use crate::spill_dir::SpillDir;
use crate::spill_run::SpillRun;
use fastmig_ondisk::Oid;
use std::io;

/// The runs of one index, ready for `build_index`, with the memory grant their in-memory part holds.
#[derive(Default)]
pub struct IndexRuns<'m> {
    pub mem: Vec<Run>,
    pub spilled: Vec<SpillRun>,
    pub grants: Vec<MemoryGrant<'m>>,
}

impl<'m> IndexRuns<'m> {
    pub fn entries(&self) -> usize {
        self.mem.iter().map(Run::len).sum::<usize>()
            + self.spilled.iter().map(SpillRun::len).sum::<usize>()
    }

    pub fn append(&mut self, other: IndexRuns<'m>) {
        self.mem.extend(other.mem);
        self.spilled.extend(other.spilled);
        self.grants.extend(other.grants);
    }
}

pub struct RunWriter<'a> {
    order: &'a dyn KeyOrder,
    dir: &'a SpillDir,
    share: usize,
    run: Run,
    spilled: Vec<SpillRun>,
    words: Vec<u64>,
}

impl<'a> RunWriter<'a> {
    /// `share`: bytes this writer's in-memory run may take (keys, words, entries and the sort's spare entries).
    pub fn new(order: &'a dyn KeyOrder, dir: &'a SpillDir, share: usize) -> Self {
        Self {
            order,
            dir,
            share,
            run: Run::default(),
            spilled: Vec::new(),
            words: Vec::new(),
        }
    }

    pub fn push(&mut self, key: &[u8], oid: Oid) -> io::Result<()> {
        self.words.clear();
        self.order.radix_words(key, &mut self.words);
        if !self.run.is_empty()
            && self.run.bytes_after_push(key.len(), self.words.len()) > self.share
        {
            self.spill()?;
        }
        self.run.push(key, oid, &self.words);
        Ok(())
    }

    fn spill(&mut self) -> io::Result<()> {
        self.run.sort(self.order);
        self.spilled.push(SpillRun::write(self.dir, &self.run)?);
        self.run.clear();
        Ok(())
    }

    /// The runs, the in-memory one kept when the memory limit has room for it now (a grant of its size) and
    /// spilled otherwise: a finished run never waits for memory, so a scan that holds its worker shares cannot
    /// block on runs nobody frees before the build (workspace#503).
    pub fn finish_within<'m>(mut self, memory: &'m MemoryLimit) -> io::Result<IndexRuns<'m>> {
        if self.run.is_empty() {
            return Ok(IndexRuns {
                spilled: self.spilled,
                ..IndexRuns::default()
            });
        }
        let need = self.run.bytes();
        let grant = memory.try_acquire(need);
        if grant.bytes < need {
            drop(grant);
            self.spill()?;
            return Ok(IndexRuns {
                spilled: self.spilled,
                ..IndexRuns::default()
            });
        }
        Ok(self.finish(grant))
    }

    /// The runs, `grant` paying for the in-memory one.
    pub fn finish<'m>(self, grant: MemoryGrant<'m>) -> IndexRuns<'m> {
        IndexRuns {
            mem: vec![self.run],
            spilled: self.spilled,
            grants: vec![grant],
        }
    }
}
