//! `cubrid-fastmig set-comparison`: the set comparison of a source and a target `unloaddb` output.
//!
//! Input: `<source unload prefix> <target unload prefix>` (`<dir>/<db name>` as `unloaddb -O <dir>` wrote it) and
//! `--diff <file>` for every differing entry. Output: one line per unload file (entries on each side, entries only
//! on one side), the first differing entries, and the exit code: 0 = the two sets hold the same entries with the
//! same counts, order ignored; 1 = they differ (a file on one side only counts as differing); 2 = usage or I/O
//! error. What an entry is: `unload_files`.
//! Source: G3 workspace#483 and ADR 0007 (the set comparison is the pass oracle independent of the verifier);
//! `--source-version`/`--target-version` name the two engine lines; where they print the same content differently
//! the rules of `unload_normalize` rewrite the entries first, and the entries each rule changed are printed.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;

use crate::args::{CliError, parse};
use fastmig_ondisk::{SourceVersion, VersionLine};

use crate::unload_files::{entries, unload_set};
use crate::unload_normalize::{normalize, official_rules, rules};
use crate::unload_rules_10_2::ClassOwners;

const HELP: &str = "\
cubrid-fastmig set-comparison <source-unload-prefix> <target-unload-prefix> [--diff <file>]
  <prefix>         <dir>/<db name> as unloaddb -O <dir> wrote it (files <prefix>_schema, _objects, ...)
  --diff <file>    write every differing entry (< source only, > target only)
  --source-version <10.2|11.2|11.3|11.4|develop> --target-version <11.4|develop>
                   the engine lines that wrote each side; both or neither (default: one line, no normalization);
                   a 10.2 source is compared with its own unload, as the other sources
  --rules official the two unloads were written by one install: the official path's target (first) and the
                   migrated target (second); with --source-version/--target-version naming the migrated pair,
                   only that pair's intended differences are masked (workspace#600)
  exit 0 = same entries, 1 = differ, 2 = usage or I/O error";

/// Differing entries printed per file on stdout; `--diff` has all of them.
const SHOWN: usize = 10;

pub fn run(args: &[String]) -> Result<u8, CliError> {
    if args.iter().any(|a| a == "--help") {
        println!("{HELP}");
        return Ok(0);
    }
    let p = parse(
        args,
        &["--diff", "--source-version", "--target-version", "--rules"],
        &[],
    )?;
    let mut source_10_2 = false;
    let pair = match (
        p.value("--rules"),
        p.value("--source-version"),
        p.value("--target-version"),
    ) {
        (Some("official"), Some(s), Some(t)) => {
            official_rules(source_version(s)?, version_line(t)?)
        }
        (Some("official"), _, _) => {
            return Err(CliError::Usage(
                "--rules official needs --source-version and --target-version".into(),
            ));
        }
        (Some(r), _, _) => {
            return Err(CliError::Usage(format!("unknown rules {r} (official)")));
        }
        (None, None, None) => Vec::new(),
        (None, Some(s), Some(t)) => {
            let s = source_version(s)?;
            source_10_2 = s == SourceVersion::V10_2;
            rules(s, version_line(t)?)
        }
        _ => {
            return Err(CliError::Usage(
                "give --source-version and --target-version together".into(),
            ));
        }
    };
    let mut normalized = vec![(0usize, 0usize); pair.len()];
    let [source, target] = p.positional.as_slice() else {
        return Err(CliError::Usage(
            "set-comparison takes two unload prefixes".into(),
        ));
    };
    let source_set = unload_set(Path::new(source))?;
    let target_set = unload_set(Path::new(target))?;
    if source_set.is_empty() || target_set.is_empty() {
        return Err(CliError::Failed(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no unload files at {source} or {target}"),
        )));
    }
    // a 10.2 unload sets class owners in its schema file; the rules of the other files read them
    let owners = match source_set.get("schema") {
        Some(path) if source_10_2 => ClassOwners::from_schema(&entries("schema", path)?),
        _ => ClassOwners::default(),
    };
    let mut suffixes: Vec<&String> = source_set.keys().chain(target_set.keys()).collect();
    suffixes.sort();
    suffixes.dedup();

    let mut differs = false;
    let mut diff = String::new();
    let mut entries_changed = String::new();
    for suffix in suffixes {
        let mut s = match source_set.get(suffix) {
            Some(path) => entries(suffix, path)?,
            None => Vec::new(),
        };
        let mut t = match target_set.get(suffix) {
            Some(path) => entries(suffix, path)?,
            None => Vec::new(),
        };
        // the entries before the rules, to count those any rule changed (one entry may be changed by several rules)
        let before = (!pair.is_empty()).then(|| (s.clone(), t.clone()));
        let (ns, nt) = (
            normalize(&pair, suffix, true, &mut s, &owners),
            normalize(&pair, suffix, false, &mut t, &owners),
        );
        if let Some((s0, t0)) = before {
            let changed =
                |a: &[Vec<u8>], b: &[Vec<u8>]| a.iter().zip(b).filter(|(x, y)| x != y).count();
            let _ = writeln!(
                entries_changed,
                "normalized _{suffix}: source {} target {}",
                changed(&s0, &s),
                changed(&t0, &t)
            );
        }
        // an entry a rule emptied was folded into another (sp-owner-authid)
        s.retain(|e| !e.is_empty());
        t.retain(|e| !e.is_empty());
        for (n, (a, b)) in ns.into_iter().zip(nt).enumerate() {
            normalized[n].0 += a;
            normalized[n].1 += b;
        }
        let presence = match (
            source_set.contains_key(suffix),
            target_set.contains_key(suffix),
        ) {
            (true, false) => " (source only)",
            (false, true) => " (target only)",
            _ => "",
        };
        let (source_len, target_len) = (s.len(), t.len());
        let (only_source, only_target) = multiset_difference(s, t);
        println!(
            "_{suffix}{presence}: source {} target {} only-source {} only-target {}",
            source_len,
            target_len,
            only_source.len(),
            only_target.len()
        );
        if only_source.is_empty() && only_target.is_empty() && presence.is_empty() {
            continue;
        }
        differs = true;
        for (mark, side) in [("<", &only_source), (">", &only_target)] {
            for (i, e) in side.iter().enumerate() {
                let text = String::from_utf8_lossy(e);
                if i < SHOWN {
                    println!("  {mark} {}", text.replace('\n', "\\n"));
                }
                let _ = writeln!(diff, "_{suffix} {mark} {}", text.replace('\n', "\\n"));
            }
        }
    }
    if pair.is_empty() {
        println!("normalization: none");
    }
    for (r, (a, b)) in pair.iter().zip(&normalized) {
        println!("normalized {}: source {a} target {b}", r.name);
    }
    print!("{entries_changed}");
    if let Some(file) = p.value("--diff") {
        fs::write(file, diff)?;
    }
    println!(
        "set comparison: {}",
        if differs { "DIFFER" } else { "SAME" }
    );
    Ok(u8::from(differs))
}

fn version_line(v: &str) -> Result<VersionLine, CliError> {
    match v {
        "11.4" => Ok(VersionLine::V11_4),
        "develop" => Ok(VersionLine::Develop),
        _ => Err(CliError::Usage(format!(
            "unknown version {v} (11.4 or develop)"
        ))),
    }
}

/// The source version as `plan.json` names it (`SourceVersion::name`).
fn source_version(v: &str) -> Result<SourceVersion, CliError> {
    [
        SourceVersion::V10_2,
        SourceVersion::V11_2,
        SourceVersion::V11_3,
        SourceVersion::V11_4,
        SourceVersion::Develop,
    ]
    .into_iter()
    .find(|s| s.name() == v)
    .ok_or_else(|| {
        CliError::Usage(format!(
            "unknown source version {v} (10.2, 11.2, 11.3, 11.4 or develop)"
        ))
    })
}

/// Entries of `a` not matched in `b` and of `b` not matched in `a`, counting repeats.
fn multiset_difference(mut a: Vec<Vec<u8>>, mut b: Vec<Vec<u8>>) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    a.sort_unstable();
    b.sort_unstable();
    let (mut only_a, mut only_b) = (Vec::new(), Vec::new());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => {
                only_a.push(std::mem::take(&mut a[i]));
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                only_b.push(std::mem::take(&mut b[j]));
                j += 1;
            }
        }
    }
    only_a.extend(a.drain(i..));
    only_b.extend(b.drain(j..));
    (only_a, only_b)
}
