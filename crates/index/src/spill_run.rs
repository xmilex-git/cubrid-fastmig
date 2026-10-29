//! A sorted run written to a spill file, and the reader the merge takes it back with.
//!
//! Input: a sorted in-memory `Run`. Output: the file (records `key_len u32 · OID 8 B · key`, host order) and a
//! sparse sample of its keys kept in memory to place splitters and range starts without reading it all.
//! Source: workspace#493; the engine writes sorted runs to temporary files the same way (`external_sort.c`).
//! The file is removed when the `SpillRun` is dropped.

use crate::key_order::KeyOrder;
use crate::sort_run::Run;
use crate::spill_arena::{Extent, ExtentReader, SpillArena};
use crate::spill_dir::SpillDir;
use crate::temp_disk::{TEMP_DISK, spill_record_bytes};
use fastmig_ondisk::Oid;
use std::cmp::Ordering;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::Arc;

/// One key in this many is kept as a sample.
const SAMPLE_EVERY: usize = 1024;
const RECORD_HEADER: usize = 4 + Oid::SIZE;

pub struct Sample {
    pub index: usize,
    pub offset: u64,
    pub key: Vec<u8>,
}

/// Where a run's records are: its own file, or extents of the spill arena (`--temp-disk`, workspace#526).
enum Storage {
    File(PathBuf),
    Arena(Arc<SpillArena>, Vec<Extent>),
}

pub struct SpillRun {
    storage: Storage,
    len: usize,
    samples: Vec<Sample>,
    /// Bytes of the file, counted in `TEMP_DISK` while the run lives.
    bytes: u64,
    /// The longest key: what a cursor's key buffer grows to.
    max_key: usize,
}

/// A position in a spill file: entry index and byte offset.
#[derive(Clone, Copy, Debug)]
pub struct SpillPos {
    pub index: usize,
    pub offset: u64,
}

/// The space of a run being written, taken as its bytes go out: an arena run gets extents per buffer flush, so a
/// merge that frees its inputs as it reads them never holds an input and its output at once (workspace#526).
struct RunSink {
    run: SpillRun,
    file: Option<File>,
}

impl Write for RunSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = buf.len() as u64;
        match &mut self.run.storage {
            Storage::File(_) => {
                let n = self.file.as_mut().unwrap().write(buf)?;
                TEMP_DISK.add(n as u64);
                self.run.bytes += n as u64;
                Ok(n)
            }
            Storage::Arena(a, ext) => {
                // the run owns the extents (and frees them on drop) before a byte goes into them
                let new = a.alloc(n)?;
                TEMP_DISK.add(n);
                self.run.bytes += n;
                for e in &new {
                    match ext.last_mut() {
                        Some(l) if l.offset + l.len == e.offset => l.len += e.len,
                        _ => ext.push(*e),
                    }
                }
                let mut at = 0;
                for e in &new {
                    a.file()
                        .write_all_at(&buf[at..at + e.len as usize], e.offset)?;
                    at += e.len as usize;
                }
                Ok(buf.len())
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.as_mut().map_or(Ok(()), |f| f.flush())
    }
}

/// Writes one sorted run record by record (`key_len u32 · OID 8 B · key`), through a buffer of `buf_bytes`.
pub struct SpillOut {
    w: BufWriter<RunSink>,
    offset: u64,
    len: usize,
    max_key: usize,
    samples: Vec<Sample>,
}

impl SpillOut {
    pub fn new(dir: &SpillDir, buf_bytes: usize) -> io::Result<Self> {
        let (storage, file) = match dir.arena() {
            Some(a) => (Storage::Arena(a.clone(), Vec::new()), None),
            None => {
                let p = dir.new_file_path();
                let f = File::create(&p)?;
                (Storage::File(p), Some(f))
            }
        };
        let run = SpillRun {
            storage,
            len: 0,
            samples: Vec::new(),
            bytes: 0,
            max_key: 0,
        };
        Ok(Self {
            w: BufWriter::with_capacity(buf_bytes, RunSink { run, file }),
            offset: 0,
            len: 0,
            max_key: 0,
            samples: Vec::new(),
        })
    }

    pub fn push(&mut self, key: &[u8], oid: Oid) -> io::Result<()> {
        if self.len.is_multiple_of(SAMPLE_EVERY) {
            self.samples.push(Sample {
                index: self.len,
                offset: self.offset,
                key: key.to_vec(),
            });
        }
        let mut hdr = [0u8; RECORD_HEADER];
        hdr[..4].copy_from_slice(&(key.len() as u32).to_le_bytes());
        oid.write(&mut hdr, 4);
        self.w.write_all(&hdr)?;
        self.w.write_all(key)?;
        self.offset += spill_record_bytes(key.len() as u64);
        self.len += 1;
        self.max_key = self.max_key.max(key.len());
        Ok(())
    }

    pub fn finish(self) -> io::Result<SpillRun> {
        let mut sink = self.w.into_inner().map_err(|e| e.into_error())?;
        sink.run.len = self.len;
        sink.run.max_key = self.max_key;
        sink.run.samples = self.samples;
        Ok(sink.run)
    }
}

impl SpillRun {
    /// Writes the sorted `run` to a new file in `dir` through a buffer of `buf_bytes`; its bytes count in
    /// [`TEMP_DISK`] from before they are written until the run is dropped (also when the write fails).
    pub fn write(dir: &SpillDir, run: &Run, buf_bytes: usize) -> io::Result<Self> {
        let mut out = SpillOut::new(dir, buf_bytes)?;
        for i in 0..run.len() {
            out.push(run.key(i), run.oid(i))?;
        }
        out.finish()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    /// Bytes the run holds on disk.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn max_key(&self) -> usize {
        self.max_key
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
        match &self.storage {
            Storage::File(p) => {
                let _ = std::fs::remove_file(p);
            }
            Storage::Arena(a, ext) => a.free(ext),
        }
        TEMP_DISK.sub(self.bytes);
    }
}

/// A run read once from its start by the merge that replaces it: each range of an arena run is freed as soon as it
/// is in the read buffer, so the merged output can take its place (a file run keeps its file until dropped).
struct Consumer {
    run: SpillRun,
    file: Option<File>,
    /// First extent not yet read to its end.
    head: usize,
}

impl Read for Consumer {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Storage::Arena(a, ext) = &mut self.run.storage else {
            return self.file.as_mut().unwrap().read(buf);
        };
        while ext.get(self.head).is_some_and(|e| e.len == 0) {
            self.head += 1;
        }
        let Some(e) = ext.get_mut(self.head) else {
            return Ok(0);
        };
        let take = buf.len().min(e.len as usize);
        let n = a.file().read_at(&mut buf[..take], e.offset)?;
        if n == 0 {
            return Err(io::Error::other("spill arena shorter than a run's extent"));
        }
        let read = Extent {
            offset: e.offset,
            len: n as u64,
        };
        e.offset += read.len;
        e.len -= read.len;
        a.free(&[read]);
        self.run.bytes -= read.len;
        TEMP_DISK.sub(read.len);
        Ok(n)
    }
}

/// A run's bytes as one stream: its file, its arena extents, or the run consumed as it is read.
enum RunReader {
    File(File),
    Arena(ExtentReader),
    Consume(Consumer),
}

impl Read for RunReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            RunReader::File(f) => f.read(buf),
            RunReader::Arena(r) => r.read(buf),
            RunReader::Consume(c) => c.read(buf),
        }
    }
}

impl Seek for RunReader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        match self {
            RunReader::File(f) => f.seek(to),
            RunReader::Arena(r) => r.seek(to),
            RunReader::Consume(_) => Err(io::Error::other(
                "a consumed run is read from its start only",
            )),
        }
    }
}

pub struct SpillCursor {
    r: BufReader<RunReader>,
    remaining: usize,
    key: Vec<u8>,
    oid: Oid,
    valid: bool,
}

impl SpillCursor {
    pub fn open(run: &SpillRun, from: SpillPos, end: usize, buf_bytes: usize) -> io::Result<Self> {
        let mut f = match &run.storage {
            Storage::File(p) => RunReader::File(File::open(p)?),
            Storage::Arena(a, ext) => RunReader::Arena(ExtentReader::new(a.clone(), ext.clone())),
        };
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

    /// Takes `run` and reads all of it once, freeing its space as it goes ([`Consumer`]).
    pub fn consume(run: SpillRun, buf_bytes: usize) -> io::Result<Self> {
        let file = match &run.storage {
            Storage::File(p) => Some(File::open(p)?),
            Storage::Arena(..) => None,
        };
        let remaining = run.len;
        let mut c = Self {
            r: BufReader::with_capacity(
                buf_bytes,
                RunReader::Consume(Consumer { run, file, head: 0 }),
            ),
            remaining,
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
