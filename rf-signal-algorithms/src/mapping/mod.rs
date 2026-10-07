pub mod latlon;
pub mod srtm;
pub mod terrain_config;
pub use latlon::LatLon;
pub mod bheat;
use bheat::heat_altitude;
use rayon::prelude::*;

use crate::{
    geometry::{haversine_distance, haversine_intermediate},
    Distance,
};

/// Create a grid of LatLon entries for a bounded tile, returning (x, y, LatLon).
/// Used as a starting point for creating map tiles.
pub fn lat_lon_tile(
    swlat: f64,
    swlon: f64,
    nelat: f64,
    nelon: f64,
    tile_size: usize,
) -> Vec<(u32, u32, LatLon)> {
    // Counted loops, not `while lon < nelon`. Accumulating a float step could
    // emit a tile_size+1'th column, and every caller uses the returned x and y
    // to index a fixed tile_size x tile_size image -- so the overrun panicked
    // the worker thread and the tile came back as a 200 with an empty body.
    let lat_step = (nelat - swlat) / tile_size as f64;
    let lon_step = (nelon - swlon) / tile_size as f64;
    let mut points = Vec::with_capacity(tile_size * tile_size);
    for y in 0..tile_size {
        let lat = swlat + (lat_step * y as f64);
        for x in 0..tile_size {
            points.push((x as u32, y as u32, LatLon::new(lat, swlon + (lon_step * x as f64))));
        }
    }
    points
}

#[cfg(test)]
mod tile_tests {
    use super::lat_lon_tile;

    #[test]
    fn tile_is_exactly_square_and_in_range() {
        for size in [16usize, 256, 512] {
            let pts = lat_lon_tile(39.39, -95.61, 39.41, -95.58, size);
            assert_eq!(pts.len(), size * size, "wrong point count for {}", size);
            assert!(pts.iter().all(|(x, y, _)| (*x as usize) < size && (*y as usize) < size));
        }
    }

    #[test]
    fn tile_spans_the_requested_box() {
        let pts = lat_lon_tile(39.0, -95.0, 39.5, -94.5, 8);
        let lats: Vec<f64> = pts.iter().map(|(_, _, p)| p.lat()).collect();
        let lons: Vec<f64> = pts.iter().map(|(_, _, p)| p.lon()).collect();
        assert!(lats.iter().cloned().fold(f64::MAX, f64::min) >= 39.0);
        assert!(lats.iter().cloned().fold(f64::MIN, f64::max) < 39.5);
        assert!(lons.iter().cloned().fold(f64::MAX, f64::min) >= -95.0);
        assert!(lons.iter().cloned().fold(f64::MIN, f64::max) < -94.5);
    }
}

fn highest_altitude(point: &LatLon, heat_path: &str) -> f64 {
    let altitudes = heat_altitude(point.lat(), point.lon(), heat_path)
        .unwrap_or((Distance::with_meters(0.0), Distance::with_meters(0.0)));
    f64::max(
        altitudes.0.as_meters(),
        altitudes.1.as_meters(),
    )
}

pub fn height_tile_elevations(points: &[(u32, u32, LatLon)], heat_path: &str) -> Vec<f64> {
    points
        .par_iter()
        .map(|(_, _, point)| highest_altitude(point, heat_path))
        .collect()
}

pub fn lat_lon_path_10m(src: &LatLon, dst: &LatLon) -> Vec<LatLon> {
    let d = haversine_distance(src, dst);
    let extent_step = 1.0 / (d.as_meters() / 10.0);
    let mut extent = 0.0;
    let mut path = Vec::with_capacity((d.as_meters() / 10.0) as usize);
    while extent <= 1.0 {
        let step_point = haversine_intermediate(src, dst, extent);
        path.push(step_point);
        extent += extent_step;
    }
    path
}

pub fn lat_lon_path_1m(src: &LatLon, dst: &LatLon) -> Vec<LatLon> {
    let d = haversine_distance(src, dst);
    let extent_step = 1.0 / d.as_meters();
    let mut extent = 0.0;
    let mut path = Vec::with_capacity((d.as_meters() / 10.0) as usize);
    while extent <= 1.0 {
        let step_point = haversine_intermediate(src, dst, extent);
        path.push(step_point);
        extent += extent_step;
    }
    path
}

pub fn lat_lon_vec_to_heights(points: &[LatLon], heat_path: &str) -> Vec<f64> {
    points
        .par_iter()
        .map(|point| highest_altitude(point, heat_path))
        .collect()
}

/// Bare ground and canopy-top altitude at every point on a path, kept apart.
///
/// `lat_lon_vec_to_heights` collapses the two with `max`, which hands ITWOM a
/// tree crown as though it were a hill. Diffraction geometry wants bare earth;
/// vegetation is an attenuation, not a knife edge. Callers that model foliage
/// properly need both series, so this returns `(ground, canopy_top)` per point,
/// both as absolute altitudes in metres.
pub fn lat_lon_vec_to_ground_clutter(points: &[LatLon], heat_path: &str) -> Vec<(f64, f64)> {
    points
        .par_iter()
        .map(|point| {
            let a = heat_altitude(point.lat(), point.lon(), heat_path)
                .unwrap_or((Distance::with_meters(0.0), Distance::with_meters(0.0)));
            let ground = a.0.as_meters();
            let canopy = a.1.as_meters();
            (ground, f64::max(ground, canopy))
        })
        .collect()
}

pub fn has_line_of_sight(
    los_path: &[f64],
    start_elevation: Distance,
    end_elevation: Distance,
) -> bool {
    let start_height = los_path[0] + start_elevation.as_meters();
    let end_height = end_elevation.as_meters() as u16; // Not using terrain because of confusion with clutter on lidar
    let height_step = (end_height as f64 - start_height as f64) / los_path.len() as f64;
    let mut current_height = start_height as f64;
    let mut visible = true;
    for p in los_path.iter() {
        if current_height < *p {
            visible = false;
            break;
        }
        current_height += height_step;
    }
    visible
}
