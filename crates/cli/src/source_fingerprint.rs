//! The source fingerprint `<dest>/fastmig/source-fingerprint`: what migrate saw of the source, so finalize deletes
//! only an unchanged source and knows exactly which files are the source's.
//!
//! Input: the open source at migrate time (log header, volume paths and sizes, lob-base-path). Output: the file;
//! [`SourceFingerprint::read`] back and [`SourceFingerprint::changes`] against the source as it is now.
//! Format: one `key\tvalue...` per line: `db`, `databases-txt`, `log` (path, append_lsa, db_creation), `volume`
//! (size, path) per volume, `vol-path`, `log-path`, `lob-base` (absent without a lob-base-path).
//! Source: decision G1 workspace#481 (fingerprint = log header append_lsa and db_creation, size per volume), G6
//! workspace#488 10 (plus the sizes of the referenced LOB files: checked against the LOB manifest).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::plan::OpenSource;

pub const FILE_NAME: &str = "source-fingerprint";

#[derive(Debug, PartialEq, Eq)]
pub struct SourceFingerprint {
    pub db: String,
    pub databases_txt: PathBuf,
    pub log: PathBuf,
    pub append_lsa: String,
    pub db_creation: i64,
    /// `(size, path)` per volume.
    pub volumes: Vec<(u64, PathBuf)>,
    pub vol_path: PathBuf,
    pub log_path: PathBuf,
    pub lob_base: Option<PathBuf>,
}

fn bad(line: &str) -> io::Error {
    io::Error::other(format!("{FILE_NAME}: bad line {line:?}"))
}

impl SourceFingerprint {
    pub fn of(s: &OpenSource, databases_txt: &Path, lob_base: Option<&Path>) -> io::Result<Self> {
        let volumes = s
            .vols
            .volumes()
            .iter()
            .map(|v| Ok((fs::metadata(&v.path)?.len(), v.path.clone())))
            .collect::<io::Result<_>>()?;
        Ok(SourceFingerprint {
            db: s.entry.name.clone(),
            databases_txt: std::path::absolute(databases_txt)?,
            log: s.lgat_path.clone(),
            append_lsa: s.log.append_lsa.to_string(),
            db_creation: s.log.db_creation,
            volumes,
            vol_path: s.entry.vol_path.clone(),
            log_path: s.entry.log_path.clone(),
            lob_base: lob_base.map(Path::to_path_buf),
        })
    }

    pub fn text(&self) -> String {
        let mut t = format!(
            "db\t{}\ndatabases-txt\t{}\nlog\t{}\t{}\t{}\nvol-path\t{}\nlog-path\t{}\n",
            self.db,
            self.databases_txt.display(),
            self.log.display(),
            self.append_lsa,
            self.db_creation,
            self.vol_path.display(),
            self.log_path.display()
        );
        for (size, p) in &self.volumes {
            t.push_str(&format!("volume\t{size}\t{}\n", p.display()));
        }
        if let Some(b) = &self.lob_base {
            t.push_str(&format!("lob-base\t{}\n", b.display()));
        }
        t
    }

    pub fn read(path: &Path) -> io::Result<Self> {
        let mut f = SourceFingerprint {
            db: String::new(),
            databases_txt: PathBuf::new(),
            log: PathBuf::new(),
            append_lsa: String::new(),
            db_creation: 0,
            volumes: Vec::new(),
            vol_path: PathBuf::new(),
            log_path: PathBuf::new(),
            lob_base: None,
        };
        for line in fs::read_to_string(path)?.lines() {
            let v: Vec<&str> = line.split('\t').collect();
            match v.as_slice() {
                ["db", d] => f.db = d.to_string(),
                ["databases-txt", p] => f.databases_txt = PathBuf::from(p),
                ["log", p, lsa, c] => {
                    f.log = PathBuf::from(p);
                    f.append_lsa = lsa.to_string();
                    f.db_creation = c.parse().map_err(|_| bad(line))?;
                }
                ["vol-path", p] => f.vol_path = PathBuf::from(p),
                ["log-path", p] => f.log_path = PathBuf::from(p),
                ["volume", size, p] => f
                    .volumes
                    .push((size.parse().map_err(|_| bad(line))?, PathBuf::from(p))),
                ["lob-base", p] => f.lob_base = Some(PathBuf::from(p)),
                _ => return Err(bad(line)),
            }
        }
        if f.db.is_empty() || f.volumes.is_empty() {
            return Err(io::Error::other(format!("{}: incomplete", path.display())));
        }
        Ok(f)
    }

    /// What differs between this record and `now` (the source read again); empty when it is unchanged.
    pub fn changes(&self, now: &SourceFingerprint) -> Vec<String> {
        let mut d = Vec::new();
        if (&self.append_lsa, self.db_creation) != (&now.append_lsa, now.db_creation) {
            d.push(format!(
                "log header append_lsa/db_creation {}/{} is now {}/{}",
                self.append_lsa, self.db_creation, now.append_lsa, now.db_creation
            ));
        }
        if self.volumes != now.volumes {
            d.push(format!(
                "volumes {:?} are now {:?}",
                self.volumes, now.volumes
            ));
        }
        d
    }
}
