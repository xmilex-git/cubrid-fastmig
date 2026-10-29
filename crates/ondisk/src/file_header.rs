//! The file header (`FILE_HEADER`, start of a file's header page): page and sector counts, file type, descriptor,
//! and where the file's tables sit; 11.4 (216 B) and develop (200 B) layouts.
//!
//! Input: a file header page (whole IO page) and the version line that wrote it. Output: [`FileHeader`], or the
//! header written back in a given line's layout.
//! Source: engine `src/storage/file_manager.c:90-149` (develop @5b1867174) and :91-165 (release/11.4 @46212adcd),
//! `file_manager.h` (`FILE_TYPE`, descriptors, `FILE_TABLESPACE`); CBRD-27308 (CUBRID/cubrid#7792) dropped the two
//! 11.4 cache fields at 184..196. R1 § 2.7, R5 § 2.4. Offsets 0..184 checked on T1 header pages (workspace#490).

use zerocopy::little_endian::{F32, I16, I32, I64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::ids::{FileAndPageLe, Hfid, Oid, OidLe, Vfid, VfidLe, Vpid, VpidLe};
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

/// Engine `FILE_HEADER` up to the line-specific tail, at the start of the DB page; same in both lines.
/// The tail at 184 is 11.4 `vpid_find_nth_last` (8) @184, `first_index_find_nth_last` @192, `reserved0..3`
/// @196..212 (216 B in all), develop `reserved0..3` @184..200 (200 B); both hold only caches and reserved words.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct FileHeaderLe {
    /// @0
    pub time_creation: I64,
    /// @8
    pub self_vfid: VfidLe,
    /// @16
    pub tablespace: TablespaceLe,
    /// @40
    pub descriptor: [u8; FILE_DESCRIPTORS_SIZE],
    /// @104
    pub n_page_total: I32,
    /// @108
    pub n_page_user: I32,
    /// @112
    pub n_page_ftab: I32,
    /// @116
    pub n_page_free: I32,
    /// @120
    pub n_page_mark_delete: I32,
    /// @124
    pub n_sector_total: I32,
    /// @128
    pub n_sector_partial: I32,
    /// @132
    pub n_sector_full: I32,
    /// @136
    pub n_sector_empty: I32,
    /// @140
    pub file_type: I32,
    /// @144
    pub file_flags: I32,
    /// @148
    pub volid_last_expand: I16,
    /// @150
    pub offset_to_partial_ftab: I16,
    /// @152
    pub offset_to_full_ftab: I16,
    /// @154
    pub offset_to_user_page_ftab: I16,
    /// @156
    pub vpid_sticky_first: VpidLe,
    /// @164
    pub vpid_last_temp_alloc: VpidLe,
    /// @172
    pub offset_to_last_temp_alloc: I32,
    /// @176
    pub vpid_last_user_page_ftab: VpidLe,
}

/// Offset of the line-specific tail.
const LINE_SPECIFIC: usize = size_of::<FileHeaderLe>();
const _: () = assert!(LINE_SPECIFIC == 184);

/// Engine `FILE_TABLESPACE` as stored (24 B at header offset 16).
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct TablespaceLe {
    /// @0
    pub initial_size: I64,
    /// @8
    pub expand_ratio: F32,
    /// @12
    pub expand_min_size: I32,
    /// @16
    pub expand_max_size: I32,
    pad: [u8; 4],
}

/// Engine `FILE_HEAP_DES`.
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct HeapDes {
    class_oid: OidLe,
    hfid: FileAndPageLe,
}

/// Engine `FILE_OVF_HEAP_DES` and `FILE_OVF_BTREE_DES` (an HFID or a BTID, then the class).
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct OverflowDes {
    owner: FileAndPageLe,
    class_oid: OidLe,
}

/// Engine `FILE_BTREE_DES` and `FILE_EHASH_DES`.
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct ClassAttrDes {
    class_oid: OidLe,
    attr_id: I32,
}

/// Engine `FILE_VACUUM_DATA_DES`.
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct VacuumDataDes {
    vpid_first: VpidLe,
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
        let h = FileHeaderLe::ref_from_prefix(&page[IO_RESERVED_SIZE..])
            .expect("a file header page holds the header")
            .0;
        let ts = &h.tablespace;
        FileHeader {
            time_creation: h.time_creation.get(),
            self_vfid: h.self_vfid.get(),
            tablespace: Tablespace {
                initial_size: ts.initial_size.get(),
                expand_ratio: ts.expand_ratio.get(),
                expand_min_size: ts.expand_min_size.get(),
                expand_max_size: ts.expand_max_size.get(),
            },
            descriptor_raw: h.descriptor,
            n_page_total: h.n_page_total.get(),
            n_page_user: h.n_page_user.get(),
            n_page_ftab: h.n_page_ftab.get(),
            n_page_free: h.n_page_free.get(),
            n_page_mark_delete: h.n_page_mark_delete.get(),
            n_sector_total: h.n_sector_total.get(),
            n_sector_partial: h.n_sector_partial.get(),
            n_sector_full: h.n_sector_full.get(),
            n_sector_empty: h.n_sector_empty.get(),
            file_type_raw: h.file_type.get(),
            file_flags: h.file_flags.get(),
            volid_last_expand: h.volid_last_expand.get(),
            offset_to_partial_ftab: h.offset_to_partial_ftab.get(),
            offset_to_full_ftab: h.offset_to_full_ftab.get(),
            offset_to_user_page_ftab: h.offset_to_user_page_ftab.get(),
            vpid_sticky_first: h.vpid_sticky_first.get(),
            vpid_last_temp_alloc: h.vpid_last_temp_alloc.get(),
            offset_to_last_temp_alloc: h.offset_to_last_temp_alloc.get(),
            vpid_last_user_page_ftab: h.vpid_last_user_page_ftab.get(),
        }
    }

    /// Writes every field, then zeroes `line`'s tail (develop: `reserved0..3`; 11.4: the find-nth cache and
    /// reserved words). Writing a copied 11.4 header in the develop layout zeroes 184..200 and leaves 200..216,
    /// which develop never reads (R1 § 3 R6). Table bytes are not touched.
    pub fn write(&self, page: &mut [u8], line: VersionLine) {
        let db = &mut page[IO_RESERVED_SIZE..];
        let le = FileHeaderLe {
            time_creation: self.time_creation.into(),
            self_vfid: self.self_vfid.into(),
            tablespace: TablespaceLe {
                initial_size: self.tablespace.initial_size.into(),
                expand_ratio: self.tablespace.expand_ratio.into(),
                expand_min_size: self.tablespace.expand_min_size.into(),
                expand_max_size: self.tablespace.expand_max_size.into(),
                pad: [0; 4],
            },
            descriptor: self.descriptor_raw,
            n_page_total: self.n_page_total.into(),
            n_page_user: self.n_page_user.into(),
            n_page_ftab: self.n_page_ftab.into(),
            n_page_free: self.n_page_free.into(),
            n_page_mark_delete: self.n_page_mark_delete.into(),
            n_sector_total: self.n_sector_total.into(),
            n_sector_partial: self.n_sector_partial.into(),
            n_sector_full: self.n_sector_full.into(),
            n_sector_empty: self.n_sector_empty.into(),
            file_type: self.file_type_raw.into(),
            file_flags: self.file_flags.into(),
            volid_last_expand: self.volid_last_expand.into(),
            offset_to_partial_ftab: self.offset_to_partial_ftab.into(),
            offset_to_full_ftab: self.offset_to_full_ftab.into(),
            offset_to_user_page_ftab: self.offset_to_user_page_ftab.into(),
            vpid_sticky_first: self.vpid_sticky_first.into(),
            vpid_last_temp_alloc: self.vpid_last_temp_alloc.into(),
            offset_to_last_temp_alloc: self.offset_to_last_temp_alloc.into(),
            vpid_last_user_page_ftab: self.vpid_last_user_page_ftab.into(),
        };
        le.write_to_prefix(db)
            .expect("a file header page holds the header");
        db[LINE_SPECIFIC..line.file_header_size()].fill(0);
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
        let d = &self.descriptor_raw[..];
        let read = "a descriptor fits in the 64-byte union";
        match self.file_type() {
            Some(FileType::Heap | FileType::HeapReuseSlots) => {
                let des = HeapDes::ref_from_prefix(d).expect(read).0;
                FileDescriptor::Heap {
                    class_oid: des.class_oid.get(),
                    hfid: des.hfid.get(),
                }
            }
            Some(FileType::MultipageObjectHeap) => {
                let des = OverflowDes::ref_from_prefix(d).expect(read).0;
                FileDescriptor::HeapOverflow {
                    hfid: des.owner.get(),
                    class_oid: des.class_oid.get(),
                }
            }
            Some(
                FileType::Btree | FileType::ExtendibleHash | FileType::ExtendibleHashDirectory,
            ) => {
                let des = ClassAttrDes::ref_from_prefix(d).expect(read).0;
                FileDescriptor::ClassAttr {
                    class_oid: des.class_oid.get(),
                    attr_id: des.attr_id.get(),
                }
            }
            Some(FileType::BtreeOverflowKey) => {
                let des = OverflowDes::ref_from_prefix(d).expect(read).0;
                FileDescriptor::BtreeOverflow {
                    btid: des.owner.get(),
                    class_oid: des.class_oid.get(),
                }
            }
            Some(FileType::VacuumData) => FileDescriptor::VacuumData {
                vpid_first: VacuumDataDes::ref_from_prefix(d)
                    .expect(read)
                    .0
                    .vpid_first
                    .get(),
            },
            _ => FileDescriptor::Raw(self.descriptor_raw),
        }
    }
}
