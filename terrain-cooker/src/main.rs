//! terrain_cooker -- build and inspect the .bheat clutter store.
//!
//! Subcommands:
//!   cook   [dir]      merge .las/.laz point clouds into .bheat tiles
//!   seed   <sites>    pre-build .bheat tiles from SRTM over a site list
//!   probe  <lat> <lon>  report the elevation the servers would see
//!   verify <sites>    check every tile a site list needs
//!
//! Every path comes from the environment (LIDAR_PATH, BHEAT_PATH, TERRAIN_PATH).
//! The original build had four absolute paths compiled in, all pointing at the
//! upstream author's home directory.
//!
//! COORDINATE SYSTEMS. This cooker requires point clouds already in WGS84
//! lon/lat. It does not link PROJ. The old code read a legacy GeoTIFF VLR and
//! unwrapped it, which panics on every modern LAS 1.4 tile (they carry OGC WKT
//! instead), and the proj crate it used is pinned to a PROJ release five major
//! versions behind the one on this machine. Reprojection is done up front by
//! PDAL instead -- see bin/lidar-prep.sh -- which also handles LAZ and gives one
//! place to fix a bad CRS rather than two.

use las::{point::Classification, Read as LasRead, Reader};
use rayon::prelude::*;
use rf_signal_algorithms::{
    bheat::bheat_altitude, set_terrain_path, srtm::get_altitude, Distance, LatLon,
};
use std::collections::HashMap;
use std::fs::read_dir;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

mod tile_writer;
use tile_writer::*;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

struct Paths {
    lidar: String,
    bheat: String,
    terrain: String,
}

impl Paths {
    fn from_env() -> Self {
        let p = Paths {
            lidar: env_or("LIDAR_PATH", "./lidar"),
            bheat: env_or("BHEAT_PATH", "./bheat"),
            terrain: env_or("TERRAIN_PATH", "./terrain"),
        };
        set_terrain_path(&p.terrain);
        p
    }
}

/// Points whose coordinates are not plausible lon/lat. Catches a projected
/// cloud that slipped past the prep step, which would otherwise be silently
/// binned into a tile on the other side of the planet.
fn looks_like_lonlat(x: f64, y: f64) -> bool {
    (-180.0..=180.0).contains(&x) && (-90.0..=90.0).contains(&y)
}

/// Decide whether Z is metres or feet by comparing ground returns against the
/// SRTM baseline. The original took the first three ground points and let the
/// last one win; a single outlier flipped the vertical scale for a whole tile.
fn detect_z_scale(path: &Path, terrain: &str) -> f64 {
    let mut reader = match Reader::from_path(path) {
        Ok(r) => r,
        Err(_) => return 1.0,
    };
    let (mut metres_votes, mut feet_votes) = (0i32, 0i32);
    for p in reader
        .points()
        .filter_map(|p| p.ok())
        .filter(|p| p.classification == Classification::Ground)
        .take(500)
    {
        if !looks_like_lonlat(p.x, p.y) {
            continue;
        }
        let known = match get_altitude(&LatLon::new(p.y, p.x), terrain) {
            Some(d) => d.as_meters(),
            None => continue,
        };
        if known <= 0.0 {
            continue;
        }
        let margin = (known * 0.15).max(20.0);
        if (p.z - known).abs() <= margin {
            metres_votes += 1;
        } else if (p.z * 0.3048 - known).abs() <= margin {
            feet_votes += 1;
        }
    }
    if feet_votes > metres_votes {
        println!("    Z looks like FEET ({} vs {} votes)", feet_votes, metres_votes);
        0.3048
    } else {
        println!("    Z looks like METRES ({} vs {} votes)", metres_votes, feet_votes);
        1.0
    }
}

fn cook_file(path: &Path, paths: &Paths) -> Result<(usize, u64, u64), String> {
    let scale = detect_z_scale(path, &paths.terrain);
    let mut reader = Reader::from_path(path).map_err(|e| format!("{}", e))?;

    let mut cache: HashMap<String, MapTile> = HashMap::new();
    let (mut used, mut rejected) = (0u64, 0u64);

    for p in reader.points().filter_map(|p| p.ok()) {
        if !looks_like_lonlat(p.x, p.y) {
            rejected += 1;
            continue;
        }
        let (lat, lon) = (p.y, p.x);
        let z = p.z * scale;
        if !(-500.0..=9000.0).contains(&z) {
            rejected += 1;
            continue;
        }
        let name = MapTile::get_tile_name(lat, lon, &paths.bheat);
        let tile = cache
            .entry(name)
            .or_insert_with(|| MapTile::get_tile(lat, lon, &paths.bheat, &paths.terrain));
        let idx = tile.index(lat, lon);
        let decimetres = (z * 10.0) as u16;
        match p.classification {
            Classification::Ground => tile.store_ground(idx, decimetres),
            _ => tile.store_clutter(idx, decimetres),
        }
        used += 1;
    }

    let mut written = 0;
    for tile in cache.values() {
        if tile.is_dirty() {
            tile.save().map_err(|e| format!("save: {}", e))?;
            written += 1;
        }
    }
    Ok((written, used, rejected))
}

fn cmd_cook(dir: Option<String>, paths: &Paths) {
    let root = PathBuf::from(dir.unwrap_or_else(|| paths.lidar.clone()));
    if !root.is_dir() {
        eprintln!("not a directory: {}", root.display());
        std::process::exit(2);
    }
    std::fs::create_dir_all(&paths.bheat).ok();

    let mut files = Vec::new();
    for entry in read_dir(&root).expect("cannot read lidar directory").flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        // Match on a lowercased extension, and tolerate its absence. The old
        // test was `extension().unwrap() == "las"`, which skipped every .laz and
        // panicked on any extensionless file sharing the directory.
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        if ext == "las" || ext == "laz" {
            files.push(p);
        }
    }
    files.sort();
    println!("{} point cloud(s) under {}", files.len(), root.display());

    let failures = Mutex::new(Vec::new());
    let done = Mutex::new(0usize);
    files.par_iter().for_each(|path| {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        println!("  {}", name);
        // One bad file must not abandon the run: the cooker is additive and a
        // multi-day cook that dies on file 900 of 3000 is a very expensive way
        // to discover one corrupt download.
        match cook_file(path, paths) {
            Ok((written, used, rejected)) => {
                let mut d = done.lock().unwrap();
                *d += 1;
                println!(
                    "  {} [{}/{}] {} tiles, {} points, {} rejected",
                    name, *d, files.len(), written, used, rejected
                );
            }
            Err(e) => {
                eprintln!("  FAILED {}: {}", name, e);
                failures.lock().unwrap().push((name, e));
            }
        }
    });

    let failures = failures.into_inner().unwrap();
    if failures.is_empty() {
        println!("cook complete, no failures");
    } else {
        println!("cook complete with {} FAILURES:", failures.len());
        for (n, e) in &failures {
            println!("   {}: {}", n, e);
        }
        std::process::exit(1);
    }
}

struct Site {
    name: String,
    lat: f64,
    lon: f64,
    radius_km: f64,
}

fn read_sites(path: &str, radius_override: Option<f64>) -> Vec<Site> {
    let text = std::fs::read_to_string(path).expect("cannot read site list");
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if n == 0 || line.trim().is_empty() {
            continue;
        }
        // entity,name,lat,lon,height_m,max_range_km,ap_count
        let mut f = Vec::new();
        let (mut cur, mut quoted) = (String::new(), false);
        for c in line.chars() {
            match c {
                '"' => quoted = !quoted,
                ',' if !quoted => f.push(std::mem::take(&mut cur)),
                _ => cur.push(c),
            }
        }
        f.push(cur);
        if f.len() < 6 {
            continue;
        }
        let (lat, lon) = (f[2].parse::<f64>(), f[3].parse::<f64>());
        if let (Ok(lat), Ok(lon)) = (lat, lon) {
            out.push(Site {
                name: f[1].clone(),
                lat,
                lon,
                radius_km: radius_override.unwrap_or_else(|| f[5].parse().unwrap_or(16.09)),
            });
        }
    }
    out
}

/// Every 0.01 x 0.01 degree tile a set of site buffers touches.
fn tiles_for(sites: &[Site]) -> Vec<(i64, i64)> {
    let mut set = std::collections::BTreeSet::new();
    for s in sites {
        let dlat = s.radius_km / 111.32;
        let dlon = s.radius_km / (111.32 * s.lat.to_radians().cos().max(1e-6));
        let (a0, a1) = (((s.lat - dlat) * 100.0).floor() as i64, ((s.lat + dlat) * 100.0).ceil() as i64);
        let (b0, b1) = (((s.lon - dlon) * 100.0).floor() as i64, ((s.lon + dlon) * 100.0).ceil() as i64);
        for a in a0..=a1 {
            for b in b0..=b1 {
                let (clat, clon) = (a as f64 / 100.0, b as f64 / 100.0);
                let (dy, dx) = ((clat - s.lat) / dlat, (clon - s.lon) / dlon);
                if dy * dy + dx * dx <= 1.0 {
                    set.insert((a, b));
                }
            }
        }
    }
    set.into_iter().collect()
}

fn cmd_seed(sites_file: &str, radius: Option<f64>, paths: &Paths) {
    let sites = read_sites(sites_file, radius);
    let tiles = tiles_for(&sites);
    println!(
        "{} sites -> {} tiles ({:.1} GiB if all are new)",
        sites.len(),
        tiles.len(),
        tiles.len() as f64 * TILE_BYTES as f64 / (1024.0 * 1024.0 * 1024.0)
    );
    std::fs::create_dir_all(&paths.bheat).ok();

    let done = std::sync::atomic::AtomicUsize::new(0);
    tiles.par_iter().for_each(|(a, b)| {
        let (lat, lon) = (*a as f64 / 100.0, *b as f64 / 100.0);
        let tile = MapTile::get_tile(lat, lon, &paths.bheat, &paths.terrain);
        if tile.is_dirty() {
            if let Err(e) = tile.save() {
                eprintln!("  save failed at {},{}: {}", lat, lon, e);
            }
        }
        let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        if n % 250 == 0 {
            println!("  {}/{}", n, tiles.len());
        }
    });
    println!("seed complete");
}

fn cmd_probe(lat: f64, lon: f64, paths: &Paths) {
    let srtm = get_altitude(&LatLon::new(lat, lon), &paths.terrain);
    let bheat = bheat_altitude(lat, lon, &paths.bheat);
    let served = rf_signal_algorithms::bheat::heat_altitude(lat, lon, &paths.bheat);
    println!("{:.6}, {:.6}", lat, lon);
    println!("  tile        {}", MapTile::get_tile_name(lat, lon, &paths.bheat));
    println!(
        "  srtm        {}",
        srtm.map(|d| format!("{:.1} m", d.as_meters()))
            .unwrap_or_else(|| "NONE".into())
    );
    println!(
        "  bheat       {}",
        bheat
            .map(|(g, c)| format!("ground {:.1} m, clutter {:.1} m", g.as_meters(), c.as_meters()))
            .unwrap_or_else(|| "not cooked".into())
    );
    println!(
        "  SERVED      {}",
        served
            .map(|(g, c)| format!("ground {:.1} m, clutter {:.1} m", g.as_meters(), c.as_meters()))
            .unwrap_or_else(|| "NONE -- would render as sea level".into())
    );
    if served.is_none() {
        std::process::exit(1);
    }
}

fn cmd_verify(sites_file: &str, radius: Option<f64>, paths: &Paths) {
    let sites = read_sites(sites_file, radius);
    let mut no_terrain = Vec::new();
    let mut no_bheat = 0usize;
    for s in &sites {
        if get_altitude(&LatLon::new(s.lat, s.lon), &paths.terrain).is_none() {
            no_terrain.push(s.name.clone());
        }
        if bheat_altitude(s.lat, s.lon, &paths.bheat).is_none() {
            no_bheat += 1;
        }
    }
    let tiles = tiles_for(&sites);
    let cooked = tiles
        .iter()
        .filter(|(a, b)| {
            Path::new(&MapTile::get_tile_name(
                *a as f64 / 100.0,
                *b as f64 / 100.0,
                &paths.bheat,
            ))
            .exists()
        })
        .count();

    println!("sites                 {}", sites.len());
    println!("  no terrain under it {}", no_terrain.len());
    println!("  no .bheat under it  {}", no_bheat);
    println!("tiles in buffers      {}", tiles.len());
    println!("  cooked              {} ({:.1}%)", cooked, cooked as f64 / tiles.len() as f64 * 100.0);
    if !no_terrain.is_empty() {
        println!("\nSITES WITH NO TERRAIN -- these render as sea level:");
        for n in no_terrain.iter().take(40) {
            println!("   {}", n);
        }
        std::process::exit(1);
    }
    println!("\nevery site has terrain under it");
}

fn usage() -> ! {
    eprintln!(
        "usage:\n  \
         terrain_cooker cook   [lidar-dir]\n  \
         terrain_cooker seed   <sites.csv> [radius_km]\n  \
         terrain_cooker probe  <lat> <lon>\n  \
         terrain_cooker verify <sites.csv> [radius_km]\n\n\
         paths come from LIDAR_PATH, BHEAT_PATH and TERRAIN_PATH"
    );
    std::process::exit(2)
}

fn main() {
    let paths = Paths::from_env();
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("cook") => cmd_cook(args.get(2).cloned(), &paths),
        Some("seed") => cmd_seed(
            args.get(2).unwrap_or_else(|| usage()),
            args.get(3).and_then(|s| s.parse().ok()),
            &paths,
        ),
        Some("probe") => {
            let lat = args.get(2).and_then(|s| s.parse().ok()).unwrap_or_else(|| usage());
            let lon = args.get(3).and_then(|s| s.parse().ok()).unwrap_or_else(|| usage());
            cmd_probe(lat, lon, &paths)
        }
        Some("verify") => cmd_verify(
            args.get(2).unwrap_or_else(|| usage()),
            args.get(3).and_then(|s| s.parse().ok()),
            &paths,
        ),
        _ => usage(),
    }
}
