//! Geo domain adapter over the agnostic core.
//!
//! This is the *only* geo-aware code in the crate: it maps a [`GeoRecord`] onto the
//! neutral [`Record`] the engine stores — `gid → group`, `population → rank`, and
//! the rest (`lat, lon, country, feature_code, name`) packed into `payload` — and
//! back again. Any other domain (people/ФИО, products, …) is just a different
//! adapter over the same [`Index`]/[`IndexBuilder`]; nothing in the core changes.

use std::io;

use crate::index::Index;
use crate::records::{push_str, read_str, read_u64, Record};

/// A place, as produced by the geo build pipeline and returned to `/geo/suggest`.
#[derive(Debug, Clone, PartialEq)]
pub struct GeoRecord {
    pub gid: u64,
    pub lat: f64,
    pub lon: f64,
    pub population: i64,
    pub country: String,
    pub feature_code: String,
    pub name: String,
}

impl GeoRecord {
    /// Encode into a neutral core [`Record`]: `gid → group`, `population → rank`,
    /// and `lat|lon|country|feature_code|name` into the opaque payload.
    pub fn to_record(&self) -> Record {
        let mut payload = Vec::new();
        payload.extend_from_slice(&self.lat.to_bits().to_le_bytes());
        payload.extend_from_slice(&self.lon.to_bits().to_le_bytes());
        push_str(&mut payload, &self.country);
        push_str(&mut payload, &self.feature_code);
        push_str(&mut payload, &self.name);
        Record {
            group: self.gid,
            rank: self.population,
            payload,
        }
    }

    /// Decode a core [`Record`] produced by [`GeoRecord::to_record`] back into a
    /// `GeoRecord`. Errors if the payload is malformed.
    pub fn from_record(r: &Record) -> io::Result<GeoRecord> {
        let p = &r.payload;
        let lat = f64::from_bits(read_u64(p, 0)?);
        let lon = f64::from_bits(read_u64(p, 8)?);
        let (country, at) = read_str(p, 16)?;
        let (feature_code, at) = read_str(p, at)?;
        let (name, _at) = read_str(p, at)?;
        Ok(GeoRecord {
            gid: r.group,
            lat,
            lon,
            population: r.rank,
            country,
            feature_code,
            name,
        })
    }
}

impl<B: AsRef<[u8]>> Index<B> {
    /// Geo-typed convenience over [`Index::suggest`]: returns decoded [`GeoRecord`]s
    /// instead of neutral core records. Same ranking/dedup semantics.
    pub fn suggest_geo(&self, prefix: &str, limit: usize) -> io::Result<Vec<GeoRecord>> {
        self.suggest(prefix, limit)?
            .iter()
            .map(GeoRecord::from_record)
            .collect()
    }

    /// Geo-typed convenience over [`Index::suggest_fuzzy`] (typo-tolerant). With
    /// `max_edits == 0` this is identical to [`Index::suggest_geo`].
    pub fn suggest_geo_fuzzy(
        &self,
        prefix: &str,
        limit: usize,
        max_edits: u32,
    ) -> io::Result<Vec<GeoRecord>> {
        self.suggest_fuzzy(prefix, limit, max_edits)?
            .iter()
            .map(GeoRecord::from_record)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::IndexBuilder;

    fn geo(gid: u64, name: &str, country: &str, pop: i64) -> GeoRecord {
        GeoRecord {
            gid,
            lat: 52.52,
            lon: 13.41,
            population: pop,
            country: country.into(),
            feature_code: "PPLC".into(),
            name: name.into(),
        }
    }

    #[test]
    fn record_roundtrips() {
        let g = geo(2950159, "Berlin", "DE", 3_426_354);
        let decoded = GeoRecord::from_record(&g.to_record()).unwrap();
        assert_eq!(decoded, g);
    }

    #[test]
    fn build_and_suggest_geo() {
        let mut b = IndexBuilder::new();
        let berlin = b.add_record(geo(1, "Berlin", "DE", 3_426_354).to_record());
        let bern = b.add_record(geo(2, "Bern", "CH", 121_631).to_record());
        for k in ["berlin", "ber"] {
            b.add_key(k, berlin);
        }
        for k in ["bern", "ber"] {
            b.add_key(k, bern);
        }
        let (fst, records) = b.build().unwrap();
        let idx = Index::from_bytes(fst, records).unwrap();

        let hits = idx.suggest_geo("ber", 8).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].name, "Berlin"); // higher population first
        assert_eq!(hits[0].country, "DE");
        assert_eq!(hits[1].name, "Bern");
    }

    #[test]
    fn suggest_geo_fuzzy_recovers_typo_and_off_matches_exact() {
        let mut b = IndexBuilder::new();
        let sol = b.add_record(geo(3143244, "Solnechnogorsk", "RU", 52_798).to_record());
        b.add_key("solnechnogorsk", sol);
        b.add_key("солнечногорск", sol);
        let (fst, records) = b.build().unwrap();
        let idx = Index::from_bytes(fst, records).unwrap();

        // Typo tolerated at edit distance 1 (Cyrillic too).
        let hits = idx.suggest_geo_fuzzy("солнечо", 8, 1).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "Solnechnogorsk");

        // max_edits == 0 is identical to the exact geo suggest.
        assert_eq!(
            idx.suggest_geo("solnech", 8).unwrap(),
            idx.suggest_geo_fuzzy("solnech", 8, 0).unwrap(),
        );
    }

    #[test]
    fn edge_values_and_unicode_roundtrip() {
        let g = GeoRecord {
            gid: u64::MAX,
            lat: -89.999,
            lon: 179.999,
            population: 0,
            country: String::new(), // empty string field
            feature_code: "PPL".into(),
            name: "Улан-Удэ".into(), // unicode + would-be-separator inside name
        };
        assert_eq!(GeoRecord::from_record(&g.to_record()).unwrap(), g);
    }

    #[test]
    fn malformed_payload_errors() {
        // A record whose payload is too short to hold lat/lon must error, not panic.
        let bad = Record {
            group: 1,
            rank: 1,
            payload: vec![0, 1, 2],
        };
        assert!(GeoRecord::from_record(&bad).is_err());
    }

    #[test]
    fn suggest_geo_dedups_by_gid_over_aliases() {
        let mut b = IndexBuilder::new();
        let berlin = b.add_record(geo(2950159, "Berlin", "DE", 3_426_354).to_record());
        // Same place reachable via several aliases -> must appear once.
        for k in ["berlin", "berlín", "берлин", "ber"] {
            b.add_key(&crate::normalize::normalize(k), berlin);
        }
        let (fst, records) = b.build().unwrap();
        let idx = Index::from_bytes(fst, records).unwrap();

        let hits = idx.suggest_geo("ber", 8).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].gid, 2950159);
    }
}
