//! The overflow key file of one B-tree, created the first time a key needs it.
//!
//! Input: a factory that creates the file's sink; long key images. Output: their chains, and whether the file
//! exists (root header `ovfid`).
//! Source: engine creates `FILE_BTREE_OVERFLOW_KEY` on the first long key (`btree_first_oid`,
//! `btree_connect_page`) and destroys an unused one (R6 § 2 step 11); here an unused one is never created.

use crate::ovf_key_writer::write_overflow_key;
use crate::page_sink::PageSink;
use fastmig_ondisk::{Vfid, Vpid};
use std::io;
use std::sync::{Mutex, OnceLock};

/// Creates the overflow key file and its sink, the first time a key overflows. The sink borrows the factory, so a
/// factory over the target's space manager (`fill`) and a stand-alone one (a closure) both fit.
pub trait MakeSink: Sync {
    fn make(&self) -> io::Result<Box<dyn PageSink + Send + '_>>;
}

impl<F: Fn() -> io::Result<Box<dyn PageSink + Send>> + Sync> MakeSink for F {
    fn make(&self) -> io::Result<Box<dyn PageSink + Send + '_>> {
        self()
    }
}

pub struct OvfKeyFile<'a> {
    make: &'a dyn MakeSink,
    sink: OnceLock<Box<dyn PageSink + Send + 'a>>,
    creating: Mutex<()>,
}

impl<'a> OvfKeyFile<'a> {
    pub fn new(make: &'a dyn MakeSink) -> Self {
        Self {
            make,
            sink: OnceLock::new(),
            creating: Mutex::new(()),
        }
    }

    fn sink(&self) -> io::Result<&dyn PageSink> {
        if let Some(s) = self.sink.get() {
            return Ok(s.as_ref());
        }
        let _guard = self.creating.lock().unwrap();
        if self.sink.get().is_none() {
            let _ = self.sink.set(self.make.make()?);
        }
        Ok(self.sink.get().unwrap().as_ref())
    }

    pub fn write_key(&self, key: &[u8]) -> io::Result<Vpid> {
        write_overflow_key(self.sink()?, key)
    }

    /// The file, if a key went to it.
    pub fn vfid(&self) -> Option<Vfid> {
        self.sink.get().map(|s| s.vfid())
    }
}
