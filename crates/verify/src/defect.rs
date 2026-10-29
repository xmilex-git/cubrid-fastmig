//! One defect the verifier found, and the identity that matches it between the source and target runs.
//!
//! Input: what a check saw. Output: [`Defect`]; its [`Defect::id`] is the tuple (check, class unique name, index
//! name or `-`, OID or key, code) — placement preservation keeps OIDs, so the same defect in source and target has
//! the same id (decision G3 Q6, workspace#483).
//! Source: G3 workspace#483 decisions 6 and 9 (report fields: identity + VPID/slot + expected/actual).

use std::fmt;

/// The five checks of G3 (workspace#483; check 5 from G6 workspace#488).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Check {
    /// 1: heap chain walk, slotted pages, record homes, record decode.
    HeapChain,
    /// 2: B-tree structure and index ↔ heap (key, OID) merge, FK keys.
    IndexHeap,
    /// 3: catalog references and OBJECT values.
    Catalog,
    /// 4: volume sector ownership.
    Sector,
    /// 5: LOB locator → external file.
    Lob,
}

impl Check {
    pub fn number(self) -> u8 {
        match self {
            Check::HeapChain => 1,
            Check::IndexHeap => 2,
            Check::Catalog => 3,
            Check::Sector => 4,
            Check::Lob => 5,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Check::HeapChain => "heap",
            Check::IndexHeap => "index",
            Check::Catalog => "catalog",
            Check::Sector => "sector",
            Check::Lob => "lob",
        }
    }
}

/// One finding. `class`/`index` are `-` when the defect belongs to no class or index (a sector, a file).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Defect {
    pub check: Check,
    pub class: String,
    pub index: String,
    /// The object the defect is about: an OID `volid|pageid|slotid`, a key, a VPID, a file.
    pub object: String,
    /// Stable kebab-case code, e.g. `chain-unvisited-page`.
    pub code: &'static str,
    /// Where it was seen (`vpid 0|123 slot 4`); not part of the identity.
    pub at: String,
    pub expected: String,
    pub actual: String,
}

/// The identity of a defect across the source and target runs.
pub type DefectId = (Check, String, String, String, &'static str);

impl Defect {
    pub fn new(check: Check, code: &'static str) -> Self {
        Defect {
            check,
            class: "-".into(),
            index: "-".into(),
            object: "-".into(),
            code,
            at: String::new(),
            expected: String::new(),
            actual: String::new(),
        }
    }

    pub fn class(mut self, c: impl Into<String>) -> Self {
        self.class = c.into();
        self
    }

    pub fn index(mut self, i: impl Into<String>) -> Self {
        self.index = i.into();
        self
    }

    pub fn object(mut self, o: impl fmt::Display) -> Self {
        self.object = o.to_string();
        self
    }

    pub fn at(mut self, a: impl Into<String>) -> Self {
        self.at = a.into();
        self
    }

    pub fn expected(mut self, e: impl fmt::Display) -> Self {
        self.expected = e.to_string();
        self
    }

    pub fn actual(mut self, a: impl fmt::Display) -> Self {
        self.actual = a.to_string();
        self
    }

    pub fn id(&self) -> DefectId {
        (
            self.check,
            self.class.clone(),
            self.index.clone(),
            self.object.clone(),
            self.code,
        )
    }
}

/// `volid|pageid|slotid`, the form the engine writes OIDs in (`classobj_put_index` BTID strings, csql `@`).
pub struct OidText(pub fastmig_ondisk::Oid);

impl fmt::Display for OidText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}|{}|{}", self.0.volid, self.0.pageid, self.0.slotid)
    }
}

/// `volid|pageid`.
pub struct VpidText(pub fastmig_ondisk::Vpid);

impl fmt::Display for VpidText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}|{}", self.0.volid, self.0.pageid)
    }
}
