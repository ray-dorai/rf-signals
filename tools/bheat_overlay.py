#!/usr/bin/env python3
"""Merge a newly cooked LiDAR flight into an existing .bheat store, without touching either.

  bheat_overlay.py --new ./bheat-new --old ./bheat --out ./bheat-merged \
                   --tiles tiles.json [--footprint flight.geojson]

The output store holds, for every .bheat tile:
  * inside the new flight's footprint: the new flight's cells (old cells are discarded, so
    trees and buildings removed since the old flight disappear);
  * outside it: the old store's cells (a tile the flight only partly covers is merged per cell);
  * tiles the new flight does not touch: symlinks to the old store.

Footprint = union of the LAZ tiles listed in <new>/cooked.done (from the lidar_laz.py tile list),
optionally intersected with --footprint, a GeoJSON of the flight's boundary (USGS publishes one
per work unit). Use it when the LAZ tiles are bigger than the area actually flown, or the edge
tiles are partial.

Point the server's heat_path at --out to try it; swap it in for the old store when satisfied.
Needs numpy and shapely 2.
"""
import argparse, glob, json, os, re
import numpy as np, shapely
from shapely.geometry import box, shape
from shapely.ops import unary_union

N = 768                    # cells per tile side
STEP = 0.01                # tile size, degrees
NAME = re.compile(r'([NS])(\d{3})t(\d{2})_([EW])(\d{3})t(\d{2})\.bheat$')

def tile_geometry(name):
    """(box, cell-centre lat grid, cell-centre lon grid) for a tile name like N040t99_W105t35.
    Rows run away from the equator and columns away from the prime meridian (abs lat/lon)."""
    m = NAME.match(name)
    if not m: return None
    ns, la, lah, ew, lo, loh = m.groups()
    blat, blon = int(la) + int(lah) / 100, int(lo) + int(loh) / 100
    sl, so = (1 if ns == 'N' else -1), (1 if ew == 'E' else -1)
    ii = (np.arange(N) + 0.5) / N * STEP
    lat, lon = sl * (blat + ii), so * (blon + ii)
    lats, lons = sorted((sl * blat, sl * (blat + STEP))), sorted((so * blon, so * (blon + STEP)))
    LON, LAT = np.meshgrid(lon, lat)
    return box(lons[0], lats[0], lons[1], lats[1]), LAT, LON

def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--new', required=True, help='store cooked from the new flight only')
    ap.add_argument('--old', required=True, help='existing store')
    ap.add_argument('--out', required=True, help='output store (created; must differ from both inputs)')
    ap.add_argument('--tiles', required=True, help='tile list used to cook --new (lidar_laz.py list)')
    ap.add_argument('--footprint', help='GeoJSON boundary of the flight (optional)')
    a = ap.parse_args()
    real = {os.path.realpath(a.old), os.path.realpath(a.new)}
    if os.path.realpath(a.out) in real: raise SystemExit('--out must be a new directory')
    os.makedirs(a.out, exist_ok=True)

    done = {l.strip() for l in open(os.path.join(a.new, 'cooked.done'))}
    tiles = [t for t in json.load(open(a.tiles)) if os.path.basename(t['url']) in done]
    if not tiles: raise SystemExit('no cooked tiles from --tiles found in --new/cooked.done')
    foot = unary_union([box(t['bbox']['minX'], t['bbox']['minY'], t['bbox']['maxX'], t['bbox']['maxY']) for t in tiles])
    if a.footprint:
        gj = json.load(open(a.footprint))
        feats = gj['features'] if 'features' in gj else [gj]
        foot = foot.intersection(unary_union([shape(f.get('geometry', f)) for f in feats]))
    foot = foot.buffer(0); shapely.prepare(foot)

    n = dict(full=0, merged=0, new_only=0, outside=0, linked=0)
    newnames = set()
    for f in sorted(glob.glob(os.path.join(a.new, '*.bheat'))):
        name = os.path.basename(f); g = tile_geometry(name)
        if g is None: continue
        newnames.add(name); tb, LAT, LON = g
        out = os.path.join(a.out, name)
        if os.path.lexists(out): os.unlink(out)
        old = os.path.join(a.old, name)
        has_old = os.path.exists(old) and os.path.getsize(old) >= N * N * 4
        if not foot.intersects(tb):                 # cooked spill-over outside the flight: keep the old tile
            n['outside'] += 1
            if has_old: os.symlink(os.path.realpath(old), out)
            continue
        nw = np.fromfile(f, dtype='<u2', count=N * N * 2).reshape(N, N, 2)
        if foot.contains(tb):
            nw.tofile(out); n['full'] += 1; continue
        inside = shapely.contains_xy(foot, LON.ravel(), LAT.ravel()).reshape(N, N)
        if has_old:
            od = np.fromfile(old, dtype='<u2', count=N * N * 2).reshape(N, N, 2)
            np.where(inside[..., None], nw, od).astype('<u2').tofile(out); n['merged'] += 1
        else:
            nw.tofile(out); n['new_only'] += 1
    for f in glob.glob(os.path.join(a.old, '*.bheat')):
        name = os.path.basename(f)
        if name in newnames: continue
        out = os.path.join(a.out, name)
        if not os.path.lexists(out): os.symlink(os.path.realpath(f), out); n['linked'] += 1
    print(n)

if __name__ == '__main__': main()
