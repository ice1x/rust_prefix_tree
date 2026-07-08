"""Container smoke test: open the mounted artifacts and run one query.

Verifies GEO_BACKEND=rust works end-to-end inside the runtime image against the
read-only mounted `index.fst` + `records.bin` (issue #7 acceptance). Paths come
from GEO_FST_PATH / GEO_RECORDS_PATH (set in Dockerfile.prod), overridable.

Usage:  python /app/smoke.py [query]
"""

import os
import sys

import geo_trie_rs

fst = os.environ.get("GEO_FST_PATH", "/data/index.fst")
rec = os.environ.get("GEO_RECORDS_PATH", "/data/records.bin")

idx = geo_trie_rs.Index.open(fst, rec)
query = sys.argv[1] if len(sys.argv) > 1 else "ber"

rows = idx.suggest(query, 8)
for rank, group, payload in rows:
    gid, name, lat, lon, country, population, feature_code = geo_trie_rs.geo_unpack(
        rank, group, payload
    )
    print(f"  {name} ({country}) gid={gid} pop={population}")

print(f"{len(rows)} result(s) for {query!r} over {len(idx)} records")
