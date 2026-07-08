//! End-to-end tests over the **real deployment pipeline** at scale.
//!
//! Unlike `integration.rs` (small, hand-picked inputs), this generates a ~1000-row
//! dataset, runs the actual `build-index` binary to produce `index.fst` +
//! `records.bin`, memory-maps them back, and asserts the properties a deployment
//! relies on:
//!
//! * **correctness at scale** — ranking, dedup and limits over a large index;
//! * **determinism** — the same input builds byte-identical artifacts (safe to
//!   cache / diff / reproduce across build hosts);
//! * **shared-open** — two independent `Index::open` calls over the same files
//!   return identical results (the OS-page-cache sharing model, README §4);
//! * **core/adapter parity** — the agnostic `suggest` decoded through the geo
//!   adapter matches the typed `suggest_geo`.

use std::io::Write;
use std::process::Command;

use geo_trie_rs::{GeoRecord, Index};
use memmap2::Mmap;
use tempfile::{tempdir, TempDir};

const FILLER: u64 = 1000;

/// Deterministically generate a dataset TSV: `FILLER` filler towns plus a known
/// "zenith" cluster (shared alias) and a "Metropolis" alias-dedup case.
fn dataset() -> String {
    let mut s = String::from("# e2e generated dataset\n");
    for i in 0..FILLER {
        let gid = 1_000_000 + i;
        let name = format!("Town{i:05}");
        // Deterministic pseudo-spread of populations (no RNG — keeps builds stable).
        let pop = (i.wrapping_mul(2_654_435_761) % 1_000_000) as i64;
        let country = ["US", "DE", "FR", "RU"][(i % 4) as usize];
        let lat = (i % 180) as f64 - 90.0;
        let lon = (i % 360) as f64 - 180.0;
        s.push_str(&format!(
            "{gid}\t{name}\t{lat}\t{lon}\t{country}\t{pop}\tPPL\t{name}\n"
        ));
    }
    // Known cluster: three places share the "zenith" alias, distinct populations.
    for (gid, name, pop) in [
        (9_000_001u64, "Zenith Alpha", 900_000i64),
        (9_000_002, "Zenith Beta", 500_000),
        (9_000_003, "Zenith Gamma", 100_000),
    ] {
        s.push_str(&format!(
            "{gid}\t{name}\t0\t0\tXX\t{pop}\tPPLC\t{name}|zenith\n"
        ));
    }
    // One place reachable via several aliases -> must dedup to a single hit.
    s.push_str("9100000\tMetropolis\t0\t0\tXX\t250000\tPPLC\tMetropolis|metro|метрополис\n");
    s
}

/// Run the real `build-index` binary on `tsv` into a fresh dir; return that dir.
fn build(tsv_path: &std::path::Path) -> TempDir {
    let out = tempdir().unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_build-index"))
        .arg(tsv_path)
        .arg(out.path())
        .status()
        .expect("run build-index");
    assert!(status.success(), "build-index failed");
    out
}

fn write_tsv(dir: &std::path::Path, contents: &str) -> std::path::PathBuf {
    let p = dir.join("data.tsv");
    std::fs::File::create(&p)
        .unwrap()
        .write_all(contents.as_bytes())
        .unwrap();
    p
}

#[test]
fn full_pipeline_scale_ranking_and_dedup() {
    let src = tempdir().unwrap();
    let tsv = write_tsv(src.path(), &dataset());
    let out = build(&tsv);
    let idx =
        Index::<Mmap>::open(out.path().join("index.fst"), out.path().join("records.bin")).unwrap();

    // FILLER towns + 3 zenith + 1 metropolis.
    assert_eq!(idx.len() as u64, FILLER + 4);

    // Shared alias "zenith" -> three places, deduped, ranked by population desc.
    let names: Vec<String> = idx
        .suggest_geo("zenith", 10)
        .unwrap()
        .into_iter()
        .map(|r| r.name)
        .collect();
    assert_eq!(names, vec!["Zenith Alpha", "Zenith Beta", "Zenith Gamma"]);

    // Alias dedup: "metro" reaches Metropolis via two keys but yields one hit.
    let metro = idx.suggest_geo("metro", 10).unwrap();
    assert_eq!(metro.len(), 1);
    assert_eq!(metro[0].gid, 9_100_000);

    // Large fan-out with a limit: results are capped and sorted by population desc.
    let top = idx.suggest_geo("town", 5).unwrap();
    assert_eq!(top.len(), 5);
    for w in top.windows(2) {
        assert!(
            w[0].population >= w[1].population,
            "not sorted by population"
        );
    }

    // A guaranteed miss.
    assert!(idx.suggest_geo("nonexistentplace", 10).unwrap().is_empty());
}

#[test]
fn build_is_deterministic() {
    let src = tempdir().unwrap();
    let tsv = write_tsv(src.path(), &dataset());

    let a = build(&tsv);
    let b = build(&tsv);

    let fst_a = std::fs::read(a.path().join("index.fst")).unwrap();
    let fst_b = std::fs::read(b.path().join("index.fst")).unwrap();
    let rec_a = std::fs::read(a.path().join("records.bin")).unwrap();
    let rec_b = std::fs::read(b.path().join("records.bin")).unwrap();

    assert_eq!(fst_a, fst_b, "index.fst is not reproducible");
    assert_eq!(rec_a, rec_b, "records.bin is not reproducible");
}

#[test]
fn independent_opens_agree_and_core_matches_adapter() {
    let src = tempdir().unwrap();
    let tsv = write_tsv(src.path(), &dataset());
    let out = build(&tsv);
    let fst = out.path().join("index.fst");
    let rec = out.path().join("records.bin");

    // Two independent mmap opens (the multi-worker sharing model) must agree.
    let idx1 = Index::<Mmap>::open(&fst, &rec).unwrap();
    let idx2 = Index::<Mmap>::open(&fst, &rec).unwrap();

    for q in ["zenith", "town000", "metro", "z"] {
        let a: Vec<_> = idx1.suggest_geo(q, 8).unwrap();
        let b: Vec<_> = idx2.suggest_geo(q, 8).unwrap();
        assert_eq!(a, b, "independent opens disagree for {q:?}");

        // Core suggest() decoded via the geo adapter must equal typed suggest_geo().
        let via_core: Vec<GeoRecord> = idx1
            .suggest(q, 8)
            .unwrap()
            .iter()
            .map(|r| GeoRecord::from_record(r).unwrap())
            .collect();
        assert_eq!(via_core, a, "core/adapter parity broken for {q:?}");
    }
}
