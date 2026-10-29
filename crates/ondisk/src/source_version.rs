//! The source version: which engine release wrote a source database, told from its active log header.
//!
//! Input: the header page of `<db>_lgat` (or a decoded header's `db_compatibility` and `db_release`). Output: a
//! [`SourceVersion`], its header layout, and the codec line it is read with once that line is opened.
//! Source: engine `src/base/release_string.c` `disk_compatibility_level` (10.15f release/10.2 @416bb667c, 11.0f
//! release/11.0 @44ea22369 and 11.1, 11.2f release/11.2 @baf1d297d and release/11.3 @8acbed10e, 11.4f release/11.4
//! @46212adcd, 11.5f develop); `log_storage.hpp` `struct log_header` (376 B in 10.2..11.3, 384 B from 11.4,
//! CBRD-25365 44c022c31 added `vol_creation`); R8 § 2.1, § 2.14, D1 (workspace#513). Offsets checked on a createdb
//! `_lgat` of each install 10.2.18, 11.0.16, 11.2.9, 11.3.6, 11.4.6 and develop (workspace#522).

use crate::VersionLine;
use crate::le_field::try_get_f32;
use crate::log_page::LOG_PAGE_HEADER_SIZE;
use crate::value::{DecodeError, DecodeResult};

/// The release that wrote a source database. 11.2 and 11.3 share `db_compatibility` 11.2f and differ by
/// `db_release` (11.3 added DEDUPLICATE without raising the level, R8 § 1.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceVersion {
    /// 10.2.x, `db_compatibility` 10.15.
    V10_2,
    /// 11.0.x and 11.1.x, `db_compatibility` 11.0 (11.1 has no layout of its own, R8 § 1.9).
    V11_0,
    /// 11.2.x, `db_compatibility` 11.2.
    V11_2,
    /// 11.3.x, `db_compatibility` 11.2.
    V11_3,
    /// 11.4.x.
    V11_4,
    /// develop (11.5.0).
    Develop,
}

/// The volume, log, B-tree and MIDXKEY header layout a source version wrote (all changed together at 11.4, R8 § 2.3,
/// § 2.13, § 2.14; MIDXKEY offset table CBRD-24988 b89ccabe5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderLayout {
    /// 10.2..11.3: volume header 128 B and log header 376 B, no `vol_creation`; B-tree node header 28 B, root header
    /// fixed part 80 B with 32-bit counts; MIDXKEY key images without an offset table.
    Before11_4,
    /// 11.4 and develop: volume header 136 B, log header 384 B, B-tree node header 32 B, root header 88 B.
    Since11_4,
}

/// Log header offsets of the two fields that tell the version, per layout (add [`LOG_PAGE_HEADER_SIZE`]).
mod off {
    pub const DB_RELEASE_BEFORE_11_4: usize = 40;
    pub const DB_COMPATIBILITY_BEFORE_11_4: usize = 56;
    pub const DB_COMPATIBILITY_SINCE_11_4: usize = 64;
    /// Engine `REL_MAX_RELEASE_LENGTH`.
    pub const DB_RELEASE_LEN: usize = 15;
}

impl SourceVersion {
    /// The version a decoded header names: `db_compatibility`, and for 11.2f also the `db_release` prefix. `None` for
    /// any other pair.
    pub fn identify(db_compatibility: f32, db_release: &str) -> Option<Self> {
        let release = |p: &str| db_release.starts_with(p);
        match db_compatibility {
            10.15 if release("10.2.") => Some(SourceVersion::V10_2),
            11.0 if release("11.0.") || release("11.1.") => Some(SourceVersion::V11_0),
            11.2 if release("11.2.") => Some(SourceVersion::V11_2),
            11.2 if release("11.3.") => Some(SourceVersion::V11_3),
            v => VersionLine::from_db_compatibility(v).map(Self::of_line),
        }
    }

    fn of_line(l: VersionLine) -> Self {
        match l {
            VersionLine::V11_4 => SourceVersion::V11_4,
            VersionLine::Develop => SourceVersion::Develop,
        }
    }

    /// Tells the version from the raw header page of an active log: `db_compatibility` at +64 names 11.4 or
    /// develop; otherwise the 376 B layout's +56 (with `db_release` at +40) names an older release. In the 384 B layout
    /// +56 lies inside `db_release` and in the 376 B layout +64 is `is_shutdown` and padding, so neither reading can
    /// pass for the other.
    pub fn detect(page: &[u8]) -> DecodeResult<Self> {
        let h = page.get(LOG_PAGE_HEADER_SIZE..).unwrap_or_default();
        let since = try_get_f32(h, off::DB_COMPATIBILITY_SINCE_11_4)?;
        if let Some(l) = VersionLine::from_db_compatibility(since) {
            return Ok(Self::of_line(l));
        }
        let before = try_get_f32(h, off::DB_COMPATIBILITY_BEFORE_11_4)?;
        let release = h
            .get(off::DB_RELEASE_BEFORE_11_4..off::DB_RELEASE_BEFORE_11_4 + off::DB_RELEASE_LEN)
            .ok_or_else(|| DecodeError {
                at: LOG_PAGE_HEADER_SIZE + off::DB_RELEASE_BEFORE_11_4,
                what: "log header db_release past the end of the page".into(),
            })?;
        let release =
            String::from_utf8_lossy(release.split(|&b| b == 0).next().unwrap_or_default());
        Self::identify(before, &release).ok_or_else(|| DecodeError {
            at: LOG_PAGE_HEADER_SIZE + off::DB_COMPATIBILITY_BEFORE_11_4,
            what: format!(
                "unknown source: db_compatibility {since} (384 B layout) / {before} with db_release {release:?} \
                 (376 B layout)"
            ),
        })
    }

    pub fn header_layout(self) -> HeaderLayout {
        match self {
            SourceVersion::V10_2
            | SourceVersion::V11_0
            | SourceVersion::V11_2
            | SourceVersion::V11_3 => HeaderLayout::Before11_4,
            SourceVersion::V11_4 | SourceVersion::Develop => HeaderLayout::Since11_4,
        }
    }

    /// The codec line the source's pages are read with; `None` until that version is opened (10.2 by
    /// workspace#520; 11.0 is not in scope, workspace#514). 11.2 and 11.3 read with the 11.4 codecs: values, records,
    /// class objects, heap headers, file headers and partition streams are byte-identical to 11.4 (R8 § 2.6-2.9,
    /// § 2.12); what differs is decided on the version itself (headers, system tables; workspace#529).
    pub fn line(self) -> Option<VersionLine> {
        match self {
            SourceVersion::V11_2 | SourceVersion::V11_3 | SourceVersion::V11_4 => {
                Some(VersionLine::V11_4)
            }
            SourceVersion::Develop => Some(VersionLine::Develop),
            SourceVersion::V10_2 | SourceVersion::V11_0 => None,
        }
    }

    /// The name used in messages and reports.
    pub fn name(self) -> &'static str {
        match self {
            SourceVersion::V10_2 => "10.2",
            SourceVersion::V11_0 => "11.0",
            SourceVersion::V11_2 => "11.2",
            SourceVersion::V11_3 => "11.3",
            SourceVersion::V11_4 => "11.4",
            SourceVersion::Develop => "develop",
        }
    }
}
