use crate::{Distance, LatLon};

pub fn haversine_distance(src: &LatLon, dst: &LatLon) -> Distance {
    use geo::algorithm::haversine_distance::HaversineDistance;
    Distance::with_meters(src.to_point().haversine_distance(&dst.to_point()))
}

pub fn haversine_intermediate(src: &LatLon, dst: &LatLon, extent: f64) -> LatLon {
    use geo::algorithm::haversine_intermediate::HaversineIntermediate;
    let start = src.to_radians_point();
    let end = dst.to_radians_point();
    let hi = start.haversine_intermediate(&end, extent);
    LatLon::from_radians_point(&hi)
}

/// Initial bearing from `src` to `dst`, degrees clockwise from true north.
/// Needed to work out where a point sits relative to a sector's boresight.
pub fn bearing_degrees(src: &LatLon, dst: &LatLon) -> f64 {
    let (lat1, lat2) = (src.lat().to_radians(), dst.lat().to_radians());
    let dlon = (dst.lon() - src.lon()).to_radians();
    let y = dlon.sin() * lat2.cos();
    let x = lat1.cos() * lat2.sin() - lat1.sin() * lat2.cos() * dlon.cos();
    let deg = y.atan2(x).to_degrees();
    (deg + 360.0) % 360.0
}

/// Smallest signed difference between two bearings, in [-180, 180).
/// Exactly opposite bearings come back as -180.
pub fn bearing_delta(from_deg: f64, to_deg: f64) -> f64 {
    ((to_deg - from_deg) % 360.0 + 540.0) % 360.0 - 180.0
}

#[cfg(test)]
mod bearing_tests {
    use super::*;

    #[test]
    fn due_north_and_east() {
        let o = LatLon::new(39.0, -95.0);
        assert!((bearing_degrees(&o, &LatLon::new(40.0, -95.0)) - 0.0).abs() < 0.01);
        let e = bearing_degrees(&o, &LatLon::new(39.0, -94.0));
        assert!((e - 90.0).abs() < 0.5, "east bearing was {}", e);
    }

    #[test]
    fn deltas_wrap() {
        assert!((bearing_delta(350.0, 10.0) - 20.0).abs() < 1e-9);
        assert!((bearing_delta(10.0, 350.0) + 20.0).abs() < 1e-9);
        assert!((bearing_delta(0.0, 180.0).abs() - 180.0).abs() < 1e-9);
    }
}
