"""End-to-end tests for the `geo_trie_rs` PyO3 extension module.

Run after building/installing the wheel (e.g. `maturin develop --features python`
or `pip install` the built wheel). Mirrors the Rust integration tests over the
Python surface: `Index.open`, `suggest`, `geo_unpack`, `normalize`.
"""

import geo_trie_rs
import pytest


def open_index(geo_index):
    fst_path, records_path = geo_index
    return geo_trie_rs.Index.open(fst_path, records_path)


def test_normalize_matches_rust_contract():
    assert geo_trie_rs.normalize("Zürich-HB") == "zurich hb"
    assert geo_trie_rs.normalize("  BER-- ") == "ber"
    assert geo_trie_rs.normalize("São Paulo") == "sao paulo"
    assert geo_trie_rs.normalize("") == ""
    # Idempotent.
    once = geo_trie_rs.normalize("Café  de   Flore")
    assert geo_trie_rs.normalize(once) == once


def test_open_and_len(geo_index):
    idx = open_index(geo_index)
    assert len(idx) == 7


def test_suggest_returns_rank_group_bytes(geo_index):
    idx = open_index(geo_index)
    rows = idx.suggest("ber", 8)
    assert rows, "expected matches for 'ber'"
    rank, group, payload = rows[0]
    assert isinstance(rank, int)
    assert isinstance(group, int)
    assert isinstance(payload, (bytes, bytearray))


def test_suggest_ranks_and_geo_unpack(geo_index):
    idx = open_index(geo_index)
    rows = idx.suggest("ber", 8)
    names = [geo_trie_rs.geo_unpack(rank, group, payload)[1] for rank, group, payload in rows]
    # Berlin (3.4M) > Bergen (213k) > Bern (121k).
    assert names == ["Berlin", "Bergen", "Bern"]


def test_geo_unpack_full_tuple(geo_index):
    idx = open_index(geo_index)
    (rank, group, payload) = idx.suggest("berl", 8)[0]
    gid, name, lat, lon, country, population, feature_code = geo_trie_rs.geo_unpack(
        rank, group, payload
    )
    assert gid == 2950159
    assert name == "Berlin"
    assert country == "DE"
    assert population == 3426354
    assert feature_code == "PPLC"
    assert lat == pytest.approx(52.52, abs=0.1)


def test_default_limit(geo_index):
    idx = open_index(geo_index)
    rows = idx.suggest("m")  # default limit=8
    names = [geo_trie_rs.geo_unpack(*r)[1] for r in rows]
    assert names == ["Moscow"]


def test_limit_is_respected(geo_index):
    idx = open_index(geo_index)
    assert len(idx.suggest("ber", 2)) == 2
    assert len(idx.suggest("ber", 0)) == 0


def test_alias_key_and_dedup(geo_index):
    idx = open_index(geo_index)
    # "nyc" is an alias of New York City; must resolve to exactly one row.
    rows = idx.suggest("nyc", 8)
    assert len(rows) == 1
    gid = geo_trie_rs.geo_unpack(*rows[0])[0]
    assert gid == 5128581


def test_accent_and_cyrillic_folding(geo_index):
    idx = open_index(geo_index)
    assert geo_trie_rs.geo_unpack(*idx.suggest("Párî", 8)[0])[1] == "Paris"
    assert geo_trie_rs.geo_unpack(*idx.suggest("Солнеч", 8)[0])[1] == "Solnechnogorsk"


def test_miss_returns_empty(geo_index):
    idx = open_index(geo_index)
    assert idx.suggest("zzzz", 8) == []
    assert idx.suggest("", 8) == []


def test_open_missing_file_raises():
    with pytest.raises(IOError):
        geo_trie_rs.Index.open("/nonexistent/index.fst", "/nonexistent/records.bin")
