//! Slotted page: reads the slot directory of a page and builds a new page record by record.
//!
//! Input: the user area of one page (`page::db_area`: the IO page without the 32 B FILEIO reserved
//! header and the 8 B watermark; 16344 B for a 16 KiB IO page, sized at run time).
//! Output: (record type, record bytes) per slot; a freshly built user area for new pages.
//! Source: engine `slotted_page.c:spage_initialize`, `spage_find_empty_slot`, `spage_insert_data`,
//! `spage_max_space_for_new_record`, `spage_find_slot`, `spage_is_unknown_slot` (develop 5b1867174,
//! release/11.4 46212adcd); the builder follows the engine rules only for a page that starts empty and
//! only gains records, which is all a new page needs.

use zerocopy::little_endian::{I16, I32, U16, U32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// Engine `SPAGE_HEADER` (src/storage/slotted_page.h). Size: 32 B in 11.4 and develop, same offsets in both
/// (host order = little endian on x86-64; computed, checked on T1 B-tree pages).
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct SpageHeader {
    /// @0
    pub num_slots: I16,
    /// @2
    pub num_records: I16,
    /// @4, [`Anchor`] as stored.
    pub anchor_type: I16,
    /// @6
    pub alignment: U16,
    /// @8
    pub total_free: I32,
    /// @12
    pub cont_free: I32,
    /// @16
    pub offset_to_free_area: I32,
    /// @20
    pub reserved1: I32,
    /// @24
    pub flags: I32,
    /// @28, bit field: bit 0 `is_saving` in both; 11.4 also has bit 1 `need_update_best_hint` (heap pages only),
    /// develop drops it (reserved bits 31 instead of 30).
    pub bits: U32,
}

pub const SPAGE_HEADER_SIZE: usize = size_of::<SpageHeader>();
const _: () = assert!(SPAGE_HEADER_SIZE == 32);

/// Engine `SPAGE_SLOT`: one u32 at the end of the page, slot 0 last.
pub const SPAGE_SLOT_SIZE: usize = 4;

/// Engine `SPAGE_EMPTY_OFFSET`: offset of a slot whose record is gone.
const SPAGE_EMPTY_OFFSET: usize = 0;

/// 11.4 `need_update_best_hint` (bit 1 of [`SpageHeader::bits`]); develop's `reserved_bits`.
const NEED_UPDATE_BEST_HINT: u32 = 0x2;

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

/// One slot of the slot directory, decoded: where its record starts, how long it is, and the record type bits as
/// stored (4 bits; values 8..15 name no engine type, and the verifier must still see them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub offset: usize,
    pub length: usize,
    pub rtype: u32,
}

impl Slot {
    /// The record type, `None` for bits no engine type has.
    pub fn record_type(&self) -> Option<RecordType> {
        RecordType::from_bits(self.rtype)
    }

    /// Engine `SPAGE_SLOT` word: bits 0..14 offset to the record, 14..28 record length, 28..32 record type.
    fn from_word(w: u32) -> Self {
        Slot {
            offset: (w & 0x3fff) as usize,
            length: (w >> 14 & 0x3fff) as usize,
            rtype: w >> 28,
        }
    }

    fn word(&self) -> u32 {
        (self.offset as u32) | ((self.length as u32) << 14) | (self.rtype << 28)
    }
}

fn header(area: &[u8]) -> &SpageHeader {
    SpageHeader::ref_from_prefix(area)
        .expect("user area holds the header")
        .0
}

fn header_mut(area: &mut [u8]) -> &mut SpageHeader {
    SpageHeader::mut_from_prefix(area)
        .expect("user area holds the header")
        .0
}

fn align_up(n: usize, align: usize) -> usize {
    n.div_ceil(align) * align
}

fn slot_pos(user_area_len: usize, slotid: usize) -> usize {
    user_area_len - SPAGE_SLOT_SIZE * (slotid + 1)
}

fn slot_at(area: &[u8], slotid: usize) -> Slot {
    let pos = slot_pos(area.len(), slotid);
    Slot::from_word(u32::from_le_bytes(
        *area[pos..].first_chunk().expect("slot inside the area"),
    ))
}

fn set_slot_at(area: &mut [u8], slotid: usize, s: Slot) {
    let pos = slot_pos(area.len(), slotid);
    area[pos..pos + SPAGE_SLOT_SIZE].copy_from_slice(&s.word().to_le_bytes());
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

    /// The page header as stored.
    pub fn header(&self) -> &'a SpageHeader {
        header(self.area)
    }

    pub fn num_slots(&self) -> usize {
        self.header().num_slots.get().max(0) as usize
    }

    pub fn num_records(&self) -> usize {
        self.header().num_records.get().max(0) as usize
    }

    pub fn alignment(&self) -> usize {
        self.header().alignment.get() as usize
    }

    pub fn total_free(&self) -> i32 {
        self.header().total_free.get()
    }

    pub fn cont_free(&self) -> i32 {
        self.header().cont_free.get()
    }

    pub fn offset_to_free_area(&self) -> i32 {
        self.header().offset_to_free_area.get()
    }

    /// Engine `spage_max_space_for_new_record`: free space minus a new slot unless a deleted slot can be reused,
    /// aligned down.
    pub fn max_space_for_new_record(&self) -> usize {
        let h = self.header();
        let mut free = h.total_free.get();
        let reuse = h.anchor_type.get() != Anchor::AnchoredDontReuseSlots as i16
            && h.num_slots.get() > h.num_records.get();
        if !reuse {
            free -= SPAGE_SLOT_SIZE as i32;
            if free < SPAGE_SLOT_SIZE as i32 {
                free = 0;
            }
        }
        let a = (h.alignment.get() as i32).max(1);
        (free.max(0) / a * a) as usize
    }

    /// Slot `slotid` as stored, unchecked; `None` past `num_slots`.
    /// For the verifier, which must see the slots `record` treats as unknown.
    pub fn raw_slot(&self, slotid: usize) -> Option<Slot> {
        (slotid < self.num_slots()).then(|| slot_at(self.area, slotid))
    }

    /// Length of the user area this page view covers.
    pub fn area_len(&self) -> usize {
        self.area.len()
    }

    /// The record in `slotid` with its type, or `None` for a slot the engine treats as unknown
    /// (out of range, empty offset, offset inside the header) or one whose bytes overrun the page.
    pub fn record(&self, slotid: usize) -> Option<(RecordType, &'a [u8])> {
        let s = self.raw_slot(slotid)?;
        let rtype = s.record_type()?;
        if s.offset == SPAGE_EMPTY_OFFSET || s.offset < SPAGE_HEADER_SIZE {
            return None;
        }
        let end = s.offset.checked_add(s.length)?;
        if end > slot_pos(self.area.len(), self.num_slots() - 1) {
            return None;
        }
        Some((rtype, &self.area[s.offset..end]))
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
        let h = header_mut(area);
        h.anchor_type.set(anchor as i16);
        h.alignment.set(alignment as u16);
        h.bits.set(is_saving as u32);
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
        let slot = Slot {
            offset,
            length: rec.len(),
            rtype: rtype as u32,
        };
        set_slot_at(self.area, slotid, slot);
        self.num_slots += 1;
        self.total_free -= space + SPAGE_SLOT_SIZE;
        self.offset_to_free_area += space;
        Some(slotid)
    }

    /// Writes the header counters; the page is complete after this.
    pub fn finish(self) {
        let h = header_mut(self.area);
        h.num_slots.set(self.num_slots as i16);
        h.num_records.set(self.num_slots as i16);
        h.total_free.set(self.total_free as i32);
        // a page that only gained records has one contiguous free area
        h.cont_free.set(self.total_free as i32);
        h.offset_to_free_area.set(self.offset_to_free_area as i32);
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

/// The free-space counters of a page header as stored (`total_free`, `cont_free`, `offset_to_free_area`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FreeCounters {
    pub total_free: i32,
    pub cont_free: i32,
    pub offset_to_free_area: i32,
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

impl<'a> SpageEditor<'a> {
    /// Wraps the user area (`page::db_area_mut`) of a slotted page.
    pub fn new(area: &'a mut [u8]) -> Option<Self> {
        SlottedPage::new(area)?;
        Some(Self { area })
    }

    pub fn view(&self) -> SlottedPage<'_> {
        SlottedPage { area: self.area }
    }

    fn h(&self) -> &SpageHeader {
        header(self.area)
    }

    fn h_mut(&mut self) -> &mut SpageHeader {
        header_mut(self.area)
    }

    fn alignment(&self) -> usize {
        (self.h().alignment.get() as usize).max(1)
    }

    fn waste(&self, len: usize) -> usize {
        align_up(len, self.alignment()) - len
    }

    fn slot(&self, id: usize) -> Slot {
        slot_at(self.area, id)
    }

    fn set_slot(&mut self, id: usize, s: Slot) {
        set_slot_at(self.area, id, s);
    }

    /// `num_slots` as stored; a negative count is not clamped here (the editor's callers checked the page).
    fn num_slots(&self) -> usize {
        self.h().num_slots.get() as usize
    }

    fn live(&self, id: usize) -> Option<Slot> {
        (id < self.num_slots())
            .then(|| self.slot(id))
            .filter(|s| s.offset != SPAGE_EMPTY_OFFSET)
    }

    fn at_end(&self, s: Slot) -> bool {
        s.offset + s.length + self.waste(s.length) == self.h().offset_to_free_area.get() as usize
    }

    /// Engine `spage_max_record_size`: the largest record a page can hold.
    pub fn max_record_size(&self) -> usize {
        self.area.len() - SPAGE_HEADER_SIZE - SPAGE_SLOT_SIZE
    }

    /// Engine `spage_max_space_for_new_record`.
    pub fn max_space_for_new_record(&self) -> usize {
        self.view().max_space_for_new_record()
    }

    /// Whether `slotid`'s record can be rewritten with `len` bytes in this page (engine `spage_is_updatable`).
    pub fn can_update(&self, slotid: usize, len: usize) -> bool {
        self.live(slotid).is_some_and(|s| {
            len <= self.max_record_size()
                && (len + self.waste(len)) as i32 - (s.length + self.waste(s.length)) as i32
                    <= self.h().total_free.get()
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
            let h = self.h_mut();
            h.total_free -= space;
            if at_end {
                h.cont_free -= space;
                h.offset_to_free_area += space;
            }
            return Ok(());
        }
        let mut space = space;
        if self.at_end(s) && space <= self.h().cont_free.get() {
            // give the record's own bytes back to the free area, then append
            let give = (old_w + s.length) as i32;
            let h = self.h_mut();
            h.total_free += give;
            h.cont_free += give;
            h.offset_to_free_area -= give;
            space = (rec.len() + new_w) as i32;
        } else if (rec.len() + new_w) as i32 > self.h().cont_free.get() {
            let old = s;
            s.offset = SPAGE_EMPTY_OFFSET;
            self.set_slot(slotid, s);
            let freed = (old_w + old.length) as i32;
            self.h_mut().total_free += freed;
            self.h_mut().num_records -= 1;
            if self.compact().is_err() {
                self.set_slot(slotid, old);
                self.h_mut().total_free -= freed;
                self.h_mut().num_records += 1;
                return Err(SpageError::Corrupt);
            }
            self.h_mut().num_records += 1;
            space = (rec.len() + new_w) as i32;
        }
        let at = self.h().offset_to_free_area.get() as usize;
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
        let h = self.h_mut();
        h.total_free -= space;
        h.cont_free -= (rec.len() + new_w) as i32;
        h.offset_to_free_area += (rec.len() + new_w) as i32;
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
        if rec.len() > self.max_record_size() || space0 as i32 > self.h().total_free.get() {
            return Err(SpageError::DoesntFit);
        }
        let n = self.num_slots();
        let slotid = if self.h().num_slots == self.h().num_records {
            n
        } else {
            (0..n)
                .find(|&i| self.slot(i).rtype == RecordType::DeletedWillReuse as u32)
                .unwrap_or(n)
        };
        let mut space = space0;
        if slotid == n {
            space += SPAGE_SLOT_SIZE;
            if space as i32 > self.h().total_free.get() {
                return Err(SpageError::DoesntFit);
            }
        }
        if space as i32 > self.h().cont_free.get() {
            self.compact()?;
        }
        if slotid == n {
            self.h_mut().num_slots += 1;
        }
        let at = self.h().offset_to_free_area.get() as usize;
        self.area[at..at + rec.len()].copy_from_slice(rec);
        self.set_slot(
            slotid,
            Slot {
                offset: at,
                length: rec.len(),
                rtype: rtype as u32,
            },
        );
        let h = self.h_mut();
        h.num_records += 1;
        h.total_free -= space as i32;
        h.cont_free -= space as i32;
        h.offset_to_free_area += space0 as i32;
        Ok(slotid)
    }

    /// Engine `spage_delete`: anchored pages keep the slot (`REC_DELETED_WILL_REUSE`, or `REC_MARKDELETED` when
    /// slots are not reused); unanchored pages drop it, shifting later slots (keep-sequence) or moving the last
    /// slot into its place (any-sequence).
    pub fn delete(&mut self, slotid: usize) -> Result<(), SpageError> {
        let s = self.live(slotid).ok_or(SpageError::NoRecord)?;
        let free = (s.length + self.waste(s.length)) as i32;
        let at_end = self.at_end(s);
        let h = self.h_mut();
        h.num_records -= 1;
        h.total_free += free;
        if at_end {
            h.cont_free += free;
            h.offset_to_free_area -= free;
        }
        let empty = |rtype| Slot {
            offset: SPAGE_EMPTY_OFFSET,
            length: 0,
            rtype,
        };
        match self.h().anchor_type.get() {
            1 => self.set_slot(slotid, empty(RecordType::DeletedWillReuse as u32)),
            2 => self.set_slot(slotid, empty(RecordType::MarkDeleted as u32)),
            anchor @ (3 | 4) => {
                let last = self.num_slots() - 1;
                if anchor == 3 {
                    let l = self.slot(last);
                    self.set_slot(slotid, l);
                } else {
                    for i in slotid..last {
                        let next = self.slot(i + 1);
                        self.set_slot(i, next);
                    }
                }
                self.set_slot(last, empty(RecordType::Unknown as u32));
                let h = self.h_mut();
                h.num_slots -= 1;
                h.total_free += SPAGE_SLOT_SIZE as i32;
                h.cont_free += SPAGE_SLOT_SIZE as i32;
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
        let freed = (s.length + self.waste(s.length)) as i32;
        let h = self.h_mut();
        h.num_records -= 1;
        h.total_free += freed;
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
        if live.len() != self.h().num_records.get() as usize {
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
        let h = self.h_mut();
        h.total_free.set(free);
        h.cont_free.set(free);
        h.offset_to_free_area.set(to as i32);
        Ok(())
    }

    /// Clears 11.4's `need_update_best_hint` header bit, which develop treats as a reserved bit that
    /// `spage_initialize` writes as 0 (R1 § 3 R3). Returns whether it was set.
    pub fn clear_need_update_best_hint(&mut self) -> bool {
        let bits = &mut self.h_mut().bits;
        let w = bits.get();
        bits.set(w & !NEED_UPDATE_BEST_HINT);
        w & NEED_UPDATE_BEST_HINT != 0
    }

    /// Header free-space counters as stored.
    pub fn free_counters(&self) -> FreeCounters {
        let h = self.h();
        FreeCounters {
            total_free: h.total_free.get(),
            cont_free: h.cont_free.get(),
            offset_to_free_area: h.offset_to_free_area.get(),
        }
    }
}
