//! The one memory limit of a run (`--mem`), in bytes, shared by everything that holds keys in memory: heap-scan
//! runs of every class and index, sort scratch, merge buffers.
//!
//! Input: the limit (cli `Limits::memory_limit_bytes`, default MemTotal/4, the same argument the verifier uses).
//! Output: grants held while the memory is in use, returned on drop.
//! Source: workspace#493 (memory limit and spill, added at the user's request); the engine's counterpart is the
//! index-build sort buffer (`index_build_buffer_size`, CBRD-27235, `system_parameter.c`) with runs spilled to
//! temporary files (`external_sort.c` `sort_param->tot_buffers`, `file_create_temp`), develop 5b1867174.

use std::sync::{Condvar, Mutex};

pub struct MemoryLimit {
    free: Mutex<usize>,
    freed: Condvar,
    limit: usize,
}

/// Bytes held; returned to the limit on drop.
pub struct MemoryGrant<'a> {
    limit: &'a MemoryLimit,
    pub bytes: usize,
}

impl MemoryLimit {
    pub fn new(limit_bytes: usize) -> Self {
        Self {
            free: Mutex::new(limit_bytes),
            freed: Condvar::new(),
            limit: limit_bytes,
        }
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Waits until `bytes` (at most the whole limit) are free and takes them.
    pub fn acquire(&self, bytes: usize) -> MemoryGrant<'_> {
        let bytes = bytes.min(self.limit);
        let mut free = self.free.lock().unwrap();
        while *free < bytes {
            free = self.freed.wait(free).unwrap();
        }
        *free -= bytes;
        MemoryGrant { limit: self, bytes }
    }

    /// Takes up to `bytes` of what is free now, without waiting (possibly nothing).
    pub fn try_acquire(&self, bytes: usize) -> MemoryGrant<'_> {
        let mut free = self.free.lock().unwrap();
        let bytes = bytes.min(*free);
        *free -= bytes;
        MemoryGrant { limit: self, bytes }
    }
}

impl<'a> MemoryGrant<'a> {
    /// Takes over the bytes of `other` (of the same limit), so one grant pays for a structure that grows.
    pub fn absorb(&mut self, mut other: MemoryGrant<'a>) {
        self.bytes += std::mem::take(&mut other.bytes);
    }

    /// Moves `bytes` (at most what this grant holds) into a grant of its own.
    pub fn split(&mut self, bytes: usize) -> MemoryGrant<'a> {
        let bytes = bytes.min(self.bytes);
        self.bytes -= bytes;
        MemoryGrant {
            limit: self.limit,
            bytes,
        }
    }
}

impl Drop for MemoryGrant<'_> {
    fn drop(&mut self) {
        *self.limit.free.lock().unwrap() += self.bytes;
        self.limit.freed.notify_all();
    }
}
