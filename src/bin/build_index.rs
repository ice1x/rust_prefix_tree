//! `build-index` — offline builder that turns a TSV of places into the two
//! memory-mappable artifacts `index.fst` + `records.bin` (README §6).
//!
//! Usage:
//!   build-index <input.tsv> <out_dir>
//!
//! Input TSV columns (tab-separated, optional `#`-prefixed header line):
//!   gid  name  lat  lon  country  population  feature_code  keys
//!
//! `keys` is a `|`-separated list of raw alias strings to index the record under
//! (e.g. multilingual alternate names). Each is normalized with the same
//! [`geo_trie_rs::normalize`] used at query time. If `keys` is empty the record is
//! indexed under `name` only.

use std::fs;
use std::io::{self, BufRead};
use std::path::Path;
use std::process::ExitCode;

use geo_trie_rs::{normalize, IndexBuilder, Record};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: {} <input.tsv> <out_dir>", args[0]);
        return ExitCode::from(2);
    }
    match run(&args[1], &args[2]) {
        Ok((keys, records, fst_len, rec_len)) => {
            println!(
                "built index: {records} records, {keys} keys -> index.fst ({fst_len} B), records.bin ({rec_len} B)"
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("build-index: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(input: &str, out_dir: &str) -> io::Result<(usize, usize, usize, usize)> {
    let file = fs::File::open(input)?;
    let reader = io::BufReader::new(file);

    let mut builder = IndexBuilder::new();
    let mut key_count = 0usize;
    let mut record_count = 0usize;

    for (lineno, line) in reader.lines().enumerate() {
        let line = line?;
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (record, raw_keys) = parse_line(trimmed).map_err(|msg| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("line {}: {msg}", lineno + 1),
            )
        })?;
        let name = record.name.clone();
        let id = builder.add_record(record);
        record_count += 1;

        let mut linked = 0usize;
        for raw in raw_keys {
            let key = normalize(raw);
            if !key.is_empty() {
                builder.add_key(&key, id);
                key_count += 1;
                linked += 1;
            }
        }
        // Always ensure the record is reachable by its own name.
        if linked == 0 {
            let key = normalize(&name);
            if !key.is_empty() {
                builder.add_key(&key, id);
                key_count += 1;
            }
        }
    }

    let (fst_bytes, rec_bytes) = builder.build()?;
    let out = Path::new(out_dir);
    fs::create_dir_all(out)?;
    fs::write(out.join("index.fst"), &fst_bytes)?;
    fs::write(out.join("records.bin"), &rec_bytes)?;
    Ok((key_count, record_count, fst_bytes.len(), rec_bytes.len()))
}

/// Parse one TSV line into a record plus its raw (un-normalized) alias keys.
fn parse_line(line: &str) -> Result<(Record, Vec<&str>), String> {
    let cols: Vec<&str> = line.split('\t').collect();
    if cols.len() < 7 {
        return Err(format!(
            "expected >=7 tab-separated columns, got {}",
            cols.len()
        ));
    }
    let gid = cols[0].parse::<u64>().map_err(|_| "bad gid".to_string())?;
    let name = cols[1].to_string();
    let lat = cols[2].parse::<f64>().map_err(|_| "bad lat".to_string())?;
    let lon = cols[3].parse::<f64>().map_err(|_| "bad lon".to_string())?;
    let country = cols[4].to_string();
    let population = cols[5]
        .parse::<i64>()
        .map_err(|_| "bad population".to_string())?;
    let feature_code = cols[6].to_string();

    let keys: Vec<&str> = match cols.get(7) {
        Some(&"") | None => vec![cols[1]],
        Some(k) => k.split('|').filter(|s| !s.is_empty()).collect(),
    };

    Ok((
        Record {
            gid,
            lat,
            lon,
            population,
            country,
            feature_code,
            name,
        },
        keys,
    ))
}
