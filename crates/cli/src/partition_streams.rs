//! `cubrid-fastmig partition-streams`: a check, not a migration command. Transcodes every partition expression
//! stream of an 11.4 database to develop and compares each with the stream the develop engine wrote for the same
//! class in a develop database built from the same script.
//!
//! Input: `<11.4 first volume> <develop first volume> --out <dir>`. Output: one line per partitioned class (stream
//! sizes, whether the trees and the bytes match) and, in `<dir>`, each stream's bytes and canonical tree text.
//! Exit status 1 when a tree differs or a stream cannot be read. Progress goes to stderr; the table is printed once
//! the run ends, so it never tears the terminal display.
//! Source: ticket P1g workspace#497 (acceptance: byte-equal, or the differences explained with equal trees).

use std::collections::BTreeMap;
use std::fmt::Write;
use std::io;
use std::path::Path;

use fastmig_catalog::source_classes::read_classes;
use fastmig_ondisk::VersionLine;
use fastmig_ondisk::partition_stream::container::stream_of_pvalues;
use fastmig_ondisk::partition_stream::{parse, serialize_with_mask, transcode_partition_stream};
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
            per_worker: 1.0 / default_rates::STEP.as_secs_f64(),
            max_workers: 1,
        },
    ),
];
const TRANSCODE: usize = 2;

/// Class name → the master partition's stream.
fn streams(first: &str, line: VersionLine) -> Result<BTreeMap<String, Vec<u8>>, CliError> {
    let vs = VolumeSet::open(Path::new(first), Locate::FirstVolumeDir, Access::ReadOnly)?;
    let mut out = BTreeMap::new();
    for c in read_classes(&vs, line)? {
        for p in c.class.partitions.iter().filter(|p| p.pname.is_none()) {
            if let Some(s) = stream_of_pvalues(line, &p.pvalues).map_err(failed)? {
                out.insert(String::from_utf8_lossy(&c.class.name).into_owned(), s);
            }
        }
    }
    Ok(out)
}

fn failed(e: impl std::fmt::Display) -> CliError {
    CliError::Failed(io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

pub fn run(argv: &[String]) -> Result<(), CliError> {
    let flags = [&["--out"], progress_args::FLAGS].concat();
    let p = args::parse(argv, &flags, &[])?;
    let [src, dev] = p.positional.as_slice() else {
        return Err(CliError::Usage(
            "partition-streams needs <11.4 first volume> <develop first volume>".into(),
        ));
    };
    let out = Path::new(
        p.value("--out")
            .ok_or_else(|| CliError::Usage("partition-streams needs --out".into()))?,
    );
    std::fs::create_dir_all(out)?;
    let progress = Progress::new("partition-streams", PHASES, 1);
    let monitor = progress_args::start(&p, progress.clone(), None)?;
    let src = streams(src, VersionLine::V11_4)?;
    progress.advance();
    let dev = streams(dev, VersionLine::Develop)?;
    progress.set_total(TRANSCODE, src.len() as u64);
    progress.advance();
    let meter = progress.meter(TRANSCODE);
    let mut table = String::new();
    let mut bad = 0;
    let _ = writeln!(table, "class\t11.4_B\ttranscoded_B\tdevelop_B\ttree\tbytes");
    for (name, s) in &src {
        // counted as it starts: the arms below leave the iteration early
        meter.add(1);
        let file = |ext: &str| out.join(format!("{name}.{ext}"));
        std::fs::write(file("114.bin"), s)?;
        std::fs::write(
            file("114.txt"),
            parse(VersionLine::V11_4, s).map_err(failed)?.canonical(),
        )?;
        let t = match transcode_partition_stream(VersionLine::V11_4, VersionLine::Develop, s) {
            Ok(t) => t,
            Err(e) => {
                let _ = writeln!(table, "{name}\t{}\t-\t-\ttranscode failed: {e}\t-", s.len());
                bad += 1;
                continue;
            }
        };
        let t_parsed = parse(VersionLine::Develop, &t).map_err(failed)?;
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
        let d_parsed = parse(VersionLine::Develop, d).map_err(failed)?;
        std::fs::write(file("dev.bin"), d)?;
        std::fs::write(file("dev.txt"), d_parsed.canonical())?;
        let tree_eq = d_parsed.canonical() == t_tree;
        // a byte the writer leaves unwritten holds the engine's buffer garbage, so only written bytes must match
        let (_, written) = serialize_with_mask(VersionLine::Develop, &t_parsed);
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
            "{name}\t-\t-\t{}\tno 11.4 stream\t-",
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
