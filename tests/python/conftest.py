"""Shared fixtures for the PyO3 extension tests.

Builds the `index.fst` + `records.bin` artifacts once per session from the same
`tests/fixtures/cities.tsv` the Rust tests use, by invoking the `build-index` CLI.
Requires a Rust toolchain on PATH (present in the maturin build environment).
"""

import pathlib
import subprocess

import pytest

ROOT = pathlib.Path(__file__).resolve().parents[2]


@pytest.fixture(scope="session")
def geo_index(tmp_path_factory):
    """Build the geo artifacts and return (fst_path, records_path) as strings."""
    out = tmp_path_factory.mktemp("geo_index")
    tsv = ROOT / "tests" / "fixtures" / "cities.tsv"
    subprocess.run(
        ["cargo", "run", "--quiet", "--bin", "build-index", "--", str(tsv), str(out)],
        cwd=ROOT,
        check=True,
    )
    return str(out / "index.fst"), str(out / "records.bin")
