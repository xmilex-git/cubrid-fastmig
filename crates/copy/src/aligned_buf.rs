//! A heap buffer aligned for direct I/O (`O_DIRECT` needs the buffer, offset and length aligned to the block size).
//!
//! Input: a length and an alignment. Output: a zeroed byte buffer with that alignment, freed on drop.
//! Source: Linux `open(2)` § O_DIRECT.

use std::alloc::{Layout, alloc_zeroed, dealloc};

pub struct AlignedBuf {
    ptr: *mut u8,
    layout: Layout,
}

// SAFETY: the buffer is owned memory with no shared state; moving it between threads is like moving a Vec<u8>.
unsafe impl Send for AlignedBuf {}

impl AlignedBuf {
    pub fn new(len: usize, align: usize) -> Self {
        let layout = Layout::from_size_align(len, align)
            .expect("power-of-two alignment, non-overflowing length");
        // SAFETY: the layout has a non-zero size (callers pass whole pages).
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        AlignedBuf { ptr, layout }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: ptr points to layout.size() initialized bytes owned by self.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.layout.size()) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        // SAFETY: ptr was allocated with this layout and is freed once.
        unsafe { dealloc(self.ptr, self.layout) }
    }
}
