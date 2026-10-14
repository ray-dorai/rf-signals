#!/usr/bin/env python3
"""Build the address layer (<resources>/address-layer-v2.json, see FORK-CHANGES.md) by running
the remote survey once for every parcel in range of your access points.

  address_survey.py points  --config sources.json --isp resources/isp.ron --out points.csv
  address_survey.py survey  --points points.csv --resources resources --binary target/release/bracket-heat \
                            --terrain ./terrain --out results.jsonl [--height 4] [--jobs 6]
  address_survey.py publish --points points.csv --results results.jsonl --config sources.json \
                            --out resources/address-layer-v2.json [--height 4]

points   Parcels (any OGR-readable file: shapefile, GeoPackage, GeoJSON...) within max_range_km of
         a tower in isp.ron -> one survey point each: the centre of the largest building in the
         parcel when a building-footprint file is given, else a point inside the parcel.
survey   Calls /mapclick for every point on a SEPARATE server instance (staged copy of your
         resources on --port, default 18001), so a live planner is never loaded. Resumable:
         re-run to continue; --restart to start over.
publish  Writes the compact file the planner serves. Every offered AP is kept per address, so
         technology and package views need no re-survey. The planner re-reads it on change.

sources.json (one entry per parcel file; field names are the columns in THAT file, omit any it lacks):
  { "keep_min_value": 1,
    "sources": { "XX": { "parcels": "/data/parcels/xx_parcels.shp", "vintage": "2025 tax year",
                         "fields": { "parcel_id": "PARCEL_ID", "county": "COUNTY", "address": "SITE_ADDR",
                                     "value": "TOTAL_VAL", "acres": "ACRES" },
                         "buildings": "/data/buildings/xx.geojson",
                         "buildings_vintage": "Microsoft US Building Footprints" } } }
A parcel is kept when it has a site address or an assessed value >= keep_min_value (skips bare land).
"buildings" expects line-delimited GeoJSON features such as the Microsoft US Building Footprints
state files (https://github.com/microsoft/USBuildingFootprints).

Needs: GDAL Python bindings (osgeo), shapely 2, numpy, requests.
"""
import argparse, collections, concurrent.futures as cf, csv, json, math, os, re, shutil, signal, subprocess, sys, time
from pathlib import Path

GRADES = {'clear': 0, 'check_mount': 1, 'unverified': 2, 'obstructed': 3}

def towers(isp):
    ron = Path(isp).read_text()
    t = [(float(a), float(b), float(c)) for a, b, c in re.findall(
        r'Tower\(\s*name: "[^"]*",\s*lat: ([-\d.]+),\s*lon: ([-\d.]+),\s*height_meters: [-\d.]+,\s*max_range_km: ([-\d.]+)', ron)]
    if not t: sys.exit(f'no towers found in {isp}')
    return t

def cmd_points(a):
    import numpy as np, shapely
    from osgeo import ogr
    ogr.UseExceptions()
    T = np.array(towers(a.isp)); c = json.load(open(a.config)); rows = []
    for name, src in c['sources'].items():
        F = src.get('fields', {}); t0 = time.time()
        ds = ogr.Open(src['parcels']); L = ds.GetLayer()
        pad = T[:, 2].max() / 111.32                 # coarse filter: box around every tower's range
        coslat = max(math.cos(math.radians(abs(T[:, 0]).max() + pad)), 0.05)
        L.SetSpatialFilterRect(T[:, 1].min() - pad / coslat, T[:, 0].min() - pad, T[:, 1].max() + pad / coslat, T[:, 0].max() + pad)
        geoms, meta = [], []
        get = lambda f, k: (f.GetField(F[k]) if k in F else None)
        for f in L:
            g = f.GetGeometryRef()
            if g is None or g.IsEmpty(): continue
            e = g.GetEnvelope(); lon, lat = (e[0] + e[1]) / 2, (e[2] + e[3]) / 2
            d = np.hypot((T[:, 0] - lat) * 111.32, (T[:, 1] - lon) * 111.32 * math.cos(math.radians(lat)))
            if not (d < T[:, 2]).any(): continue
            addr = str(get(f, 'address') or '').strip(); val = get(f, 'value') or 0
            if not addr and val < c.get('keep_min_value', 1): continue
            geoms.append(bytes(g.ExportToWkb()))
            meta.append((name, str(get(f, 'county') or ''), str(get(f, 'parcel_id') or ''), addr, get(f, 'acres') or 0))
        if not geoms: print(f'{name}: no parcels in range'); continue
        P = shapely.make_valid(shapely.from_wkb(geoms))
        print(f'{name}: {len(P):,} parcels in range kept ({time.time()-t0:.0f}s)', flush=True)
        pts = shapely.point_on_surface(P); basis = np.array(['parcel'] * len(P), dtype=object)
        if src.get('buildings') and os.path.exists(src['buildings']):
            bb = shapely.total_bounds(P); B = []
            with open(src['buildings']) as fh:
                for line in fh:                      # one feature per line
                    i = line.find('"coordinates":[[[')
                    if i < 0: continue
                    j = line.find(',', i + 17); k = line.find(']', j)
                    try: x, y = float(line[i+17:j]), float(line[j+1:k])
                    except ValueError: continue
                    if bb[0] <= x <= bb[2] and bb[1] <= y <= bb[3]:
                        try: B.append(shapely.geometry.shape(json.loads(line.rstrip().rstrip(','))['geometry']))
                        except Exception: pass
            if B:
                B = np.array(B, dtype=object); C = shapely.centroid(B); area = shapely.area(B)
                pi, bi = shapely.STRtree(C).query(P, predicate='contains')
                best = {}
                for p, b in zip(pi, bi):
                    if p not in best or area[b] > area[best[p]]: best[p] = b
                for p, b in best.items(): pts[p] = C[b]; basis[p] = 'building'
                print(f'{name}: {len(B):,} buildings in area; {len(best):,} parcels placed on a building', flush=True)
        for (st, county, pid, addr, acres), pt, bs in zip(meta, pts, basis):
            rows.append(dict(id=f'{st}:{county}:{pid}', source=st, county=county, parcel_id=pid, address=addr,
                             acres=round(float(acres), 3), lat=round(pt.y, 6), lon=round(pt.x, 6), point_basis=bs))
    if not rows: sys.exit('no parcels in range of any tower')
    seen = set(); uniq = [r for r in rows if not (r['id'] in seen or seen.add(r['id']))]
    with open(a.out, 'w', newline='') as fh:
        w = csv.DictWriter(fh, fieldnames=list(uniq[0])); w.writeheader(); w.writerows(uniq)
    print(f'{len(uniq):,} points -> {a.out} ({len(rows)-len(uniq)} duplicate ids dropped)',
          dict(collections.Counter(r['point_basis'] for r in uniq)))

def serve(a):
    """Start bracket-heat on a staged copy of the resources, listening on a.port only."""
    import requests
    src = Path(a.resources).resolve(); stage = Path(a.out).resolve().parent/'.survey-stage'/'resources'
    if stage.exists(): shutil.rmtree(stage)
    stage.mkdir(parents=True)
    for p in src.iterdir():                         # everything as-is, except the port
        if p.name != 'isp.ron': os.symlink(p, stage/p.name)
    (stage/'isp.ron').write_text(re.sub(r'listen_port:\s*\d+', f'listen_port: {a.port}', (src/'isp.ron').read_text()))
    exe = stage.parent/'bracket-heat'; shutil.copy2(a.binary, exe)   # a rebuild mid-survey cannot change results
    env = dict(os.environ, BRACKET_HEAT_RESOURCES=str(stage), TERRAIN_PATH=str(Path(a.terrain).resolve()),
               BRACKET_HEAT_ADDRESS='127.0.0.1')
    p = subprocess.Popen([str(exe)], cwd=stage.parent, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(120):
        try:
            if requests.get(f'http://127.0.0.1:{a.port}/health', timeout=2).ok: return p
        except requests.RequestException: time.sleep(0.5)
    p.kill(); sys.exit(f'survey server did not start on port {a.port}')

def cmd_survey(a):
    import requests
    pts = list(csv.DictReader(open(a.points))); res = Path(a.out)
    done = set()
    if res.exists() and not a.restart: done = {json.loads(l)['id'] for l in open(res) if l.strip()}
    elif res.exists(): res.unlink()
    todo = [p for p in pts if p['id'] not in done]
    if a.limit: todo = todo[:a.limit]
    print(f'{len(pts):,} points, {len(done):,} done, {len(todo):,} to survey at {a.height} m', flush=True)
    if not todo: return
    proc = serve(a); t0 = time.time()
    def one(p):
        try:
            r = requests.get(f"http://127.0.0.1:{a.port}/mapclick/{p['lat']}/{p['lon']}/{a.height}/0/0", timeout=900)
            if r.status_code != 200: return dict(id=p['id'], err=r.status_code)
            # [tower:ap, grade, rssi, required mount m, distance km] for every offered AP
            links = [[t['name'].split(' @')[0], GRADES.get(t['disposition'], 2), round(t['rssi'], 1),
                      None if t.get('min_mount_agl_m') is None else round(min(t['min_mount_agl_m'], 99.0), 1),
                      round(t['distance_km'], 2)] for t in r.json()['towers']]
            return dict(id=p['id'], l=links)
        except Exception as e:
            return dict(id=p['id'], err=type(e).__name__)
    try:
        with open(res, 'a') as fh, cf.ThreadPoolExecutor(a.jobs) as ex:
            for i, r in enumerate(ex.map(one, todo), 1):
                fh.write(json.dumps(r) + '\n')
                if i % 2000 == 0:
                    fh.flush(); el = time.time() - t0
                    print(f'{i:,}/{len(todo):,}  {i/el:.1f}/s  eta {(len(todo)-i)/(i/el)/3600:.1f} h', flush=True)
    finally:
        proc.send_signal(signal.SIGTERM); proc.wait(timeout=30)
    print(f'done in {(time.time()-t0)/3600:.2f} h', flush=True)

def cmd_publish(a):
    pts = {p['id']: p for p in csv.DictReader(open(a.points))}
    aps = {}; rows = []; c = collections.Counter()
    for l in open(a.results):
        r = json.loads(l)
        if 'err' in r or r['id'] not in pts: c['error'] += 1; continue
        p = pts[r['id']]
        links = [[aps.setdefault(n, len(aps)), g, rssi, m, d] for n, g, rssi, m, d in r['l']]
        c['with_ap' if links else 'none'] += 1
        rows.append([float(p['lat']), float(p['lon']), p['address'], p['id'], 1 if p['point_basis'] == 'building' else 0, links])
    srcs = json.load(open(a.config))['sources'] if a.config else {}
    meta = dict(version=2, generated=time.strftime('%Y-%m-%d'), cpe_height_m=a.height, count=len(rows),
                aps=[n for n, _ in sorted(aps.items(), key=lambda kv: kv[1])],
                row_columns=['lat', 'lon', 'address', 'id', 'on_building', 'links'],
                link_columns=['ap_index', 'grade(0 clear,1 check_mount,2 unverified,3 obstructed)', 'rssi', 'mount_m', 'distance_km'],
                sources={s: dict(parcels=v.get('vintage'), buildings=v.get('buildings_vintage')) for s, v in srcs.items()})
    dest = Path(a.out); tmp = dest.with_suffix('.tmp')
    tmp.write_text(json.dumps(dict(meta=meta, rows=rows), separators=(',', ':'))); os.replace(tmp, dest)
    print(f'{len(rows):,} addresses, {len(aps)} APs, {sum(len(r[5]) for r in rows):,} links -> {dest} '
          f'({dest.stat().st_size/1e6:.1f} MB)', dict(c))

if __name__ == '__main__':
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='cmd', required=True)
    p = sub.add_parser('points'); p.add_argument('--config', required=True); p.add_argument('--isp', required=True)
    p.add_argument('--out', default='points.csv')
    s = sub.add_parser('survey'); s.add_argument('--points', required=True); s.add_argument('--resources', required=True)
    s.add_argument('--binary', required=True); s.add_argument('--terrain', required=True); s.add_argument('--out', default='results.jsonl')
    s.add_argument('--port', type=int, default=18001); s.add_argument('--height', type=float, default=4)
    s.add_argument('--jobs', type=int, default=6); s.add_argument('--limit', type=int, default=0); s.add_argument('--restart', action='store_true')
    u = sub.add_parser('publish'); u.add_argument('--points', required=True); u.add_argument('--results', required=True)
    u.add_argument('--config'); u.add_argument('--out', required=True); u.add_argument('--height', type=float, default=4)
    a = ap.parse_args()
    {'points': cmd_points, 'survey': cmd_survey, 'publish': cmd_publish}[a.cmd](a)
