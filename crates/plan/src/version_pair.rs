//! The version pair of a migration: the source's release line and the target's, chosen once by the plan.
//!
//! Input: the source line (log header `db_compatibility`) and `--target-version`. Output: a [`VersionPair`] and, for
//! a pair the tool cannot migrate, the refusal. Later stages read the pair and never re-derive the target line.
//! Source: workspace#505 (target version `11.4|develop`, default develop, decided by the plan; relayed decision of
//! the map supervisor 2026-10-29); ADR 0003 (direct pairs only, no downgrade).

use fastmig_ondisk::VersionLine;

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
            (s, t) if s == t => None,
            (VersionLine::V11_4, VersionLine::Develop) => Some(
                "version pair 11.4 -> develop: catalog writing is not wired yet (workspace#504)"
                    .into(),
            ),
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
