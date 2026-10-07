use super::MapTile;
use crate::mapping::terrain_config::terrain_path;
use crate::srtm::get_altitude;
use crate::Distance;
use crate::LatLon;
use lazy_static::*;
use memmap::{Mmap, MmapOptions};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::fs::File;

/// A bheat tile is 768 x 768 cells, two u16 entries (ground, clutter) per cell.
const ROW_SIZE: usize = 768;
const COL_SIZE: usize = 768;
const EXPECTED_BYTES: usize = ROW_SIZE * COL_SIZE * 2 * 2;

lazy_static! {
    static ref HEAT_CACHE: RwLock<HashMap<String, Option<Mmap>>> = RwLock::new(HashMap::new());
}

/// Ground and clutter altitude at a point.
///
/// Resolution order:
///   1. the cooked LiDAR `.bheat` tile, if one covers this point;
///   2. the SRTM `.hgt` pyramid, if a terrain path has been registered via
///      `set_terrain_path` (clutter is reported as equal to ground, because
///      SRTM carries no clutter layer);
///   3. `None`.
///
/// Step 2 is what keeps coverage honest outside the cooked LiDAR footprint.
/// Without it a missing tile reads as sea level, which looks like an RF result
/// but is not one.
pub fn heat_altitude(lat: f64, lon: f64, heat_path: &str) -> Option<(Distance, Distance)> {
    if let Some(hit) = bheat_altitude(lat, lon, heat_path) {
        return Some(hit);
    }
    srtm_fallback(lat, lon)
}

/// Ground and clutter strictly from the cooked LiDAR store, with no fallback.
/// Used by tooling that needs to know whether a tile actually exists.
pub fn bheat_altitude(lat: f64, lon: f64, heat_path: &str) -> Option<(Distance, Distance)> {
    let filename = MapTile::get_tile_name(lat, lon, heat_path);

    let read_lock = HEAT_CACHE.read();
    if let Some(tile_file) = read_lock.get(&filename) {
        return match tile_file {
            Some(mm) => get_elevation(lat, lon, mm),
            None => None,
        };
    }
    std::mem::drop(read_lock);

    let mut write_lock = HEAT_CACHE.write();
    // Another thread may have populated the entry while we waited for the lock.
    if let Some(tile_file) = write_lock.get(&filename) {
        return match tile_file {
            Some(mm) => get_elevation(lat, lon, mm),
            None => None,
        };
    }

    let mapped = File::open(&filename)
        .ok()
        .and_then(|f| match f.metadata() {
            // A short tile is a truncated or still-being-written cook. Reject it
            // rather than mapping it and indexing off the end later.
            Ok(m) if m.len() as usize >= EXPECTED_BYTES => Some(f),
            _ => None,
        })
        .and_then(|f| unsafe { MmapOptions::new().map(&f).ok() });

    let result = mapped.as_ref().and_then(|mm| get_elevation(lat, lon, mm));
    write_lock.insert(filename, mapped);
    result
}

fn srtm_fallback(lat: f64, lon: f64) -> Option<(Distance, Distance)> {
    let path = terrain_path()?;
    let ground = get_altitude(&LatLon::new(lat, lon), &path)?;
    Some((ground, ground))
}

fn get_elevation(lat: f64, lon: f64, memory: &Mmap) -> Option<(Distance, Distance)> {
    let index = MapTile::index(lat, lon);
    let offset = index * 2;
    let heights = bytemuck::cast_slice::<u8, u16>(memory);
    let ground = heights.get(offset)? / 10;
    let clutter = heights.get(offset + 1)? / 10;
    Some((
        Distance::with_meters(ground),
        Distance::with_meters(clutter),
    ))
}

/// Cooked-LiDAR (ground, surface) in metres, decimetre precision, for many
/// points at once; `None` where no cooked tile covers a point. No SRTM
/// fallback: callers use this to judge obstructions, and terrain-only data
/// cannot show a tree or a building.
///
/// `bheat_altitude` truncates to whole metres and takes the cache lock per
/// point; this keeps the stored decimetres and locks once per call.
pub fn bheat_profile(points: &[(f64, f64)], heat_path: &str) -> Vec<Option<(f64, f64)>> {
    // Populate the cache for every tile the profile touches.
    let mut names: Vec<String> = points
        .iter()
        .map(|(la, lo)| MapTile::get_tile_name(*la, *lo, heat_path))
        .collect();
    {
        let mut uniq = names.clone();
        uniq.sort();
        uniq.dedup();
        for (n, (la, lo)) in uniq.iter().filter_map(|n| {
            names.iter().position(|m| m == n).map(|i| (n, points[i]))
        }) {
            if !HEAT_CACHE.read().contains_key(n) {
                let _ = bheat_altitude(la, lo, heat_path);
            }
        }
    }
    let cache = HEAT_CACHE.read();
    points
        .iter()
        .zip(names.drain(..))
        .map(|((la, lo), name)| {
            let mm = cache.get(&name)?.as_ref()?;
            let heights = bytemuck::cast_slice::<u8, u16>(mm);
            let idx = clamped_index(*la, *lo);
            let g = *heights.get(idx * 2)? as f64 / 10.0;
            let c = *heights.get(idx * 2 + 1)? as f64 / 10.0;
            Some((g, c))
        })
        .collect()
}

/// `MapTile::index` with the row/column clamp the cooker applies, so a point
/// on a tile's north or east edge reads its own tile, not the next row.
fn clamped_index(lat: f64, lon: f64) -> usize {
    let (la, lo) = (lat.abs(), lon.abs());
    let base_lat = la.floor() + (la.fract() * 100.0).floor() / 100.0;
    let base_lon = lo.floor() + (lo.fract() * 100.0).floor() / 100.0;
    let row = (((la - base_lat) * 100.0 * ROW_SIZE as f64) as usize).min(ROW_SIZE - 1);
    let col = (((lo - base_lon) * 100.0 * COL_SIZE as f64) as usize).min(COL_SIZE - 1);
    row * COL_SIZE + col
}
