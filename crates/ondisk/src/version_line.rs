//! The engine release lines whose on-disk layouts this crate knows, and the per-line constants that differ.
//!
//! Input: a release line (or the `db_compatibility` float read from a log header). Output: that line's constants.
//! Source: engine `src/base/release_string.c:105` (`disk_compatibility_level`: 11.4f in release/11.4 @46212adcd,
//! 11.5f in develop @5b1867174), `src/object/system_metadata_version.h:32` (develop `SYSTEM_METADATA_VERSION` 1),
//! `CMakeLists.txt` VERSION (develop 11.5.0), `file_manager.c` `FILE_HEADER_ALIGNED_SIZE`; R1 § 2.1, R5 § 2.4/2.7.

/// A release line with a known on-disk layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VersionLine {
    /// release/11.4 (11.4.x).
    V11_4,
    /// develop (11.5.0).
    Develop,
}

impl VersionLine {
    /// Log header `db_compatibility`.
    pub fn db_compatibility(self) -> f32 {
        match self {
            VersionLine::V11_4 => 11.4,
            VersionLine::Develop => 11.5,
        }
    }

    /// Log header `db_release` this tool writes for a target of this line. 11.4: the T1 baseline install (11.4.6).
    pub fn db_release(self) -> &'static str {
        match self {
            VersionLine::V11_4 => "11.4.6",
            VersionLine::Develop => "11.5.0",
        }
    }

    /// Log header bytes 102..104: 11.4 has `dummy3`/`dummy4` (0); develop has `sysmeta_version`, which must equal
    /// `SYSTEM_METADATA_VERSION` or boot refuses (`ER_SYSMETA_UPGRADE_REQUIRED`, boot_sr.c:2608).
    pub fn sysmeta_version(self) -> u16 {
        match self {
            VersionLine::V11_4 => 0,
            VersionLine::Develop => 1,
        }
    }

    /// Engine `FILE_HEADER_ALIGNED_SIZE`: 216 B in 11.4, 200 B in develop (CBRD-27308, CUBRID/cubrid#7792).
    pub fn file_header_size(self) -> usize {
        match self {
            VersionLine::V11_4 => 216,
            VersionLine::Develop => 200,
        }
    }

    /// The line a log header's `db_compatibility` names; `None` for any other value (refused by pre-check).
    pub fn from_db_compatibility(v: f32) -> Option<Self> {
        [VersionLine::V11_4, VersionLine::Develop]
            .into_iter()
            .find(|l| l.db_compatibility().to_bits() == v.to_bits())
    }
}
