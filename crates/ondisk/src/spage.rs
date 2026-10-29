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

/// Why an edit of a slotted page did not happen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpageError {
    /// Not enough total free space (engine `SP_DOESNT_FIT`): the record must go to another page.
    DoesntFit,
    /// The slot does not hold a record (out of range or deleted).
    NoRecord,
    /// The page header and slots disagree; the page is not edited.
    Corrupt,
}

/// In-place editor of an existing slotted page, following the engine's space accounting so the header counters
/// stay what the engine would leave: rewrite a record in its slot, insert, delete, change a slot's record type,
/// compact, clear the 11.4-only header bit.
/// Source: engine `slotted_page.c` `spage_update` / `spage_check_updatable` / `spage_update_record_in_place` /
/// `spage_update_record_after_compact` (:2193-2595), `spage_insert` / `spage_find_empty_slot` /
/// `spage_find_free_slot` (:1264-1455, 1739-1856), `spage_delete` / `spage_shift_slot_down` /
/// `spage_reduce_a_slot` (:1502-1527, 2027-2133), `spage_compact` (:1144-1253),
/// `spage_max_space_for_new_record` (:948-984), develop 5b1867174 (same in release/11.4 46212adcd apart from the
/// header bit). Space saved for undo (`is_saving`) is ignored: an offline database has no active transactions.
pub struct SpageEditor<'a> {
    area: &'a mut [u8],
}

const H_NUM_SLOTS: usize = 0;
const H_NUM_RECORDS: usize = 2;
const H_ANCHOR: usize = 4;
const H_ALIGNMENT: usize = 6;
const H_TOTAL_FREE: usize = 8;
const H_CONT_FREE: usize = 12;
const H_OFFSET_TO_FREE_AREA: usize = 16;
const H_BITS: usize = 28;
/// 11.4 `need_update_best_hint` (bit 1 of the bit-field word); develop's `reserved_bits`.
const NEED_UPDATE_BEST_HINT: u32 = 0x2;

#[derive(Clone, Copy)]
struct Slot {
    offset: usize,
    length: usize,
    rtype: u32,
}

impl<'a> SpageEditor<'a> {
    /// Wraps the user area (`page::db_area_mut`) of a slotted page.
    pub fn new(area: &'a mut [u8]) -> Option<Self> {
        SlottedPage::new(area)?;
        Some(Self { area })
    }

    pub fn view(&self) -> SlottedPage<'_> {
        SlottedPage { area: self.area }
    }

    fn h(&self, off: usize) -> i32 {
        if off < 8 {
            get_i16(self.area, off) as i32
        } else {
            get_i32(self.area, off)
        }
    }

    fn set_h(&mut self, off: usize, v: i32) {
        if off < 8 {
            put_i16(self.area, off, v as i16)
        } else {
            put_i32(self.area, off, v)
        }
    }

    fn add_h(&mut self, off: usize, d: i32) {
        let v = self.h(off) + d;
        self.set_h(off, v);
    }

    fn alignment(&self) -> usize {
        (get_u16(self.area, H_ALIGNMENT) as usize).max(1)
    }

    fn waste(&self, len: usize) -> usize {
        align_up(len, self.alignment()) - len
    }

    fn slot(&self, id: usize) -> Slot {
        let raw = get_u32(self.area, slot_pos(self.area.len(), id));
        Slot {
            offset: (raw & 0x3fff) as usize,
            length: (raw >> 14 & 0x3fff) as usize,
            rtype: raw >> 28,
        }
    }

    fn set_slot(&mut self, id: usize, s: Slot) {
        let raw = (s.offset as u32) | ((s.length as u32) << 14) | (s.rtype << 28);
        let n = self.area.len();
        put_u32(self.area, slot_pos(n, id), raw);
    }

    fn num_slots(&self) -> usize {
        self.h(H_NUM_SLOTS) as usize
    }

    fn live(&self, id: usize) -> Option<Slot> {
        (id < self.num_slots())
            .then(|| self.slot(id))
            .filter(|s| s.offset != SPAGE_EMPTY_OFFSET)
    }

    fn at_end(&self, s: Slot) -> bool {
        s.offset + s.length + self.waste(s.length) == self.h(H_OFFSET_TO_FREE_AREA) as usize
    }

    /// Engine `spage_max_record_size`: the largest record a page can hold.
    pub fn max_record_size(&self) -> usize {
        self.area.len() - SPAGE_HEADER_SIZE - SPAGE_SLOT_SIZE
    }

    /// Engine `spage_max_space_for_new_record`.
    pub fn max_space_for_new_record(&self) -> usize {
        let mut free = self.h(H_TOTAL_FREE);
        let reuse = self.h(H_ANCHOR) != Anchor::AnchoredDontReuseSlots as i32
            && self.h(H_NUM_SLOTS) > self.h(H_NUM_RECORDS);
        if !reuse {
            free -= SPAGE_SLOT_SIZE as i32;
            if free < SPAGE_SLOT_SIZE as i32 {
                free = 0;
            }
        }
        let a = self.alignment() as i32;
        (free.max(0) / a * a) as usize
    }

    /// Whether `slotid`'s record can be rewritten with `len` bytes in this page (engine `spage_is_updatable`).
    pub fn can_update(&self, slotid: usize, len: usize) -> bool {
        self.live(slotid).is_some_and(|s| {
            len <= self.max_record_size()
                && (len + self.waste(len)) as i32 - (s.length + self.waste(s.length)) as i32
                    <= self.h(H_TOTAL_FREE)
        })
    }

    /// Engine `spage_update`: rewrites the record in `slotid` (same slot, same record type), in place when it
    /// does not grow, otherwise at the free area after compacting if needed.
    pub fn update(&mut self, slotid: usize, rec: &[u8]) -> Result<(), SpageError> {
        let mut s = self.live(slotid).ok_or(SpageError::NoRecord)?;
        if !self.can_update(slotid, rec.len()) {
            return Err(SpageError::DoesntFit);
        }
        let (old_w, new_w) = (self.waste(s.length), self.waste(rec.len()));
        let space = (rec.len() + new_w) as i32 - (s.length + old_w) as i32;
        if rec.len() <= s.length {
            let at_end = self.at_end(s);
            self.area[s.offset..s.offset + rec.len()].copy_from_slice(rec);
            s.length = rec.len();
            self.set_slot(slotid, s);
            self.add_h(H_TOTAL_FREE, -space);
            if at_end {
                self.add_h(H_CONT_FREE, -space);
                self.add_h(H_OFFSET_TO_FREE_AREA, space);
            }
            return Ok(());
        }
        let mut space = space;
        if self.at_end(s) && space <= self.h(H_CONT_FREE) {
            // give the record's own bytes back to the free area, then append
            let give = (old_w + s.length) as i32;
            self.add_h(H_TOTAL_FREE, give);
            self.add_h(H_CONT_FREE, give);
            self.add_h(H_OFFSET_TO_FREE_AREA, -give);
            space = (rec.len() + new_w) as i32;
        } else if (rec.len() + new_w) as i32 > self.h(H_CONT_FREE) {
            let old = s;
            s.offset = SPAGE_EMPTY_OFFSET;
            self.set_slot(slotid, s);
            self.add_h(H_TOTAL_FREE, (old_w + old.length) as i32);
            self.add_h(H_NUM_RECORDS, -1);
            if self.compact().is_err() {
                self.set_slot(slotid, old);
                self.add_h(H_TOTAL_FREE, -((old_w + old.length) as i32));
                self.add_h(H_NUM_RECORDS, 1);
                return Err(SpageError::Corrupt);
            }
            self.add_h(H_NUM_RECORDS, 1);
            space = (rec.len() + new_w) as i32;
        }
        let at = self.h(H_OFFSET_TO_FREE_AREA) as usize;
        if at + rec.len() > self.area.len() {
            return Err(SpageError::Corrupt);
        }
        self.area[at..at + rec.len()].copy_from_slice(rec);
        self.set_slot(
            slotid,
            Slot {
                offset: at,
                length: rec.len(),
                rtype: s.rtype,
            },
        );
        self.add_h(H_TOTAL_FREE, -space);
        self.add_h(H_CONT_FREE, -((rec.len() + new_w) as i32));
        self.add_h(H_OFFSET_TO_FREE_AREA, (rec.len() + new_w) as i32);
        Ok(())
    }

    /// Engine `spage_update_record_type`.
    pub fn set_record_type(&mut self, slotid: usize, rtype: RecordType) -> Result<(), SpageError> {
        let mut s = self.live(slotid).ok_or(SpageError::NoRecord)?;
        s.rtype = rtype as u32;
        self.set_slot(slotid, s);
        Ok(())
    }

    /// Engine `spage_insert`: the first `REC_DELETED_WILL_REUSE` slot when there is one, else a new slot at the
    /// end. Returns the slot id.
    pub fn insert(&mut self, rtype: RecordType, rec: &[u8]) -> Result<usize, SpageError> {
        let space0 = rec.len() + self.waste(rec.len());
        if rec.len() > self.max_record_size() || space0 as i32 > self.h(H_TOTAL_FREE) {
            return Err(SpageError::DoesntFit);
        }
        let n = self.num_slots();
        let slotid = if self.h(H_NUM_SLOTS) == self.h(H_NUM_RECORDS) {
            n
        } else {
            (0..n)
                .find(|&i| self.slot(i).rtype == RecordType::DeletedWillReuse as u32)
                .unwrap_or(n)
        };
        let mut space = space0;
        if slotid == n {
            space += SPAGE_SLOT_SIZE;
            if space as i32 > self.h(H_TOTAL_FREE) {
                return Err(SpageError::DoesntFit);
            }
        }
        if space as i32 > self.h(H_CONT_FREE) {
            self.compact()?;
        }
        if slotid == n {
            self.add_h(H_NUM_SLOTS, 1);
        }
        let at = self.h(H_OFFSET_TO_FREE_AREA) as usize;
        self.area[at..at + rec.len()].copy_from_slice(rec);
        self.set_slot(
            slotid,
            Slot {
                offset: at,
                length: rec.len(),
                rtype: rtype as u32,
            },
        );
        self.add_h(H_NUM_RECORDS, 1);
        self.add_h(H_TOTAL_FREE, -(space as i32));
        self.add_h(H_CONT_FREE, -(space as i32));
        self.add_h(H_OFFSET_TO_FREE_AREA, space0 as i32);
        Ok(slotid)
    }

    /// Engine `spage_delete`: anchored pages keep the slot (`REC_DELETED_WILL_REUSE`, or `REC_MARKDELETED` when
    /// slots are not reused); unanchored pages drop it, shifting later slots (keep-sequence) or moving the last
    /// slot into its place (any-sequence).
    pub fn delete(&mut self, slotid: usize) -> Result<(), SpageError> {
        let s = self.live(slotid).ok_or(SpageError::NoRecord)?;
        let free = (s.length + self.waste(s.length)) as i32;
        self.add_h(H_NUM_RECORDS, -1);
        self.add_h(H_TOTAL_FREE, free);
        if self.at_end(s) {
            self.add_h(H_CONT_FREE, free);
            self.add_h(H_OFFSET_TO_FREE_AREA, -free);
        }
        let empty = |rtype| Slot {
            offset: SPAGE_EMPTY_OFFSET,
            length: 0,
            rtype,
        };
        match self.h(H_ANCHOR) {
            1 => self.set_slot(slotid, empty(RecordType::DeletedWillReuse as u32)),
            2 => self.set_slot(slotid, empty(RecordType::MarkDeleted as u32)),
            3 | 4 => {
                let last = self.num_slots() - 1;
                if self.h(H_ANCHOR) == 3 {
                    let l = self.slot(last);
                    self.set_slot(slotid, l);
                } else {
                    for i in slotid..last {
                        let next = self.slot(i + 1);
                        self.set_slot(i, next);
                    }
                }
                self.set_slot(last, empty(RecordType::Unknown as u32));
                self.add_h(H_NUM_SLOTS, -1);
                self.add_h(H_TOTAL_FREE, SPAGE_SLOT_SIZE as i32);
                self.add_h(H_CONT_FREE, SPAGE_SLOT_SIZE as i32);
            }
            _ => return Err(SpageError::Corrupt),
        }
        Ok(())
    }

    /// Engine `spage_vacuum_slot` (develop :4826-4865, same in 11.4): the slot keeps its id and length, loses its
    /// offset, and becomes `REC_DELETED_WILL_REUSE` (`reusable`) or `REC_MARKDELETED`; only `total_free` and
    /// `num_records` change, so a [`Self::compact`] usually follows.
    pub fn vacuum_slot(&mut self, slotid: usize, reusable: bool) -> Result<(), SpageError> {
        let mut s = self.live(slotid).ok_or(SpageError::NoRecord)?;
        self.add_h(H_NUM_RECORDS, -1);
        self.add_h(H_TOTAL_FREE, (s.length + self.waste(s.length)) as i32);
        s.offset = SPAGE_EMPTY_OFFSET;
        s.rtype = if reusable {
            RecordType::DeletedWillReuse
        } else {
            RecordType::MarkDeleted
        } as u32;
        self.set_slot(slotid, s);
        Ok(())
    }

    /// Engine `spage_compact`: moves every record down in offset order so the free space is one area.
    pub fn compact(&mut self) -> Result<(), SpageError> {
        let n = self.num_slots();
        let mut live: Vec<usize> = (0..n)
            .filter(|&i| self.slot(i).offset != SPAGE_EMPTY_OFFSET)
            .collect();
        if live.len() != self.h(H_NUM_RECORDS) as usize {
            return Err(SpageError::Corrupt);
        }
        live.sort_by_key(|&i| self.slot(i).offset);
        let a = self.alignment();
        let mut to = SPAGE_HEADER_SIZE;
        for i in live {
            let mut s = self.slot(i);
            to = align_up(to, a);
            if to + s.length > self.area.len() {
                return Err(SpageError::Corrupt);
            }
            if to != s.offset {
                self.area.copy_within(s.offset..s.offset + s.length, to);
                s.offset = to;
                self.set_slot(i, s);
            }
            to += s.length;
        }
        to = align_up(to, a);
        let free = (self.area.len() - to - n * SPAGE_SLOT_SIZE) as i32;
        self.set_h(H_TOTAL_FREE, free);
        self.set_h(H_CONT_FREE, free);
        self.set_h(H_OFFSET_TO_FREE_AREA, to as i32);
        Ok(())
    }

    /// Clears 11.4's `need_update_best_hint` header bit, which develop treats as a reserved bit that
    /// `spage_initialize` writes as 0 (R1 § 3 R3). Returns whether it was set.
    pub fn clear_need_update_best_hint(&mut self) -> bool {
        let w = get_u32(self.area, H_BITS);
        put_u32(self.area, H_BITS, w & !NEED_UPDATE_BEST_HINT);
        w & NEED_UPDATE_BEST_HINT != 0
    }

    /// Header counters (`total_free`, `cont_free`, `offset_to_free_area`) as stored.
    pub fn free_counters(&self) -> (i32, i32, i32) {
        (
            self.h(H_TOTAL_FREE),
            self.h(H_CONT_FREE),
            self.h(H_OFFSET_TO_FREE_AREA),
        )
    }
}
