use rf_signal_algorithms::srtm::get_altitude;
use rf_signal_algorithms::*;
use std::fs::File;
use std::io::prelude::*;
use std::path::Path;

const ROW_SIZE: usize = 768;
const COL_SIZE: usize = 768;
const NUM_CELLS: usize = COL_SIZE * ROW_SIZE;
const TOTAL_ENTRIES: usize = NUM_CELLS * 2;
pub const TILE_BYTES: usize = TOTAL_ENTRIES * 2;

pub struct MapTile {
    filename: String,
    heights: Vec<u16>,
    dirty: bool,
}

impl MapTile {
    /// Load an existing tile, or seed a new one from the SRTM baseline.
    ///
    /// Seeding matters: a .bheat tile that exists but is empty reads as sea
    /// level. Every new tile starts as the best terrain we have, and LiDAR then
    /// raises it where the point cloud says so.
    pub fn get_tile(lat: f64, lon: f64, bheat_path: &str, terrain_path: &str) -> Self {
        let filename = MapTile::get_tile_name(lat, lon, bheat_path);
        if Path::new(&filename).exists() {
            if let Ok(mut file) = File::open(&filename) {
                let mut raw = Vec::with_capacity(TILE_BYTES);
                if file.read_to_end(&mut raw).is_ok() && raw.len() >= TILE_BYTES {
                    let heights = bytemuck::cast_slice::<u8, u16>(&raw[..TILE_BYTES]).to_vec();
                    return MapTile { filename, heights, dirty: false };
                }
                // Short or unreadable: a previous run was interrupted. Rebuild it
                // rather than merging LiDAR into a truncated tile.
                eprintln!("  rebuilding short tile {}", filename);
            }
        }

        let mut tile = MapTile {
            filename,
            heights: vec![0; TOTAL_ENTRIES],
            dirty: true,
        };
        for idx in 0..NUM_CELLS {
            let (plat, plon) = tile.coords(idx, lat, lon);
            let h = get_altitude(&LatLon::new(plat, plon), terrain_path)
                .unwrap_or(Distance::with_meters(0.0))
                .as_meters();
            tile.heights[idx * 2] = (h.max(0.0) * 10.0) as u16;
        }
        tile
    }

    pub fn index(&self, lat: f64, lon: f64) -> usize {
        let lat_abs = lat.abs();
        let lon_abs = lon.abs();
        let sub_lat = (lat_abs.fract() * 100.0).floor();
        let sub_lon = (lon_abs.fract() * 100.0).floor();

        let base_lat = lat_abs.floor() + (sub_lat / 100.0);
        let row_index = (((lat_abs - base_lat) * 100.0) * ROW_SIZE as f64) as usize;
        let base_lon = lon_abs.floor() + (sub_lon / 100.0);
        let col_index = (((lon_abs - base_lon) * 100.0) * COL_SIZE as f64) as usize;

        // Clamp: a point exactly on the tile's north or east edge rounds to 768
        // and used to write into the next row.
        let row_index = row_index.min(ROW_SIZE - 1);
        let col_index = col_index.min(COL_SIZE - 1);
        (row_index * COL_SIZE) + col_index
    }

    pub fn store_ground(&mut self, index: usize, altitude: u16) {
        if self.heights[index * 2] < altitude {
            self.heights[index * 2] = altitude;
            self.dirty = true;
        }
    }

    /// Metres above ground beyond which a non-ground return is not clutter.
    ///
    /// Classification[1:6] on the way in removes the LAS noise classes, but NOT
    /// cloud and fog, which come back as class 1 (unclassified) and are
    /// spatially coherent -- a cloud is a real object. Observed as a 403-cell
    /// patch 324-384 m above level 2137 m terrain at N041t79_W106t74.
    ///
    /// Because clutter is stored as a per-cell MAXIMUM, one such patch is a
    /// permanent phantom obstruction that kills every path crossing it and
    /// looks like a legitimate RF result. Sampled over 126 million cooked
    /// cells, real clutter reaches 42 m at p99.999, so this threshold discards
    /// atmospheric artefacts without touching anything structural.
    const MAX_CLUTTER_DM: u16 = 1200; // 120 m in decimetres

    pub fn store_clutter(&mut self, index: usize, altitude: u16) {
        let ground = self.heights[index * 2];
        if altitude > ground.saturating_add(Self::MAX_CLUTTER_DM) {
            return;
        }
        if self.heights[(index * 2) + 1] < altitude {
            self.heights[(index * 2) + 1] = altitude;
            self.dirty = true;
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Write atomically. A reader that mmaps a half-written tile gets garbage
    /// elevations with no error, so the file is never modified in place.
    pub fn save(&self) -> std::io::Result<()> {
        let tmp = format!("{}.part", self.filename);
        {
            let mut file = File::create(&tmp)?;
            file.write_all(bytemuck::cast_slice(&self.heights))?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, &self.filename)
    }

    pub fn get_tile_name(lat: f64, lon: f64, bheat_path: &str) -> String {
        let lat_c = if lat < 0.0 { 'S' } else { 'N' };
        let lon_c = if lon < 0.0 { 'W' } else { 'E' };
        let lat_abs = lat.abs();
        let lon_abs = lon.abs();
        let sub_lat = (lat_abs.fract() * 100.0).floor();
        let sub_lon = (lon_abs.fract() * 100.0).floor();

        // Must match bheat::tile_reader::get_tile_name exactly, including the
        // "{:03 }" with its embedded space -- it is legal and emits no space,
        // but the two sides have to agree character for character.
        format!(
            "{}/{}{:03}t{:02}_{}{:03 }t{:02}.bheat",
            bheat_path.trim_end_matches('/'),
            lat_c,
            lat_abs.floor() as i32,
            sub_lat,
            lon_c,
            lon_abs.floor() as i32,
            sub_lon,
        )
    }

    pub fn coords(&self, index: usize, lat: f64, lon: f64) -> (f64, f64) {
        let col = index % COL_SIZE;
        let row = index / COL_SIZE;
        let lat_abs = lat.abs();
        let lon_abs = lon.abs();
        let sub_lat = (lat_abs.fract() * 100.0).floor();
        let sub_lon = (lon_abs.fract() * 100.0).floor();

        let step = (1.0 / COL_SIZE as f64) / 100.0;
        let lat_pos = lat_abs.floor() + (sub_lat / 100.0) + (step * row as f64);
        let lon_pos = lon_abs.floor() + (sub_lon / 100.0) + (step * col as f64);

        (
            if lat < 0.0 { -lat_pos } else { lat_pos },
            if lon < 0.0 { -lon_pos } else { lon_pos },
        )
    }
}
