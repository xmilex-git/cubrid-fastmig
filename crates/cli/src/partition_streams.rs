//! `cubrid-fastmig partition-streams`: a check, not a migration command. Transcodes every partition expression
//! stream of an 11.4, 11.3, 11.2 or 10.2 database to develop and compares each with the stream the develop engine
//! wrote for the same class in a develop database built from the same script.
//!
//! Input: `<source first volume> <target first volume> --out <dir> [--from 11.4|11.3|11.2|10.2] [--to develop|11.4]`
//! (defaults 11.4 and develop; `--to 11.4` compares with the streams 11.4.6 wrote, workspace#552).
//! A 10.2 class name has no owner, so a 10.2 run matches develop classes by the name after the owner. Output: one line per
//! partitioned class (stream sizes, whether the trees and the bytes match) and, in `<dir>`, each stream's bytes and
//! canonical tree text.
//! Exit status 1 when a tree differs or a stream cannot be read. Progress goes to stderr; the table is printed once
//! the run ends, so it never tears the terminal display.
//! Source: tickets P1g workspace#497, T17 workspace#540 (acceptance: byte-equal, or the differences explained with
//! equal trees).

use std::collections::BTreeMap;
use std::fmt::Write;
use std::io;
use std::path::Path;

use fastmig_catalog::source_classes::read_classes;
use fastmig_ondisk::VersionLine;
use fastmig_ondisk::partition_stream::container::stream_of_pvalues;
use fastmig_ondisk::partition_stream::{parse, serialize_with_mask, transcode_partition_stream};
use fastmig_ondisk::source_version::HeaderLayout;
use fastmig_ondisk::volume_set::{Access, Locate, VolumeSet};
use fastmig_progress::{PhaseSpec, Progress, Unit, default_rates};

use crate::args::{self, CliError};
use crate::progress_args;

const PHASES: &[PhaseSpec] = &[
    PhaseSpec::step("read-source-classes"),
    PhaseSpec::step("read-develop-classes"),
    // a class stream is a few hundred bytes: the step default, per class
    PhaseSpec::work(
        "transcode",
        Unit::Count("classes"),
        default_rates::DefaultRate {
            points: &[(1, 1.0 / default_rates::STEP.as_secs_f64())],
        },
    ),
];
const TRANSCODE: usize = 2;

/// Class name (without the owner when `unowned`) → the master partition's stream.
fn streams(
    first: &str,
    line: VersionLine,
    layout: HeaderLayout,
    unowned: bool,
) -> Result<BTreeMap<String, Vec<u8>>, CliError> {
    let vs = VolumeSet::open(
        Path::new(first),
        Locate::FirstVolumeDir,
        Access::ReadOnly,
        layout,
    )?;
    let mut out = BTreeMap::new();
    for c in read_classes(&vs, line)? {
        for p in c.class.partitions.iter().filter(|p| p.pname.is_none()) {
            if let Some(s) = stream_of_pvalues(line, &p.pvalues).map_err(failed)? {
                let name = String::from_utf8_lossy(&c.class.name).into_owned();
                let name = match name.split_once('.') {
                    Some((_, n)) if unowned => n.to_owned(),
                    _ => name,
                };
                out.insert(name, s);
            }
        }
    }
    Ok(out)
}

fn failed(e: impl std::fmt::Display) -> CliError {
    CliError::Failed(io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

pub fn run(argv: &[String]) -> Result<(), CliError> {
    let flags = [&["--out", "--from", "--to"], progress_args::FLAGS].concat();
    let p = args::parse(argv, &flags, &[])?;
    let [src, dev] = p.positional.as_slice() else {
        return Err(CliError::Usage(
            "partition-streams needs <source first volume> <develop first volume>".into(),
        ));
    };
    let out = Path::new(
        p.value("--out")
            .ok_or_else(|| CliError::Usage("partition-streams needs --out".into()))?,
    );
    std::fs::create_dir_all(out)?;
    let progress = Progress::new("partition-streams", PHASES, 1, std::time::Instant::now());
    let monitor = progress_args::start(&p, progress.clone(), None, None)?;
    // 11.2 and 11.3 use the 11.4 codecs in the older header layout (R8)
    let (from, layout) = match p.value("--from").unwrap_or("11.4") {
        "11.4" => (VersionLine::V11_4, HeaderLayout::Since11_4),
        "11.2" | "11.3" => (VersionLine::V11_4, HeaderLayout::Before11_4),
        "10.2" => (VersionLine::V10_2, HeaderLayout::Before11_4),
        x => {
            return Err(CliError::Usage(format!(
                "--from {x}: 11.4, 11.3, 11.2 or 10.2"
            )));
        }
    };
    // the target line: develop, or 11.4 for a 10.2..11.3 source (workspace#552)
    let to = match p.value("--to").unwrap_or("develop") {
        "develop" => VersionLine::Develop,
        "11.4" => VersionLine::V11_4,
        x => return Err(CliError::Usage(format!("--to {x}: develop or 11.4"))),
    };
    let unowned = from == VersionLine::V10_2;
    let src = streams(src, from, layout, false)?;
    progress.advance();
    let dev = streams(dev, to, HeaderLayout::Since11_4, unowned)?;
    progress.set_total(TRANSCODE, src.len() as u64);
    progress.advance();
    let meter = progress.meter(TRANSCODE);
    let mut table = String::new();
    let mut bad = 0;
    let _ = writeln!(
        table,
        "class\tsource_B\ttranscoded_B\tdevelop_B\ttree\tbytes"
    );
    for (name, s) in &src {
        // counted as it starts: the arms below leave the iteration early
        meter.add(1);
        let file = |ext: &str| out.join(format!("{name}.{ext}"));
        std::fs::write(file("src.bin"), s)?;
        std::fs::write(file("src.txt"), parse(from, s).map_err(failed)?.canonical())?;
        let t = match transcode_partition_stream(from, to, s) {
            Ok(t) => t,
            Err(e) => {
                let _ = writeln!(table, "{name}\t{}\t-\t-\ttranscode failed: {e}\t-", s.len());
                bad += 1;
                continue;
            }
        };
        let t_parsed = parse(to, &t).map_err(failed)?;
        let t_tree = t_parsed.canonical();
        std::fs::write(file("trans.bin"), &t)?;
        std::fs::write(file("trans.txt"), &t_tree)?;
        let Some(d) = dev.get(name) else {
            let _ = writeln!(
                table,
                "{name}\t{}\t{}\t-\tno develop stream\t-",
                s.len(),
                t.len()
            );
            bad += 1;
            continue;
        };
        let d_parsed = parse(to, d).map_err(failed)?;
        std::fs::write(file("dev.bin"), d)?;
        std::fs::write(file("dev.txt"), d_parsed.canonical())?;
        let tree_eq = d_parsed.canonical() == t_tree;
        // a byte the writer leaves unwritten holds the engine's buffer garbage, so only written bytes must match
        let (_, written) = serialize_with_mask(to, &t_parsed);
        let bytes = if t.len() != d.len() {
            format!("len {} vs {}", t.len(), d.len())
        } else {
            let differ = |w: bool| {
                (0..t.len())
                    .filter(|&i| t[i] != d[i] && written[i] == w)
                    .count()
            };
            match (differ(true), differ(false)) {
                (0, 0) => "equal".into(),
                (0, n) => format!("written bytes equal; {n} unwritten B differ"),
                (n, _) => format!("{n} written B differ"),
            }
        };
        let _ = writeln!(
            table,
            "{name}\t{}\t{}\t{}\t{}\t{bytes}",
            s.len(),
            t.len(),
            d.len(),
            if tree_eq { "equal" } else { "DIFFERENT" },
        );
        if !tree_eq {
            bad += 1;
        }
    }
    for name in dev.keys().filter(|n| !src.contains_key(*n)) {
        let _ = writeln!(
            table,
            "{name}\t-\t-\t{}\tno source stream\t-",
            dev[name].len()
        );
    }
    progress.advance();
    monitor.finish();
    print!("{table}");
    if bad > 0 {
        return Err(failed(format!("{bad} partition stream(s) differ")));
    }
    Ok(())
}
