//! End-to-end integration tests.
//!
//! Two things are exercised:
//! * the geo `/geo/suggest` workflow from the README (build the artifacts from a TSV
//!   via the `build-index` CLI, mmap them back, query) — the path the PyO3 wrapper
//!   drives inside the reviews service; and
//! * the same engine driving a **different domain** (people / ФИО) with no core
//!   changes — proving the index is domain-agnostic.

use std::process::Command;

use geo_trie_rs::{normalize, GeoRecord, Index, IndexBuilder, Record};
use memmap2::Mmap;
use tempfile::tempdir;

// ----------------------------------------------------------------------------
// Geo domain (the original use case), via the geo adapter.
// ----------------------------------------------------------------------------

fn geo(gid: u64, name: &str, country: &str, pop: i64) -> GeoRecord {
    GeoRecord {
        gid,
        lat: 0.0,
        lon: 0.0,
        population: pop,
        country: country.into(),
        feature_code: "PPL".into(),
        name: name.into(),
    }
}

#[test]
fn geo_build_and_query_in_memory() {
    let mut b = IndexBuilder::new();
    let moscow = b.add_record(geo(1, "Moscow", "RU", 10_381_222).to_record());
    let solnech = b.add_record(geo(2, "Solnechnogorsk", "RU", 52_798).to_record());

    for key in ["moscow", "moskva", "москва", "mos"] {
        b.add_key(key, moscow);
    }
    for key in ["solnechnogorsk", "солнечногорск"] {
        b.add_key(key, solnech);
    }

    let (fst, records) = b.build().unwrap();
    let idx = Index::from_bytes(fst, records).unwrap();

    let hits = idx.suggest_geo("mos", 8).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].name, "Moscow");
    assert_eq!(hits[0].country, "RU");

    // Cyrillic prefix folds through normalization too.
    let hits = idx.suggest_geo("Солнеч", 8).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].gid, 2);
}

#[test]
fn geo_build_index_cli_then_mmap() {
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
        .suggest_geo("ber", 8)
        .unwrap()
        .into_iter()
        .map(|r| r.name)
        .collect();
    assert_eq!(names, vec!["Berlin", "Bergen", "Bern"]);

    // Alias key "nyc" resolves to New York City.
    let hits = idx.suggest_geo("nyc", 8).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].gid, 5128581);

    // Accent- and case-folding parity: "Párî" -> Paris.
    let hits = idx.suggest_geo("Párî", 8).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].name, "Paris");

    // A miss returns nothing.
    assert!(idx.suggest_geo("zzzz", 8).unwrap().is_empty());
}

// ----------------------------------------------------------------------------
// A different domain: people / ФИО. Same engine, a domain-specific payload.
// group = person id, rank = a relevance score, payload = "Surname|Department".
// ----------------------------------------------------------------------------

struct Person {
    id: u64,
    score: i64,
    full_name: &'static str,
    department: &'static str,
    /// Alias keys to index under (e.g. "Surname Name", transliterations).
    keys: &'static [&'static str],
}

fn person_record(p: &Person) -> Record {
    Record {
        group: p.id,
        rank: p.score,
        payload: format!("{}|{}", p.full_name, p.department).into_bytes(),
    }
}

fn decode_person(r: &Record) -> (String, String) {
    let s = String::from_utf8(r.payload.clone()).unwrap();
    let (name, dept) = s.split_once('|').unwrap();
    (name.to_string(), dept.to_string())
}

#[test]
fn people_index_is_just_another_adapter() {
    let people = [
        Person {
            id: 1,
            score: 50,
            full_name: "Иванов Иван Иванович",
            department: "Sales",
            keys: &["Иванов Иван", "Ivanov Ivan"],
        },
        Person {
            id: 2,
            score: 90,
            full_name: "Иванова Мария Петровна",
            department: "Legal",
            keys: &["Иванова Мария", "Ivanova Maria"],
        },
        Person {
            id: 3,
            score: 10,
            full_name: "Петров Пётр Петрович",
            department: "IT",
            keys: &["Петров Пётр", "Petrov Petr"],
        },
    ];

    let mut b = IndexBuilder::new();
    for p in &people {
        let id = b.add_record(person_record(p));
        for k in p.keys {
            b.add_key(&normalize(k), id);
        }
    }
    let (fst, records) = b.build().unwrap();
    let idx = Index::from_bytes(fst, records).unwrap();

    // Prefix "иван" matches both Ivanovs; higher score (Мария, 90) ranks first.
    let hits = idx.suggest("иван", 8).unwrap();
    let decoded: Vec<(String, String)> = hits.iter().map(decode_person).collect();
    assert_eq!(
        decoded,
        vec![
            ("Иванова Мария Петровна".to_string(), "Legal".to_string()),
            ("Иванов Иван Иванович".to_string(), "Sales".to_string()),
        ]
    );

    // Latin transliteration alias resolves to the same person (dedup by group).
    let hits = idx.suggest("petrov", 8).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].group, 3);
    assert_eq!(decode_person(&hits[0]).1, "IT");
}
