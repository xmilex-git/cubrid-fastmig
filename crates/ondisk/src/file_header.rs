//! The file header (`FILE_HEADER`, start of a file's header page): page and sector counts, file type, descriptor,
//! and where the file's tables sit; 11.4 (216 B) and develop (200 B) layouts.
//!
//! Input: a file header page (whole IO page) and the version line that wrote it. Output: [`FileHeader`], or the
//! header written back in a given line's layout.
//! Source: engine `src/storage/file_manager.c:90-149` (develop @5b1867174) and :91-165 (release/11.4 @46212adcd),
//! `file_manager.h` (`FILE_TYPE`, descriptors, `FILE_TABLESPACE`); CBRD-27308 (CUBRID/cubrid#7792) dropped the two
//! 11.4 cache fields at 184..196. R1 § 2.7, R5 § 2.4. Offsets 0..184 checked on T1 header pages (workspace#490).

use crate::ids::{Hfid, Oid, Vfid, Vpid};
use crate::le_field::{get_f32, get_i16, get_i32, get_i64, put_f32, put_i16, put_i32, put_i64};
use crate::page::IO_RESERVED_SIZE;
use crate::version_line::VersionLine;

/// Engine `FILE_TYPE`, same values in 11.4 and develop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(i32)]
pub enum FileType {
    Tracker = 0,
    Heap = 1,
    HeapReuseSlots = 2,
    MultipageObjectHeap = 3,
    Btree = 4,
    BtreeOverflowKey = 5,
    ExtendibleHash = 6,
    ExtendibleHashDirectory = 7,
    Catalog = 8,
    DroppedFiles = 9,
    VacuumData = 10,
    QueryArea = 11,
    Temp = 12,
    UnknownType = 13,
}

impl FileType {
    pub fn from_i32(v: i32) -> Option<Self> {
        use FileType::*;
        Some(match v {
            0 => Tracker,
            1 => Heap,
            2 => HeapReuseSlots,
            3 => MultipageObjectHeap,
            4 => Btree,
            5 => BtreeOverflowKey,
            6 => ExtendibleHash,
            7 => ExtendibleHashDirectory,
            8 => Catalog,
            9 => DroppedFiles,
            10 => VacuumData,
            11 => QueryArea,
            12 => Temp,
            13 => UnknownType,
            _ => return None,
        })
    }
}

/// Engine file flags (`FILE_FLAG_*`).
pub const FILE_FLAG_NUMERABLE: i32 = 0x1;
pub const FILE_FLAG_TEMPORARY: i32 = 0x2;
pub const FILE_FLAG_ENCRYPTED_MASK: i32 = 0xC;

/// Engine `FILE_DESCRIPTORS_SIZE`: the descriptor union at header offset 40.
pub const FILE_DESCRIPTORS_SIZE: usize = 64;

/// Field offsets inside `FILE_HEADER` (DB page offsets; same in both lines up to 184).
pub mod off {
    pub const TIME_CREATION: usize = 0;
    pub const SELF: usize = 8;
    pub const TABLESPACE: usize = 16;
    pub const DESCRIPTOR: usize = 40;
    pub const N_PAGE_TOTAL: usize = 104;
    pub const N_PAGE_USER: usize = 108;
    pub const N_PAGE_FTAB: usize = 112;
    pub const N_PAGE_FREE: usize = 116;
    pub const N_PAGE_MARK_DELETE: usize = 120;
    pub const N_SECTOR_TOTAL: usize = 124;
    pub const N_SECTOR_PARTIAL: usize = 128;
    pub const N_SECTOR_FULL: usize = 132;
    pub const N_SECTOR_EMPTY: usize = 136;
    pub const TYPE: usize = 140;
    pub const FILE_FLAGS: usize = 144;
    pub const VOLID_LAST_EXPAND: usize = 148;
    pub const OFFSET_TO_PARTIAL_FTAB: usize = 150;
    pub const OFFSET_TO_FULL_FTAB: usize = 152;
    pub const OFFSET_TO_USER_PAGE_FTAB: usize = 154;
    pub const VPID_STICKY_FIRST: usize = 156;
    pub const VPID_LAST_TEMP_ALLOC: usize = 164;
    pub const OFFSET_TO_LAST_TEMP_ALLOC: usize = 172;
    pub const VPID_LAST_USER_PAGE_FTAB: usize = 176;
    /// 11.4: `vpid_find_nth_last` (8) @184, `first_index_find_nth_last` @192, `reserved0..3` @196..212.
    /// develop: `reserved0..3` @184..200.
    pub const LINE_SPECIFIC: usize = 184;
}

/// Engine `FILE_TABLESPACE` (24 B at header offset 16): `initial_size` i64 @0, `expand_ratio` f32 @8,
/// `expand_min_size` i32 @12, `expand_max_size` i32 @16 (bytes).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tablespace {
    pub initial_size: i64,
    pub expand_ratio: f32,
    pub expand_min_size: i32,
    pub expand_max_size: i32,
}

/// The file descriptor union, decoded by file type (engine `FILE_DESCRIPTORS`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileDescriptor {
    /// `FILE_HEAP_DES`: `class_oid` @0, `hfid` @8.
    Heap { class_oid: Oid, hfid: Hfid },
    /// `FILE_OVF_HEAP_DES`: `hfid` @0, `class_oid` @12.
    HeapOverflow { hfid: Hfid, class_oid: Oid },
    /// `FILE_BTREE_DES` / `FILE_EHASH_DES`: `class_oid` @0, `attr_id` @8.
    ClassAttr { class_oid: Oid, attr_id: i32 },
    /// `FILE_OVF_BTREE_DES`: `btid` @0, `class_oid` @12.
    BtreeOverflow { btid: Hfid, class_oid: Oid },
    /// `FILE_VACUUM_DATA_DES`: `vpid_first` @0.
    VacuumData { vpid_first: Vpid },
    /// Any other type: the raw 64 bytes.
    Raw([u8; FILE_DESCRIPTORS_SIZE]),
}

/// Engine `FILE_HEADER`, decoded. The line-specific tail (184..216 in 11.4, 184..200 in develop) holds only
/// caches and reserved words; it is read as raw bytes and written as zeros.
#[derive(Clone, Debug, PartialEq)]
pub struct FileHeader {
    pub time_creation: i64,
    pub self_vfid: Vfid,
    pub tablespace: Tablespace,
    pub descriptor_raw: [u8; FILE_DESCRIPTORS_SIZE],
    pub n_page_total: i32,
    pub n_page_user: i32,
    pub n_page_ftab: i32,
    pub n_page_free: i32,
    pub n_page_mark_delete: i32,
    pub n_sector_total: i32,
    pub n_sector_partial: i32,
    pub n_sector_full: i32,
    pub n_sector_empty: i32,
    pub file_type_raw: i32,
    pub file_flags: i32,
    pub volid_last_expand: i16,
    pub offset_to_partial_ftab: i16,
    pub offset_to_full_ftab: i16,
    pub offset_to_user_page_ftab: i16,
    pub vpid_sticky_first: Vpid,
    pub vpid_last_temp_alloc: Vpid,
    pub offset_to_last_temp_alloc: i32,
    pub vpid_last_user_page_ftab: Vpid,
}

impl FileHeader {
    /// Decodes the header of a file header page. Fields 0..184 are line independent.
    pub fn read(page: &[u8]) -> Self {
        let h = &page[IO_RESERVED_SIZE..];
        let mut descriptor_raw = [0u8; FILE_DESCRIPTORS_SIZE];
        descriptor_raw
            .copy_from_slice(&h[off::DESCRIPTOR..off::DESCRIPTOR + FILE_DESCRIPTORS_SIZE]);
        let ts = off::TABLESPACE;
        FileHeader {
            time_creation: get_i64(h, off::TIME_CREATION),
            self_vfid: Vfid::read(h, off::SELF),
            tablespace: Tablespace {
                initial_size: get_i64(h, ts),
                expand_ratio: get_f32(h, ts + 8),
                expand_min_size: get_i32(h, ts + 12),
                expand_max_size: get_i32(h, ts + 16),
            },
            descriptor_raw,
            n_page_total: get_i32(h, off::N_PAGE_TOTAL),
            n_page_user: get_i32(h, off::N_PAGE_USER),
            n_page_ftab: get_i32(h, off::N_PAGE_FTAB),
            n_page_free: get_i32(h, off::N_PAGE_FREE),
            n_page_mark_delete: get_i32(h, off::N_PAGE_MARK_DELETE),
            n_sector_total: get_i32(h, off::N_SECTOR_TOTAL),
            n_sector_partial: get_i32(h, off::N_SECTOR_PARTIAL),
            n_sector_full: get_i32(h, off::N_SECTOR_FULL),
            n_sector_empty: get_i32(h, off::N_SECTOR_EMPTY),
            file_type_raw: get_i32(h, off::TYPE),
            file_flags: get_i32(h, off::FILE_FLAGS),
            volid_last_expand: get_i16(h, off::VOLID_LAST_EXPAND),
            offset_to_partial_ftab: get_i16(h, off::OFFSET_TO_PARTIAL_FTAB),
            offset_to_full_ftab: get_i16(h, off::OFFSET_TO_FULL_FTAB),
            offset_to_user_page_ftab: get_i16(h, off::OFFSET_TO_USER_PAGE_FTAB),
            vpid_sticky_first: Vpid::read(h, off::VPID_STICKY_FIRST),
            vpid_last_temp_alloc: Vpid::read(h, off::VPID_LAST_TEMP_ALLOC),
            offset_to_last_temp_alloc: get_i32(h, off::OFFSET_TO_LAST_TEMP_ALLOC),
            vpid_last_user_page_ftab: Vpid::read(h, off::VPID_LAST_USER_PAGE_FTAB),
        }
    }

    /// Writes every field, then zeroes `line`'s tail (develop: `reserved0..3`; 11.4: the find-nth cache and
    /// reserved words). Writing a copied 11.4 header in the develop layout zeroes 184..200 and leaves 200..216,
    /// which develop never reads (R1 § 3 R6). Table bytes are not touched.
    pub fn write(&self, page: &mut [u8], line: VersionLine) {
        let h = &mut page[IO_RESERVED_SIZE..];
        put_i64(h, off::TIME_CREATION, self.time_creation);
        self.self_vfid.write(h, off::SELF);
        let ts = off::TABLESPACE;
        h[ts..off::DESCRIPTOR].fill(0);
        put_i64(h, ts, self.tablespace.initial_size);
        put_f32(h, ts + 8, self.tablespace.expand_ratio);
        put_i32(h, ts + 12, self.tablespace.expand_min_size);
        put_i32(h, ts + 16, self.tablespace.expand_max_size);
        h[off::DESCRIPTOR..off::DESCRIPTOR + FILE_DESCRIPTORS_SIZE]
            .copy_from_slice(&self.descriptor_raw);
        put_i32(h, off::N_PAGE_TOTAL, self.n_page_total);
        put_i32(h, off::N_PAGE_USER, self.n_page_user);
        put_i32(h, off::N_PAGE_FTAB, self.n_page_ftab);
        put_i32(h, off::N_PAGE_FREE, self.n_page_free);
        put_i32(h, off::N_PAGE_MARK_DELETE, self.n_page_mark_delete);
        put_i32(h, off::N_SECTOR_TOTAL, self.n_sector_total);
        put_i32(h, off::N_SECTOR_PARTIAL, self.n_sector_partial);
        put_i32(h, off::N_SECTOR_FULL, self.n_sector_full);
        put_i32(h, off::N_SECTOR_EMPTY, self.n_sector_empty);
        put_i32(h, off::TYPE, self.file_type_raw);
        put_i32(h, off::FILE_FLAGS, self.file_flags);
        put_i16(h, off::VOLID_LAST_EXPAND, self.volid_last_expand);
        put_i16(h, off::OFFSET_TO_PARTIAL_FTAB, self.offset_to_partial_ftab);
        put_i16(h, off::OFFSET_TO_FULL_FTAB, self.offset_to_full_ftab);
        put_i16(
            h,
            off::OFFSET_TO_USER_PAGE_FTAB,
            self.offset_to_user_page_ftab,
        );
        self.vpid_sticky_first.write(h, off::VPID_STICKY_FIRST);
        self.vpid_last_temp_alloc
            .write(h, off::VPID_LAST_TEMP_ALLOC);
        put_i32(
            h,
            off::OFFSET_TO_LAST_TEMP_ALLOC,
            self.offset_to_last_temp_alloc,
        );
        self.vpid_last_user_page_ftab
            .write(h, off::VPID_LAST_USER_PAGE_FTAB);
        h[off::LINE_SPECIFIC..line.file_header_size()].fill(0);
    }

    pub fn file_type(&self) -> Option<FileType> {
        FileType::from_i32(self.file_type_raw)
    }

    pub fn is_numerable(&self) -> bool {
        self.file_flags & FILE_FLAG_NUMERABLE != 0
    }

    pub fn is_temporary(&self) -> bool {
        self.file_flags & FILE_FLAG_TEMPORARY != 0
    }

    pub fn is_tde_encrypted(&self) -> bool {
        self.file_flags & FILE_FLAG_ENCRYPTED_MASK != 0
    }

    pub fn descriptor(&self) -> FileDescriptor {
        let d = &self.descriptor_raw;
        match self.file_type() {
            Some(FileType::Heap | FileType::HeapReuseSlots) => FileDescriptor::Heap {
                class_oid: Oid::read(d, 0),
                hfid: Hfid::read(d, 8),
            },
            Some(FileType::MultipageObjectHeap) => FileDescriptor::HeapOverflow {
                hfid: Hfid::read(d, 0),
                class_oid: Oid::read(d, 12),
            },
            Some(
                FileType::Btree | FileType::ExtendibleHash | FileType::ExtendibleHashDirectory,
            ) => FileDescriptor::ClassAttr {
                class_oid: Oid::read(d, 0),
                attr_id: get_i32(d, 8),
            },
            Some(FileType::BtreeOverflowKey) => FileDescriptor::BtreeOverflow {
                btid: Hfid::read(d, 0),
                class_oid: Oid::read(d, 12),
            },
            Some(FileType::VacuumData) => FileDescriptor::VacuumData {
                vpid_first: Vpid::read(d, 0),
            },
            _ => FileDescriptor::Raw(*d),
        }
    }
}
