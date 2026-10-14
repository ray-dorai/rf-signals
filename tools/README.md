# Tools

Python helpers for building the data bracket-heat reads. None of them change a store or config
in place: each writes a new output you point the server at when you are satisfied.

| Script | What it does | Needs |
|--------|--------------|-------|
| `lidar_laz.py` | Lists a USGS 3DEP project's LAZ tiles and cooks them into a new `.bheat` store with `terrain_cooker` | pdal, curl, shapely |
| `bheat_overlay.py` | Merges a newly cooked flight into an existing store: new cells inside the flight's footprint, old cells elsewhere | numpy, shapely 2 |
| `address_survey.py` | Surveys every parcel in range of your towers and writes `address-layer-v2.json` | GDAL (osgeo), shapely 2, numpy, requests |

Run any of them with `--help` for the full usage.

## Adding a newer LiDAR flight

```sh
# 1. Find the project name: without --project, list prints the projects in the box.
python3 tools/lidar_laz.py list --bbox -95.10,38.95,-94.90,39.10
python3 tools/lidar_laz.py list --bbox -95.10,38.95,-94.90,39.10 --project XX_Example_2024 --out tiles.json

# 2. Cook into a NEW store. Never cook a newer flight onto old tiles: the cooker keeps the
#    per-cell maximum, so trees and buildings removed since the old flight would stay.
cargo build --release -p terrain_cooker
python3 tools/lidar_laz.py cook --tiles tiles.json --store ./bheat-2024 \
    --cooker target/release/terrain_cooker --terrain ./terrain --protect ./bheat

# 3. Merge into a third directory; both inputs are left untouched.
python3 tools/bheat_overlay.py --new ./bheat-2024 --old ./bheat --out ./bheat-merged --tiles tiles.json

# 4. Point heat_path in isp.ron at ./bheat-merged, restart, compare, then swap it in.
```

3DEP flights are not always classified into ground, vegetation and buildings. The cooker uses
the highest non-ground return per cell either way, so unclassified flights still produce a
usable clutter surface.

## Building the address layer

```sh
python3 tools/address_survey.py points  --config sources.json --isp bracket-heat/resources/isp.ron --out points.csv
python3 tools/address_survey.py survey  --points points.csv --resources bracket-heat/resources \
    --binary target/release/bracket-heat --terrain ./terrain --out results.jsonl
python3 tools/address_survey.py publish --points points.csv --results results.jsonl --config sources.json \
    --out bracket-heat/resources/address-layer-v2.json
```

The survey runs its own server instance on port 18001 (`--port`), so a live planner is not
loaded. It takes roughly 3 addresses per second with 6 workers, depending on how many APs are in
range. The address layer's format is described in [FORK-CHANGES.md](../FORK-CHANGES.md#address-layer).

See [docs/snmp-ap-transmit-oids.md](../docs/snmp-ap-transmit-oids.md) for reading real AP
transmit power to put in the link budgets.
