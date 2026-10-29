//! Big-endian cursor and builder for OR-encoded bytes (record bodies, packed values, packed domains).
//!
//! Input: a byte slice (read) or nothing (write). Output: integers, byte runs and 4-byte alignment, the way the
//! engine's `OR_BUF` moves through a buffer. Alignment is relative to the start of the slice, which the engine
//! also assumes (records and packed values start 4-byte aligned).
//! Source: engine `src/base/object_representation.h` (`OR_GET_INT` = `ntohl`, `or_get_align32`, `or_put_align32`),
//! `src/object/object_representation.c` (`or_get_*`/`or_put_*`); same in release/11.4 and develop.

use std::fmt;

/// A decode failure: what was being read and at which offset of the slice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeError {
    pub at: usize,
    pub what: String,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "decode error at byte {}: {}", self.at, self.what)
    }
}

impl std::error::Error for DecodeError {}

pub type DecodeResult<T> = Result<T, DecodeError>;

/// Read cursor over OR bytes. Every multi-byte integer is big endian.
#[derive(Clone)]
pub struct OrReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> OrReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        OrReader { buf, pos: 0 }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// The whole slice the cursor runs over.
    pub fn slice(&self) -> &'a [u8] {
        self.buf
    }

    pub fn error(&self, what: impl Into<String>) -> DecodeError {
        DecodeError {
            at: self.pos,
            what: what.into(),
        }
    }

    pub fn bytes(&mut self, n: usize) -> DecodeResult<&'a [u8]> {
        if n > self.remaining() {
            return Err(self.error(format!("need {n} bytes, {} left", self.remaining())));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    /// The next `N` bytes as an array (fixed-size fields).
    pub fn array<const N: usize>(&mut self) -> DecodeResult<[u8; N]> {
        let a = *self.buf[self.pos..]
            .first_chunk::<N>()
            .ok_or_else(|| self.error(format!("need {N} bytes, {} left", self.remaining())))?;
        self.pos += N;
        Ok(a)
    }

    pub fn skip(&mut self, n: usize) -> DecodeResult<()> {
        self.bytes(n).map(|_| ())
    }

    pub fn u8(&mut self) -> DecodeResult<u8> {
        Ok(self.bytes(1)?[0])
    }

    pub fn peek_u8(&self) -> DecodeResult<u8> {
        self.buf
            .get(self.pos)
            .copied()
            .ok_or_else(|| self.error("need 1 byte, 0 left"))
    }

    pub fn i16(&mut self) -> DecodeResult<i16> {
        Ok(i16::from_be_bytes(self.array()?))
    }

    pub fn u16(&mut self) -> DecodeResult<u16> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    pub fn i32(&mut self) -> DecodeResult<i32> {
        Ok(i32::from_be_bytes(self.array()?))
    }

    pub fn u32(&mut self) -> DecodeResult<u32> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    pub fn i64(&mut self) -> DecodeResult<i64> {
        Ok(i64::from_be_bytes(self.array()?))
    }

    /// Engine `or_get_align32`: skip to the next multiple of 4 from the slice start.
    pub fn align4(&mut self) -> DecodeResult<()> {
        let pad = (4 - (self.pos & 3)) & 3;
        self.skip(pad)
    }
}

/// Write builder for OR bytes. Every multi-byte integer is big endian.
#[derive(Clone, Default)]
pub struct OrWriter {
    pub buf: Vec<u8>,
}

impl OrWriter {
    pub fn new() -> Self {
        OrWriter { buf: Vec::new() }
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn bytes(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }

    pub fn zeros(&mut self, n: usize) {
        self.buf.resize(self.buf.len() + n, 0);
    }

    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn i16(&mut self, v: i16) {
        self.bytes(&v.to_be_bytes());
    }

    pub fn u16(&mut self, v: u16) {
        self.bytes(&v.to_be_bytes());
    }

    pub fn i32(&mut self, v: i32) {
        self.bytes(&v.to_be_bytes());
    }

    pub fn u32(&mut self, v: u32) {
        self.bytes(&v.to_be_bytes());
    }

    pub fn i64(&mut self, v: i64) {
        self.bytes(&v.to_be_bytes());
    }

    /// Engine `or_put_align32`: zero-pad to the next multiple of 4 from the buffer start.
    pub fn align4(&mut self) {
        let pad = (4 - (self.buf.len() & 3)) & 3;
        self.zeros(pad);
    }

    /// Overwrites a big-endian i32 already reserved at `at`.
    pub fn patch_i32(&mut self, at: usize, v: i32) {
        self.buf[at..at + 4].copy_from_slice(&v.to_be_bytes());
    }
}
