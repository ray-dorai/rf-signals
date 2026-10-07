//! Process-wide terrain configuration.
//!
//! `bheat` tiles are an *enhancement* layer: they carry LiDAR-derived ground and
//! clutter at ~1.5 m resolution, but they only exist where LiDAR has been cooked.
//! The SRTM (.hgt) pyramid is the *baseline* layer and covers the whole service
//! area. Registering the terrain path here lets `heat_altitude` fall back to SRTM
//! when a bheat tile is missing, instead of reporting sea level.
use lazy_static::*;
use parking_lot::RwLock;

lazy_static! {
    static ref TERRAIN_PATH: RwLock<Option<String>> = RwLock::new(None);
}

/// Register the base directory holding the `1/`, `3/` and `third/` .hgt trees.
/// Call once at process start. Passing an empty string disables the fallback.
pub fn set_terrain_path(path: &str) {
    let mut w = TERRAIN_PATH.write();
    *w = if path.is_empty() {
        None
    } else {
        Some(path.trim_end_matches('/').to_string())
    };
}

/// The registered terrain path, if any.
pub fn terrain_path() -> Option<String> {
    TERRAIN_PATH.read().clone()
}
