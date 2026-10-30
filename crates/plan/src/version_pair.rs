//! The version pair of a migration: the source's release line and the target's, chosen once by the plan.
//!
//! Input: the source version (log header `db_compatibility`, `db_release`) and `--target-version`. Output: a [`VersionPair`] and, for
//! a pair the tool cannot migrate, the refusal. Later stages read the pair and never re-derive the target line.
//! Source: workspace#505 (target version `11.4|develop`, default develop, decided by the plan; relayed decision of
//! the map supervisor 2026-10-29); ADR 0003 (direct pairs only, no downgrade); target 11.4 refused while the index
//! builder writes the develop layout only (map supervisor 2026-10-29); an 11.4 source to an 11.4 target since
//! workspace#530.

use fastmig_ondisk::{SourceVersion, VersionLine};

/// (source line, target line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VersionPair {
    pub source: VersionLine,
    pub target: VersionLine,
}

impl VersionPair {
    /// Why the tool refuses this pair from source `version`; `None` when it migrates it.
    pub fn refusal(self, version: SourceVersion) -> Option<String> {
        match (self.source, self.target) {
            // an 11.4 target: the same-version path only (11.4 B-tree layout workspace#530, catalog writer #533);
            // 10.2..11.3 read with an 11.4 codec line are not the same version
            (VersionLine::V11_4, VersionLine::V11_4) if version == SourceVersion::V11_4 => None,
            (_, VersionLine::V11_4) => Some(format!(
                "target 11.4 from source {}: only an 11.4 source is wired to an 11.4 target (workspace#472)",
                version.name()
            )),
            (s, t) if s == t => None,
            // 10.2: values (workspace#520), names (#544), TDE metadata (#515), system classes (#549), partition streams
            // (#540)
            (VersionLine::V10_2 | VersionLine::V11_4, VersionLine::Develop) => None,
            (s, t) => Some(format!(
                "version pair {} -> {} is a downgrade (ADR 0003)",
                line_name(s),
                line_name(t)
            )),
        }
    }

    /// The pair whose catalog is regenerated and whose CHAR/NUMERIC/NCHAR records (from 10.2 also LZO1X strings,
    /// workspace#520) are re-encoded (workspace#482).
    pub fn regenerates_catalog(self) -> bool {
        matches!(
            (self.source, self.target),
            (
                VersionLine::V10_2 | VersionLine::V11_4,
                VersionLine::Develop
            )
        )
    }
}

/// The codec line of a source version, or why the tool does not migrate that version yet. Each refusal names the
/// ticket that opens the version (map supervisor 2026-10-30, workspace#522); the order is G7 workspace#514.
pub fn source_line(v: SourceVersion) -> Result<VersionLine, String> {
    v.line().ok_or_else(|| match v {
        SourceVersion::V11_0 => format!("source {}: not in scope (workspace#514)", v.name()),
        SourceVersion::V10_2
        | SourceVersion::V11_2
        | SourceVersion::V11_3
        | SourceVersion::V11_4
        | SourceVersion::Develop => unreachable!("a line of its own"),
    })
}

/// The `--target-version` spelling of a line.
pub fn line_name(l: VersionLine) -> &'static str {
    match l {
        VersionLine::V10_2 => "10.2",
        VersionLine::V11_4 => "11.4",
        VersionLine::Develop => "develop",
    }
}

/// Parses `--target-version`.
pub fn parse_line(s: &str) -> Option<VersionLine> {
    [VersionLine::V11_4, VersionLine::Develop]
        .into_iter()
        .find(|l| line_name(*l) == s)
}
