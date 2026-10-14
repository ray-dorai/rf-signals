#!/usr/bin/env python3
"""Cook USGS 3DEP LAZ tiles into a .bheat store.

Recent 3DEP projects are often published as LAZ tiles only, before (or instead of) the
Entwine/EPT copy. This lists a project's tiles from the USGS National Map API, then for each
tile: download -> PDAL (keep classes 1-6, reproject to WGS84) -> `terrain_cooker cook`.

  lidar_laz.py list --bbox W,S,E,N                         (prints the projects in the box)
  lidar_laz.py list --bbox W,S,E,N --project XX_Example_2024 [--out tiles.json]
  lidar_laz.py cook --tiles tiles.json --store ./bheat-new --cooker ./terrain_cooker \
                    --terrain ./terrain [--within W,S,E,N] [--jobs 8] [--limit N]

Cook a NEW flight into a NEW, empty store. The cooker keeps the per-cell maximum, so cooking a
newer flight on top of older tiles keeps every tree and building that has since been removed.
Merge the result into an existing store with bheat_overlay.py.

Downloads run in parallel; cooks run one at a time, because adjacent LAZ tiles share .bheat
tiles and the cooker read-modify-writes them. Resumable: finished tiles are listed in
<store>/cooked.done. Needs: pdal, curl, shapely (cook --within only).
"""
import argparse, collections, json, os, shutil, subprocess, sys, tempfile, threading, time
import urllib.parse, urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed

TNM = 'https://tnmaccess.nationalmap.gov/api/v1/products'

def bbox_arg(s):
    v = [float(x) for x in s.split(',')]
    if len(v) != 4 or not (v[0] < v[2] and v[1] < v[3]):
        raise argparse.ArgumentTypeError('expected W,S,E,N')
    return v

def cmd_list(a):
    items, off = [], 0
    while True:
        q = urllib.parse.urlencode({'datasets': 'Lidar Point Cloud (LPC)', 'bbox': ','.join(map(str, a.bbox)),
                                    'max': 500, 'offset': off, 'outputFormat': 'JSON'})
        with urllib.request.urlopen(f'{TNM}?{q}', timeout=120) as r:
            it = json.load(r).get('items', [])
        items += it; off += len(it)
        if len(it) < 500: break
    projects = collections.Counter(x['downloadURL'].split('/Projects/')[1].split('/')[0]
                                   for x in items if '/Projects/' in (x.get('downloadURL') or ''))
    if not a.project:
        for name, k in sorted(projects.items()): print(f'{k:6d}  {name}')
        return
    tiles = [dict(url=x['downloadURL'], bbox=x['boundingBox'], size=x.get('sizeInBytes'))
             for x in items if a.project in (x.get('downloadURL') or '') and x['downloadURL'].lower().endswith('.laz')]
    json.dump(tiles, open(a.out, 'w'))
    print(f'{len(items)} products in the box; {len(tiles)} LAZ tiles match "{a.project}", '
          f'{sum(t["size"] or 0 for t in tiles)/1e9:.1f} GB -> {a.out}')
    if not tiles: print('projects in this box:', ', '.join(sorted(projects)) or 'none')

def cmd_cook(a):
    store = os.path.realpath(a.store)
    for p in a.protect:
        if store == os.path.realpath(p): sys.exit(f'refusing to cook into protected store {p}')
    os.makedirs(store, exist_ok=True)
    tiles = json.load(open(a.tiles))
    if a.within:
        from shapely.geometry import box
        w = box(*a.within)
        tiles = [t for t in tiles if w.intersects(box(t['bbox']['minX'], t['bbox']['minY'], t['bbox']['maxX'], t['bbox']['maxY']))]
    state = os.path.join(store, 'cooked.done')
    done = {l.strip() for l in open(state)} if os.path.exists(state) else set()
    todo = [t for t in tiles if os.path.basename(t['url']) not in done]
    if a.limit: todo = todo[:a.limit]
    print(f'{len(tiles)} tiles selected, {len(done)} done, {len(todo)} to do, '
          f'{sum(t.get("size") or 0 for t in todo)/1e9:.1f} GB', flush=True)
    stage = a.stage or os.path.join(store, '.stage'); os.makedirs(stage, exist_ok=True)
    env = dict(os.environ, BHEAT_PATH=store, TERRAIN_PATH=os.path.realpath(a.terrain))
    cook_lock = threading.Lock(); t0 = time.time(); n = {'ok': 0, 'fail': 0}

    def one(t):
        name = os.path.basename(t['url']); work = tempfile.mkdtemp(prefix='laz_', dir=stage)
        try:
            laz = os.path.join(work, name)
            r = subprocess.run(['curl', '-s', '-f', '--retry', '5', '--retry-delay', '10', '--max-time', '3600', '-o', laz, t['url']])
            if r.returncode or (t.get('size') and os.path.getsize(laz) != t['size']):
                return name, 'fail', f'download rc {r.returncode}'
            cook = os.path.join(work, 'cook'); os.makedirs(cook)
            # Classes 1-6: unclassified, ground, vegetation, buildings. Drops noise, water, bridges.
            # The cooker needs WGS84 with enough precision to place points within a 1.3 m cell.
            pipe = {'pipeline': [{'type': 'readers.las', 'filename': laz},
                                 {'type': 'filters.range', 'limits': 'Classification[1:6]'},
                                 {'type': 'filters.reprojection', 'out_srs': 'EPSG:4326'},
                                 {'type': 'writers.las', 'filename': os.path.join(cook, 'block.las'),
                                  'scale_x': 1e-7, 'scale_y': 1e-7, 'scale_z': 0.01,
                                  'offset_x': 'auto', 'offset_y': 'auto', 'offset_z': 'auto'}]}
            pj = os.path.join(work, 'pipe.json'); json.dump(pipe, open(pj, 'w'))
            r = subprocess.run(['pdal', 'pipeline', pj], capture_output=True, text=True, timeout=3600)
            if r.returncode: return name, 'fail', 'pdal ' + r.stderr[-150:]
            os.unlink(laz)
            with cook_lock:
                c = subprocess.run([a.cooker, 'cook', cook], capture_output=True, text=True, env=env, timeout=3600)
                if c.returncode or 'no failures' not in c.stdout:
                    return name, 'fail', 'cook ' + (c.stdout + c.stderr)[-150:]
                with open(state, 'a') as fh: fh.write(name + '\n')
            return name, 'ok', ''
        except Exception as e:
            return name, 'fail', str(e)[:150]
        finally:
            shutil.rmtree(work, ignore_errors=True)

    with ThreadPoolExecutor(a.jobs) as pool:
        for i, f in enumerate(as_completed([pool.submit(one, t) for t in todo]), 1):
            name, st, msg = f.result(); n[st] += 1
            el = time.time() - t0
            print(f'[{i}/{len(todo)}] {name} {st} {msg}  eta {(len(todo)-i)*el/i/3600:.1f}h', flush=True)
    print(f'DONE ok {n["ok"]} fail {n["fail"]} in {(time.time()-t0)/3600:.1f} h (re-run to retry failures)', flush=True)
    sys.exit(1 if n['fail'] else 0)

if __name__ == '__main__':
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='cmd', required=True)
    l = sub.add_parser('list', help='list a 3DEP project\'s LAZ tiles inside a box')
    l.add_argument('--bbox', type=bbox_arg, required=True, help='W,S,E,N in degrees')
    l.add_argument('--project', help='project or work-unit name as it appears in the download URL; omit to list projects')
    l.add_argument('--out', default='tiles.json')
    c = sub.add_parser('cook', help='download and cook the listed tiles')
    c.add_argument('--tiles', required=True)
    c.add_argument('--store', required=True, help='output .bheat directory (use a new one per flight)')
    c.add_argument('--cooker', required=True, help='path to the terrain_cooker binary')
    c.add_argument('--terrain', required=True, help='SRTM .hgt directory (TERRAIN_PATH)')
    c.add_argument('--within', type=bbox_arg, help='only tiles touching this W,S,E,N box')
    c.add_argument('--protect', action='append', default=[], help='store that must never be written (repeatable)')
    c.add_argument('--stage', help='scratch directory for downloads (default <store>/.stage)')
    c.add_argument('--jobs', type=int, default=8, help='parallel downloads')
    c.add_argument('--limit', type=int, default=0)
    a = ap.parse_args()
    {'list': cmd_list, 'cook': cmd_cook}[a.cmd](a)
