//! The run log: the full text the screen only summarizes, in a file under the target (`<dest>/fastmig/migrate.log`).
//! Every warning in full, a progress line at each phase boundary and every interval (the `--progress plain` format,
//! [`crate::render_line`]), the closing summary line, and what the command adds at its end (the report path, an
//! error).
//!
//! Input: lines from the monitor and the command, and the file's path once the command has created its directory
//! (lines before that are held and written first). Output: the file, appended line by line.
//! A write error is latched and returned by [`RunLog::close`], not dropped: the command fails on it at its end.
//! Source: workspace#610 decision 8.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct RunLog {
    held: Vec<String>,
    file: Option<File>,
    path: Option<PathBuf>,
    error: Option<io::Error>,
}

impl RunLog {
    /// Writes `line` to the file, or holds it until [`RunLog::open`].
    pub fn line(&mut self, line: &str) {
        match &mut self.file {
            Some(f) if self.error.is_none() => {
                if let Err(e) = writeln!(f, "{line}") {
                    self.error = Some(e);
                }
            }
            Some(_) => {}
            None => self.held.push(line.to_string()),
        }
    }

    /// Creates the file at `path` (a rerun's old log is replaced) and writes the held lines.
    pub fn open(&mut self, path: &Path) -> io::Result<()> {
        let mut f = File::create(path)?;
        for l in self.held.drain(..) {
            writeln!(f, "{l}")?;
        }
        self.file = Some(f);
        self.path = Some(path.to_path_buf());
        Ok(())
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The first write error since the file was opened.
    pub fn close(&mut self) -> io::Result<()> {
        self.file = None;
        self.error.take().map_or(Ok(()), Err)
    }
}
