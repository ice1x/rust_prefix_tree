#!/usr/bin/env bash
# Black-box end-to-end deployment check:
#   1. build the PyO3 wheel with maturin,
#   2. install it into the active Python,
#   3. build the index artifacts with the `build-index` CLI (offline step),
#   4. open them from a fresh Python process and assert query results.
#
# This exercises the exact path the reviews service uses in production. It is run
# by the CI `e2e` job and can be run locally.
#
# Env overrides (for local runs):
#   PYTHON         python interpreter to use (default: python3)
#   MATURIN_TARGET rust target triple to build for (e.g. x86_64-apple-darwin)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

PY="${PYTHON:-python3}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

target_args=()
if [[ -n "${MATURIN_TARGET:-}" ]]; then
  target_args=(--target "$MATURIN_TARGET")
fi

echo "==> [1/4] build wheel (maturin)"
"$PY" -m pip install --quiet --upgrade maturin
maturin build --quiet --release --features python "${target_args[@]}" --out "$WORK/dist"

echo "==> [2/4] install wheel"
"$PY" -m pip install --quiet --force-reinstall --no-deps "$WORK"/dist/*.whl

echo "==> [3/4] build index artifacts via build-index CLI"
cargo run --quiet --bin build-index -- tests/fixtures/cities.tsv "$WORK/idx"
test -f "$WORK/idx/index.fst"
test -f "$WORK/idx/records.bin"

echo "==> [4/4] query from a fresh Python process and assert"
"$PY" - "$WORK/idx" <<'PY'
import sys
import geo_trie_rs

idx_dir = sys.argv[1]
idx = geo_trie_rs.Index.open(f"{idx_dir}/index.fst", f"{idx_dir}/records.bin")

assert len(idx) == 7, f"unexpected len: {len(idx)}"

# Ranking: Berlin (3.4M) > Bergen (213k) > Bern (121k).
names = [geo_trie_rs.geo_unpack(*row)[1] for row in idx.suggest("ber", 8)]
assert names == ["Berlin", "Bergen", "Bern"], names

# Alias key resolves; accent + Cyrillic folding; misses.
assert geo_trie_rs.geo_unpack(*idx.suggest("nyc", 8)[0])[0] == 5128581
assert geo_trie_rs.geo_unpack(*idx.suggest("Párî", 8)[0])[1] == "Paris"
assert geo_trie_rs.geo_unpack(*idx.suggest("Солнеч", 8)[0])[1] == "Solnechnogorsk"
assert idx.suggest("zzzz", 8) == []

# normalize contract is exposed and idempotent.
assert geo_trie_rs.normalize("Zürich-HB") == "zurich hb"

print("e2e OK:", names)
PY

echo "==> e2e passed"
