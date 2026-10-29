//! The version pair of a migration: the source's release line and the target's, chosen once by the plan.
//!
//! Input: the source version (log header `db_compatibility`, `db_release`) and `--target-version`. Output: a [`VersionPair`] and, for
//! a pair the tool cannot migrate, the refusal. Later stages read the pair and never re-derive the target line.
//! Source: workspace#505 (target version `11.4|develop`, default develop, decided by the plan; relayed decision of
//! the map supervisor 2026-10-29); ADR 0003 (direct pairs only, no downgrade); target 11.4 refused while the index
//! builder writes the develop layout only (map supervisor 2026-10-29, workspace#472 Not yet specified).

use fastmig_ondisk::{SourceVersion, VersionLine};

/// (source line, target line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VersionPair {
    pub source: VersionLine,
    pub target: VersionLine,
}

impl VersionPair {
    /// Why the tool refuses this pair; `None` when it migrates it.
    pub fn refusal(self) -> Option<String> {
        match (self.source, self.target) {
            // the index builder writes the develop B-tree layout only (map supervisor 2026-10-29)
            (_, VersionLine::V11_4) => Some(
                "target 11.4: 11.4 layout B-tree builder not implemented (workspace#472 Not yet specified)"
                    .into(),
            ),
            (s, t) if s == t => None,
            (VersionLine::V11_4, VersionLine::Develop) => None,
            (s, t) => Some(format!(
                "version pair {} -> {} is a downgrade (ADR 0003)",
                line_name(s),
                line_name(t)
            )),
        }
    }

    /// The pair whose catalog is regenerated and whose CHAR/NUMERIC/NCHAR records are re-encoded (workspace#482).
    pub fn regenerates_catalog(self) -> bool {
        (self.source, self.target) == (VersionLine::V11_4, VersionLine::Develop)
    }
}

/// The codec line of a source version, or why the tool does not migrate that version yet. Each refusal names the
/// ticket that opens the version (map supervisor 2026-10-30, workspace#522); the order is G7 workspace#514.
pub fn source_line(v: SourceVersion) -> Result<VersionLine, String> {
    v.line().ok_or_else(|| match v {
        SourceVersion::V10_2 => format!("source {} not supported yet (workspace#520)", v.name()),
        SourceVersion::V11_0 => format!("source {}: not in scope (workspace#514)", v.name()),
        SourceVersion::V11_2
        | SourceVersion::V11_3
        | SourceVersion::V11_4
        | SourceVersion::Develop => unreachable!("a line of its own"),
    })
}

/// The `--target-version` spelling of a line.
pub fn line_name(l: VersionLine) -> &'static str {
    match l {
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
