//! A sorted run written to a spill file, and the reader the merge takes it back with.
//!
//! Input: a sorted in-memory `Run`. Output: the file (records `key_len u32 · OID 8 B · key`, host order) and a
//! sparse sample of its keys kept in memory to place splitters and range starts without reading it all.
//! Source: workspace#493; the engine writes sorted runs to temporary files the same way (`external_sort.c`).
//! The file is removed when the `SpillRun` is dropped.

use crate::key_order::KeyOrder;
use crate::sort_run::Run;
use crate::spill_dir::SpillDir;
use crate::temp_disk::{TEMP_DISK, spill_record_bytes};
use fastmig_ondisk::Oid;
use std::cmp::Ordering;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

/// One key in this many is kept as a sample.
const SAMPLE_EVERY: usize = 1024;
const RECORD_HEADER: usize = 4 + Oid::SIZE;

pub struct Sample {
    pub index: usize,
    pub offset: u64,
    pub key: Vec<u8>,
}

pub struct SpillRun {
    path: PathBuf,
    len: usize,
    samples: Vec<Sample>,
    /// Bytes of the file, counted in `TEMP_DISK` while the run lives.
    bytes: u64,
}

/// A position in a spill file: entry index and byte offset.
#[derive(Clone, Copy, Debug)]
pub struct SpillPos {
    pub index: usize,
    pub offset: u64,
}

impl SpillRun {
    /// Writes the sorted `run` to a new file in `dir`; its bytes count in [`TEMP_DISK`] from before the first write
    /// until the run is dropped (also when the write fails).
    pub fn write(dir: &SpillDir, run: &Run) -> io::Result<Self> {
        let bytes = (0..run.len())
            .map(|i| spill_record_bytes(run.key(i).len() as u64))
            .sum();
        TEMP_DISK.add(bytes);
        let mut me = Self {
            path: dir.new_file_path(),
            len: run.len(),
            samples: Vec::with_capacity(run.len() / SAMPLE_EVERY + 1),
            bytes,
        };
        let mut w = BufWriter::with_capacity(1 << 20, File::create(&me.path)?);
        let mut offset = 0u64;
        let mut hdr = [0u8; RECORD_HEADER];
        for i in 0..run.len() {
            let key = run.key(i);
            if i % SAMPLE_EVERY == 0 {
                me.samples.push(Sample {
                    index: i,
                    offset,
                    key: key.to_vec(),
                });
            }
            hdr[..4].copy_from_slice(&(key.len() as u32).to_le_bytes());
            run.oid(i).write(&mut hdr, 4);
            w.write_all(&hdr)?;
            w.write_all(key)?;
            offset += (RECORD_HEADER + key.len()) as u64;
        }
        w.flush()?;
        Ok(me)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn samples(&self) -> &[Sample] {
        &self.samples
    }

    pub fn end(&self) -> SpillPos {
        SpillPos {
            index: self.len,
            offset: u64::MAX,
        }
    }

    /// First position whose key is ≥ `key`.
    pub fn lower_bound(&self, key: &[u8], order: &dyn KeyOrder) -> io::Result<SpillPos> {
        let start = self
            .samples
            .partition_point(|s| order.compare(&s.key, key) == Ordering::Less);
        let Some(from) = start.checked_sub(1).map(|i| &self.samples[i]) else {
            return Ok(SpillPos {
                index: 0,
                offset: 0,
            });
        };
        let mut c = SpillCursor::open(
            self,
            SpillPos {
                index: from.index,
                offset: from.offset,
            },
            self.len,
            1 << 16,
        )?;
        let mut pos = SpillPos {
            index: from.index,
            offset: from.offset,
        };
        while c.valid() && order.compare(c.key(), key) == Ordering::Less {
            pos.offset += (RECORD_HEADER + c.key().len()) as u64;
            pos.index += 1;
            c.advance()?;
        }
        if pos.index == self.len {
            pos = self.end();
        }
        Ok(pos)
    }
}

impl Drop for SpillRun {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        TEMP_DISK.sub(self.bytes);
    }
}

/// Reads entries `[from.index, end)` of a spill file in order.
pub struct SpillCursor {
    r: BufReader<File>,
    remaining: usize,
    key: Vec<u8>,
    oid: Oid,
    valid: bool,
}

impl SpillCursor {
    pub fn open(run: &SpillRun, from: SpillPos, end: usize, buf_bytes: usize) -> io::Result<Self> {
        let mut f = File::open(&run.path)?;
        if from.index < end {
            f.seek(SeekFrom::Start(from.offset))?;
        }
        let mut c = Self {
            r: BufReader::with_capacity(buf_bytes, f),
            remaining: end.saturating_sub(from.index),
            key: Vec::new(),
            oid: Oid::NULL,
            valid: false,
        };
        c.advance()?;
        Ok(c)
    }

    pub fn valid(&self) -> bool {
        self.valid
    }

    pub fn key(&self) -> &[u8] {
        &self.key
    }

    pub fn oid(&self) -> Oid {
        self.oid
    }

    pub fn advance(&mut self) -> io::Result<()> {
        if self.remaining == 0 {
            self.valid = false;
            return Ok(());
        }
        let mut hdr = [0u8; RECORD_HEADER];
        self.r.read_exact(&mut hdr)?;
        let n = u32::from_le_bytes(hdr[..4].try_into().unwrap()) as usize;
        self.oid = Oid::read(&hdr, 4);
        self.key.resize(n, 0);
        self.r.read_exact(&mut self.key)?;
        self.remaining -= 1;
        self.valid = true;
        Ok(())
    }
}
