//! Slotted page: reads the slot directory of a page and builds a new page record by record.
//!
//! Input: the user area of one page (`page::db_area`: the IO page without the 32 B FILEIO reserved
//! header and the 8 B watermark; 16344 B for a 16 KiB IO page, sized at run time).
//! Output: (record type, record bytes) per slot; a freshly built user area for new pages.
//! Source: engine `slotted_page.c:spage_initialize`, `spage_find_empty_slot`, `spage_insert_data`,
//! `spage_max_space_for_new_record`, `spage_find_slot`, `spage_is_unknown_slot` (develop 5b1867174,
//! release/11.4 46212adcd); the builder follows the engine rules only for a page that starts empty and
//! only gains records, which is all a new page needs.

/// Engine `SPAGE_HEADER` (src/storage/slotted_page.h). Size: 32 B in 11.4 and develop.
/// Offsets (both versions, host order = little endian on x86-64; computed, checked on T1 B-tree pages):
/// 0 `num_slots` i16, 2 `num_records` i16, 4 `anchor_type` i16, 6 `alignment` u16, 8 `total_free` i32,
/// 12 `cont_free` i32, 16 `offset_to_free_area` i32, 20 `reserved1` i32, 24 `flags` i32,
/// 28 bit field u32: bit 0 `is_saving` in both; 11.4 also has bit 1 `need_update_best_hint`
/// (heap pages only), develop drops it (reserved bits 31 instead of 30).
pub const SPAGE_HEADER_SIZE: usize = 32;

use crate::le_field::{get_i16, get_i32, get_u16, get_u32, put_i16, put_i32, put_u16, put_u32};

/// Engine `SPAGE_SLOT`: one u32 at the end of the page, slot 0 last; bits 0..14 offset to the record,
/// 14..28 record length, 28..32 record type.
pub const SPAGE_SLOT_SIZE: usize = 4;

/// Engine `SPAGE_EMPTY_OFFSET`: offset of a slot whose record is gone.
const SPAGE_EMPTY_OFFSET: usize = 0;

/// Engine slot anchoring (`ANCHORED` .. `UNANCHORED_KEEP_SEQUENCE`, slotted_page.h).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i16)]
pub enum Anchor {
    Anchored = 1,
    AnchoredDontReuseSlots = 2,
    UnanchoredAnySequence = 3,
    UnanchoredKeepSequence = 4,
}

/// Engine `REC_*` record types stored in a slot (storage_common.h `INT16 type`), same in 11.4 and develop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RecordType {
    Unknown = 0,
    AssignAddress = 1,
    Home = 2,
    NewHome = 3,
    Relocation = 4,
    BigOne = 5,
    MarkDeleted = 6,
    DeletedWillReuse = 7,
}

impl RecordType {
    fn from_bits(bits: u32) -> Option<Self> {
        Some(match bits {
            0 => Self::Unknown,
            1 => Self::AssignAddress,
            2 => Self::Home,
            3 => Self::NewHome,
            4 => Self::Relocation,
            5 => Self::BigOne,
            6 => Self::MarkDeleted,
            7 => Self::DeletedWillReuse,
            _ => return None,
        })
    }
}

fn align_up(n: usize, align: usize) -> usize {
    n.div_ceil(align) * align
}

fn slot_pos(user_area_len: usize, slotid: usize) -> usize {
    user_area_len - SPAGE_SLOT_SIZE * (slotid + 1)
}

/// Read view of a slotted page's user area.
#[derive(Clone, Copy)]
pub struct SlottedPage<'a> {
    area: &'a [u8],
}

impl<'a> SlottedPage<'a> {
    /// Wraps the user area; `None` if it is too short to hold the header or the slot count overruns it.
    pub fn new(area: &'a [u8]) -> Option<Self> {
        if area.len() < SPAGE_HEADER_SIZE {
            return None;
        }
        let page = Self { area };
        let n = page.num_slots();
        if SPAGE_HEADER_SIZE + n * SPAGE_SLOT_SIZE > area.len() {
            return None;
        }
        Some(page)
    }

    pub fn num_slots(&self) -> usize {
        get_i16(self.area, 0).max(0) as usize
    }

    pub fn num_records(&self) -> usize {
        get_i16(self.area, 2).max(0) as usize
    }

    pub fn anchor_raw(&self) -> i16 {
        get_i16(self.area, 4)
    }

    pub fn alignment(&self) -> usize {
        get_u16(self.area, 6) as usize
    }

    pub fn total_free(&self) -> i32 {
        get_i32(self.area, 8)
    }

    /// The record in `slotid` with its type, or `None` for a slot the engine treats as unknown
    /// (out of range, empty offset, offset inside the header) or one whose bytes overrun the page.
    pub fn record(&self, slotid: usize) -> Option<(RecordType, &'a [u8])> {
        if slotid >= self.num_slots() {
            return None;
        }
        let pos = slot_pos(self.area.len(), slotid);
        let raw = get_u32(self.area, pos);
        let offset = (raw & 0x3fff) as usize;
        let length = ((raw >> 14) & 0x3fff) as usize;
        let rtype = RecordType::from_bits(raw >> 28)?;
        if offset == SPAGE_EMPTY_OFFSET || offset < SPAGE_HEADER_SIZE {
            return None;
        }
        let end = offset.checked_add(length)?;
        if end > slot_pos(self.area.len(), self.num_slots() - 1) {
            return None;
        }
        Some((rtype, &self.area[offset..end]))
    }
}

/// Builds a page that starts empty (`spage_initialize`) and gains records only at the end of its
/// slot directory (`spage_insert` / `spage_insert_at(num_slots)` on a page without deleted slots).
pub struct SpageBuilder<'a> {
    area: &'a mut [u8],
    alignment: usize,
    num_slots: usize,
    total_free: usize,
    offset_to_free_area: usize,
}

impl<'a> SpageBuilder<'a> {
    /// `spage_initialize(anchor, alignment, is_saving)`: zeroes `area` (the page's user area,
    /// `page::db_area_mut`) and starts an empty slotted page in it.
    pub fn new(area: &'a mut [u8], anchor: Anchor, alignment: usize, is_saving: bool) -> Self {
        area.fill(0);
        put_i16(area, 4, anchor as i16);
        put_u16(area, 6, alignment as u16);
        put_u32(area, 28, is_saving as u32);
        let len = area.len();
        Self {
            area,
            alignment,
            num_slots: 0,
            // engine: total_free = DB_ALIGN(pagesize - sizeof(SPAGE_HEADER), alignment)
            total_free: align_up(len - SPAGE_HEADER_SIZE, alignment),
            offset_to_free_area: align_up(SPAGE_HEADER_SIZE, alignment),
        }
    }

    pub fn num_slots(&self) -> usize {
        self.num_slots
    }

    /// `spage_max_space_for_new_record`: free space minus a new slot, aligned down; 0 when less than a slot is left.
    pub fn max_space_for_new_record(&self) -> usize {
        let free = self.total_free as isize - SPAGE_SLOT_SIZE as isize;
        if free < SPAGE_SLOT_SIZE as isize {
            return 0;
        }
        (free as usize / self.alignment) * self.alignment
    }

    /// `spage_insert` into a new slot; returns the slot id, or `None` when the record does not fit (SP_DOESNT_FIT).
    pub fn insert(&mut self, rtype: RecordType, rec: &[u8]) -> Option<usize> {
        let space = align_up(rec.len(), self.alignment);
        if rec.len() > 0x3fff || space + SPAGE_SLOT_SIZE > self.total_free {
            return None;
        }
        let slotid = self.num_slots;
        let offset = self.offset_to_free_area;
        // the slot directory grows down from the page end; free area must not reach it
        if offset + space > slot_pos(self.area.len(), slotid) {
            return None;
        }
        self.area[offset..offset + rec.len()].copy_from_slice(rec);
        let raw = (offset as u32) | ((rec.len() as u32) << 14) | ((rtype as u32) << 28);
        put_u32(self.area, slot_pos(self.area.len(), slotid), raw);
        self.num_slots += 1;
        self.total_free -= space + SPAGE_SLOT_SIZE;
        self.offset_to_free_area += space;
        Some(slotid)
    }

    /// Writes the header counters; the page is complete after this.
    pub fn finish(self) {
        put_i16(self.area, 0, self.num_slots as i16);
        put_i16(self.area, 2, self.num_slots as i16);
        put_i32(self.area, 8, self.total_free as i32);
        // a page that only gained records has one contiguous free area
        put_i32(self.area, 12, self.total_free as i32);
        put_i32(self.area, 16, self.offset_to_free_area as i32);
    }
}
