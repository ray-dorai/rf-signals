mod units;
pub use units::{Distance, Frequency};
mod rfcalc;
pub use rfcalc::*;

//#[cfg(feature = "srtm")]
mod mapping;

//#[cfg(feature = "srtm")]
pub mod srtm {
    pub use crate::mapping::srtm::*;
}

pub mod bheat {
    pub use crate::mapping::bheat::*;
}

pub use mapping::terrain_config::{set_terrain_path, terrain_path};

//#[cfg(feature = "srtm")]
pub use mapping::latlon::LatLon;
pub use mapping::{
    has_line_of_sight, height_tile_elevations, lat_lon_path_10m, lat_lon_path_1m, lat_lon_tile,
    lat_lon_vec_to_ground_clutter, lat_lon_vec_to_heights,
};

// Re-export geo
//#[cfg(feature = "srtm")]
pub mod geo {
    pub use geo::*;
}

pub mod geometry;
