use super::SrtmTile;
use crate::Distance;
use crate::LatLon;
use lazy_static::*;
use memmap::{Mmap, MmapOptions};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::fs::File;
use std::sync::Arc;

lazy_static! {
    /// `None` is a cached negative: the tile was looked for and is not on disk.
    /// Without it every query for an uncovered area re-stats the filesystem.
    static ref TILE_CACHE: RwLock<HashMap<SrtmTile, Option<Arc<Mmap>>>> =
        RwLock::new(HashMap::new());
}

/// SRTM void marker. Also the value a signed -32768 takes when a tile is
/// read as unsigned, which is how voids used to leak through as 32 km peaks.
const VOID: i16 = -32768;
/// Lowest plausible land elevation (Dead Sea shore is about -430 m).
const MIN_PLAUSIBLE_M: i16 = -500;

/// Ground elevation at a point, taking the best resolution that yields a
/// usable sample.
///
/// The three .hgt trees are tried finest-first. A tile that is missing, short,
/// or voided at this particular point falls through to the next coarser tree
/// rather than failing the query, so a gap in the 1/3" coverage degrades
/// resolution instead of producing a hole.
pub fn get_altitude(loc: &LatLon, terrain_path: &str) -> Option<Distance> {
    for tile in [loc.to_srtm_third(), loc.to_srtm3(), loc.to_srtm1()].iter() {
        if let Some(mapped) = tile_mmap(tile, terrain_path) {
            if let Some(elevation) = get_elevation(loc, tile, &mapped) {
                return Some(elevation);
            }
        }
    }
    None
}

/// Whether any .hgt tile at all covers this point. Used by preflight tooling.
pub fn has_terrain_coverage(loc: &LatLon, terrain_path: &str) -> bool {
    [loc.to_srtm_third(), loc.to_srtm3(), loc.to_srtm1()]
        .iter()
        .any(|t| tile_mmap(t, terrain_path).is_some())
}

fn tile_mmap(tile: &SrtmTile, terrain_path: &str) -> Option<Arc<Mmap>> {
    {
        let reader = TILE_CACHE.read();
        if let Some(entry) = reader.get(tile) {
            return entry.clone();
        }
    }

    let mut writer = TILE_CACHE.write();
    if let Some(entry) = writer.get(tile) {
        return entry.clone();
    }

    let filename = tile.filename(terrain_path);
    let mapped = File::open(&filename)
        .ok()
        .and_then(|f| match f.metadata() {
            Ok(m) if m.len() as usize >= tile.expected_bytes() => Some(f),
            _ => None,
        })
        .and_then(|f| unsafe { MmapOptions::new().map(&f).ok() })
        .map(Arc::new);

    writer.insert(*tile, mapped.clone());
    mapped
}

fn get_elevation(loc: &LatLon, tile: &SrtmTile, memory: &Mmap) -> Option<Distance> {
    let floor = loc.floor();
    let offset = match tile {
        // 1201 samples across a whole degree: 3 arc-second data.
        SrtmTile::Srtm1 { .. } => {
            const BYTES_PER_SAMPLE: usize = 2;
            const N_SAMPLES: usize = 1201;
            const SAMPLES_PER_DEGREE: usize = N_SAMPLES - 1;
            let row =
                ((floor.lat() + 1.0 - loc.lat()) * SAMPLES_PER_DEGREE as f64).round() as usize;
            let col = ((loc.lon() - floor.lon()) * SAMPLES_PER_DEGREE as f64).round() as usize;
            BYTES_PER_SAMPLE * ((row * N_SAMPLES) + col)
        }
        // 3601 samples across a whole degree: 1 arc-second data.
        SrtmTile::Srtm3 { .. } => {
            const BYTES_PER_SAMPLE: usize = 2;
            const N_SAMPLES: usize = 3601;
            const SAMPLES_PER_DEGREE: usize = N_SAMPLES - 1;
            let row =
                ((floor.lat() + 1.0 - loc.lat()) * SAMPLES_PER_DEGREE as f64).round() as usize;
            let col = ((loc.lon() - floor.lon()) * SAMPLES_PER_DEGREE as f64).round() as usize;
            BYTES_PER_SAMPLE * ((row * N_SAMPLES) + col)
        }
        // 1201 samples across one ninth of a degree: 1/3 arc-second data.
        SrtmTile::SrtmThird {
            lat_tile, lon_tile, ..
        } => {
            const SAMPLES_PER_DEGREE: usize = 1200;
            const SAMPLES_PER_EXTENT: usize = 1201;
            const BYTES_PER_SAMPLE: usize = 2;
            let lat_extent_base_10 = loc.lat() * 10.0 - floor.lat() * 10.0;
            let lat_extent_base_9 = ((lat_extent_base_10 / 10.0) * 9.0) + 1.0;
            let lon_extent_base_10 = loc.lon() * 10.0 - floor.lon() * 10.0;
            let lon_extent_base_9 = ((lon_extent_base_10 / 10.0) * 9.0) + 1.0;
            let lat_percent = lat_extent_base_9 - *lat_tile as f64;
            let lon_percent = lon_extent_base_9 - *lon_tile as f64;
            let row = ((1.0 - lat_percent) * SAMPLES_PER_DEGREE as f64).round() as usize;
            let col = (lon_percent * SAMPLES_PER_DEGREE as f64).round() as usize;
            BYTES_PER_SAMPLE * ((row * SAMPLES_PER_EXTENT) + col)
        }
    };

    // .hgt is big-endian signed 16-bit.
    let high_byte = *memory.get(offset)?;
    let low_byte = *memory.get(offset + 1)?;
    let h = (((high_byte as u16) << 8) | low_byte as u16) as i16;

    if h == VOID || h < MIN_PLAUSIBLE_M {
        return None;
    }
    Some(Distance::with_meters(h as f64))
}

#[cfg(test)]
mod test {
    use super::get_altitude;
    use super::LatLon;

    #[test]
    fn test_srtm_third_elevation() {
        let loc = LatLon::new(38.947775, -92.323385);
        let altitude = get_altitude(&loc, "resources");
        assert!(altitude.is_some());
        if let Some(alt) = altitude {
            assert_eq!(alt.as_meters(), 232.0);
        }
    }

    #[test]
    fn missing_terrain_is_none_not_zero() {
        // Mid-Pacific: no tile ships with the crate.
        let loc = LatLon::new(0.5, -150.5);
        assert!(get_altitude(&loc, "resources").is_none());
    }
}
