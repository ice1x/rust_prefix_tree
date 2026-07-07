//! End-to-end integration tests for the geo autocomplete backend.
//!
//! These exercise the realistic `/geo/suggest` workflow from the README: build the
//! artifacts from a TSV of places (via the `build-index` CLI and via the library
//! builder), then mmap them back and run prefix queries — the same path the PyO3
//! wrapper drives inside the reviews service.

use std::process::Command;

use geo_trie_rs::{Index, IndexBuilder, Record};
use memmap2::Mmap;
use tempfile::tempdir;

fn rec(gid: u64, name: &str, country: &str, pop: i64) -> Record {
    Record {
        gid,
        lat: 0.0,
        lon: 0.0,
        population: pop,
        country: country.into(),
        feature_code: "PPL".into(),
        name: name.into(),
    }
}

/// The library path: build in memory, query in memory.
#[test]
fn build_and_query_in_memory() {
    let mut b = IndexBuilder::new();
    let moscow = b.add_record(rec(1, "Moscow", "RU", 10_381_222));
    let solnech = b.add_record(rec(2, "Solnechnogorsk", "RU", 52_798));

    for key in ["moscow", "moskva", "москва", "mos"] {
        b.add_key(key, moscow);
    }
    for key in ["solnechnogorsk", "солнечногорск"] {
        b.add_key(key, solnech);
    }

    let (fst, records) = b.build().unwrap();
    let idx = Index::from_bytes(fst, records).unwrap();

    // Latin prefix.
    let hits = idx.suggest("mos", 8).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].name, "Moscow");
    assert_eq!(hits[0].country, "RU");

    // Cyrillic prefix folds through normalization too.
    let hits = idx.suggest("Солнеч", 8).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].gid, 2);
}

/// The deploy path: run the `build-index` binary on the fixture TSV, then mmap the
/// produced artifacts exactly as the service does.
#[test]
fn build_index_cli_then_mmap() {
    let out = tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_build-index");
    let tsv = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cities.tsv");

    let status = Command::new(bin)
        .arg(tsv)
        .arg(out.path())
        .status()
        .expect("run build-index");
    assert!(status.success(), "build-index exited non-zero");

    let fst_path = out.path().join("index.fst");
    let rec_path = out.path().join("records.bin");
    assert!(fst_path.exists() && rec_path.exists());

    let idx = Index::<Mmap>::open(&fst_path, &rec_path).unwrap();
    assert_eq!(idx.len(), 7);

    // "ber" prefix: Berlin (3.4M) ranks above Bergen (213k) above Bern (121k).
    let names: Vec<String> = idx
        .suggest("ber", 8)
        .unwrap()
        .into_iter()
        .map(|r| r.name)
        .collect();
    assert_eq!(names, vec!["Berlin", "Bergen", "Bern"]);

    // Alias key "nyc" resolves to New York City.
    let hits = idx.suggest("nyc", 8).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].gid, 5128581);

    // Accent- and case-folding parity: "Párî" -> Paris.
    let hits = idx.suggest("Párî", 8).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].name, "Paris");

    // A miss returns nothing.
    assert!(idx.suggest("zzzz", 8).unwrap().is_empty());
}
