//! The spill arena: one file of the temp disk limit's size, allocated in full before the run writes anything, that
//! every spill run of the run is written into as extents. Its space is never given back to the filesystem until
//! the run ends, so no other process can take it in the middle (user: no disk full during a run).
//!
//! Input: the path and the size (`--temp-disk`). Output: [`SpillArena`] — extents handed out first fit and taken
//! back on free (a run that finds no single free range big enough gets several), and an
//! [`ExtentReader`] that sees a run's extents as one byte stream. The file is removed when the arena is dropped;
//! a killed run leaves it to the incomplete marker's rerun cleanup (cli records it).
//! Source: workspace#526 (1번 세션 decision: spill inside the reservation, no window between freeing and taking
//! space); POSIX `posix_fallocate(3)`.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fastmig_ondisk::preallocate;

/// One range of the arena file: byte offset and length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extent {
    pub offset: u64,
    pub len: u64,
}

pub struct SpillArena {
    file: File,
    path: PathBuf,
    size: u64,
    /// Free ranges by offset, adjacent ones merged.
    free: Mutex<BTreeMap<u64, u64>>,
}

impl SpillArena {
    /// Creates the file (it must not exist) and allocates all of its `size` bytes on the filesystem.
    pub fn create(path: &Path, size: u64) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;
        if let Err(e) = preallocate::allocate(&file, size) {
            let _ = std::fs::remove_file(path);
            return Err(e);
        }
        Ok(Self {
            file,
            path: path.to_path_buf(),
            size,
            free: Mutex::new(if size > 0 {
                BTreeMap::from([(0, size)])
            } else {
                BTreeMap::new()
            }),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// Takes `bytes` as one or more extents, lowest offsets first. Err when the arena holds less free space: the
    /// batching keeps every scan's spill bound within the arena, so this means the bound was wrong.
    pub fn alloc(&self, bytes: u64) -> io::Result<Vec<Extent>> {
        let mut free = self.free.lock().unwrap();
        let total: u64 = free.values().sum();
        if total < bytes {
            return Err(io::Error::other(format!(
                "spill arena {}: {bytes} B asked, {total} B free of {} B (the spill bound was exceeded)",
                self.path.display(),
                self.size
            )));
        }
        // a free range that holds it all keeps the run in one piece; otherwise take ranges in offset order
        if let Some((&off, &len)) = free.iter().find(|&(_, &len)| len >= bytes) {
            free.remove(&off);
            if len > bytes {
                free.insert(off + bytes, len - bytes);
            }
            return Ok(vec![Extent {
                offset: off,
                len: bytes,
            }]);
        }
        let mut out = Vec::new();
        let mut left = bytes;
        while left > 0 {
            let (&off, &len) = free.iter().next().expect("total checked above");
            free.remove(&off);
            let take = len.min(left);
            if len > take {
                free.insert(off + take, len - take);
            }
            out.push(Extent {
                offset: off,
                len: take,
            });
            left -= take;
        }
        Ok(out)
    }

    /// Gives extents back, merging each with its free neighbours.
    pub fn free(&self, extents: &[Extent]) {
        let mut free = self.free.lock().unwrap();
        for e in extents.iter().filter(|e| e.len > 0) {
            let (mut off, mut len) = (e.offset, e.len);
            if let Some((&p, &pl)) = free.range(..off).next_back()
                && p + pl == off
            {
                free.remove(&p);
                off = p;
                len += pl;
            }
            if let Some(nl) = free.remove(&(off + len)) {
                len += nl;
            }
            free.insert(off, len);
        }
    }

    pub fn file(&self) -> &File {
        &self.file
    }
}

impl Drop for SpillArena {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Maps a position of a run's byte stream to (extent index, offset in the file).
fn locate(extents: &[Extent], pos: u64) -> Option<(usize, u64)> {
    let mut start = 0;
    for (i, e) in extents.iter().enumerate() {
        if pos < start + e.len {
            return Some((i, e.offset + (pos - start)));
        }
        start += e.len;
    }
    None
}

/// Reads a run's byte stream back from its extents; seeks only from the start.
pub struct ExtentReader {
    arena: Arc<SpillArena>,
    extents: Vec<Extent>,
    pos: u64,
}

impl ExtentReader {
    pub fn new(arena: Arc<SpillArena>, extents: Vec<Extent>) -> Self {
        Self {
            arena,
            extents,
            pos: 0,
        }
    }
}

impl Read for ExtentReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some((i, at)) = locate(&self.extents, self.pos) else {
            return Ok(0);
        };
        let e = self.extents[i];
        let take = buf.len().min((e.offset + e.len - at) as usize);
        let n = self.arena.file().read_at(&mut buf[..take], at)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for ExtentReader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        match to {
            SeekFrom::Start(p) => {
                self.pos = p;
                Ok(p)
            }
            _ => Err(io::Error::other(
                "spill run reader seeks from the start only",
            )),
        }
    }
}
