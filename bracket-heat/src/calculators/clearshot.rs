//! LiDAR clear-shot screen for a remote survey.
//!
//! The ITWOM prediction runs on bare earth, so on its own it says "serve" to
//! nearly every address in range, including addresses where an install failed
//! for lack of line of sight. Predicted RSSI does not separate the two: failed
//! installs cluster in treed towns with many towers in range, where predicted
//! signal is strong.
//!
//! This screen asks the question an installer answers on the roof: is there a
//! clear shot over the cooked LiDAR surface (trees and buildings together) from
//! the AP to a plausible CPE mount? It grades confidence; it never removes an
//! AP from the list, because many working links only clear from a mount the
//! address point does not describe.
//!
//!   * Clear: 60% first-Fresnel clear from the address point itself, mount at
//!     roof/ground + 1.5 m, held to 3-6 m above ground.
//!   * CheckMount: direct ray clear from some point within 15 m, mount up to
//!     10 m above ground.
//!   * Obstructed: neither.   * Unverified: no cooked LiDAR at either end.
//!
//! The thresholds were tuned against one operator's install outcomes (successful
//! installs and technician-documented no-line-of-sight failures). Treat them as
//! starting points and as grades, not guarantees: re-check them against your own
//! outcomes, and remember LiDAR ages as trees grow and buildings go up.
use rf_signal_algorithms::{bheat::bheat_profile, LatLon};
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Clear,
    CheckMount,
    Unverified,
    Obstructed,
}

#[derive(Clone, Copy, Debug)]
pub struct Clearance {
    pub disposition: Disposition,
    /// 60%-Fresnel clearance at the CRM point (m); negative = obstructed.
    pub point_fresnel_m: Option<f64>,
    /// Best direct-ray clearance found within the mount search (m).
    pub best_ray_m: Option<f64>,
    /// Lowest mount, metres above ground AT THE CRM POINT, that clears 60% of
    /// the first Fresnel zone over the LiDAR surface. Never below the local
    /// roof + mast. None without cooked LiDAR.
    pub min_mount_agl_m: Option<f64>,
    /// Lowest mount (direct ray clear) at the best spot within the 15 m search,
    /// metres above that spot's ground.
    pub min_mount_nearby_agl_m: Option<f64>,
}

const MAST_M: f64 = 1.5;
const MIN_AGL_M: f64 = 3.0;
const POINT_MAX_AGL_M: f64 = 6.0;
const SEARCH_MAX_AGL_M: f64 = 10.0;
const SEARCH_RADIUS_M: f64 = 15.0;
const SEARCH_GRID_M: f64 = 5.0;
const NEAR_M: f64 = 300.0; // 1 m sampling this close to the CPE, 4 m beyond
const SKIP_TOWER_M: f64 = 20.0; // tower structure / own clutter cell
const SKIP_CPE_M: f64 = 2.0; // the mount's own roof cell
const EARTH_K_R_M: f64 = (4.0 / 3.0) * 6_371_000.0;

pub fn haversine_m(a: &LatLon, b: &LatLon) -> f64 {
    let (p1, p2) = (a.lat().to_radians(), b.lat().to_radians());
    let x = ((p2 - p1) / 2.0).sin().powi(2)
        + p1.cos() * p2.cos() * ((b.lon() - a.lon()).to_radians() / 2.0).sin().powi(2);
    2.0 * 6_371_000.0 * x.sqrt().asin()
}

/// (ray clearance, 60% Fresnel clearance) for one CPE position, or None when
/// either end lacks cooked LiDAR.
fn clearance_at(
    tower: &LatLon, tx_agl: f64, ghz: f64, cpe: &LatLon, max_agl: f64, heat_path: &str,
) -> Option<(f64, f64)> {
    let d = haversine_m(tower, cpe);
    if !(d > 0.0) || !d.is_finite() {
        return None;
    }
    let far_end = (d - NEAR_M).max(0.0);
    let mut s: Vec<f64> = Vec::new();
    let mut x = 0.0;
    while x < far_end { s.push(x); x += 4.0; }
    let mut x = far_end;
    while x < d { s.push(x); x += 1.0; }
    s.push(d);
    let pts: Vec<(f64, f64)> = s
        .iter()
        .map(|v| {
            let f = v / d;
            (tower.lat() + (cpe.lat() - tower.lat()) * f, tower.lon() + (cpe.lon() - tower.lon()) * f)
        })
        .collect();
    let prof = bheat_profile(&pts, heat_path);
    let (g0, _) = prof[0]?;
    let (gn, cn) = prof[prof.len() - 1]?;
    let tx = g0 + tx_agl;
    let rx = ((cn.max(gn)) + MAST_M).max(gn + MIN_AGL_M).min(gn + max_agl);
    let wavelength = 0.299_792_458 / ghz;
    let (mut ray_min, mut fres_min) = (f64::INFINITY, f64::INFINITY);
    for (sv, p) in s.iter().zip(prof.iter()) {
        if *sv <= SKIP_TOWER_M || *sv >= d - SKIP_CPE_M { continue; }
        let Some((g, c)) = p else { continue };
        let surf = c.max(*g);
        let bulge = sv * (d - sv) / (2.0 * EARTH_K_R_M);
        let gap = tx + (rx - tx) * sv / d - bulge - surf;
        let fres = (wavelength * sv * (d - sv) / d).sqrt();
        ray_min = ray_min.min(gap);
        fres_min = fres_min.min(gap - 0.6 * fres);
    }
    if ray_min.is_infinite() {
        return Some((99.0, 99.0)); // nothing between the ends to obstruct
    }
    Some((ray_min, fres_min))
}

/// Lowest mount height above ground at `cpe` that keeps `fresnel_k` of the
/// first Fresnel zone clear. Clearance is linear in receiver height, so this is
/// closed-form: each profile sample sets a minimum receiver altitude and the
/// worst sample governs.
fn required_agl(
    tower: &LatLon, tx_agl: f64, ghz: f64, cpe: &LatLon, fresnel_k: f64, heat_path: &str,
) -> Option<f64> {
    let d = haversine_m(tower, cpe);
    if !(d > 0.0) || !d.is_finite() { return None; }
    let far_end = (d - NEAR_M).max(0.0);
    let mut s: Vec<f64> = Vec::new();
    let mut x = 0.0;
    while x < far_end { s.push(x); x += 4.0; }
    let mut x = far_end;
    while x < d { s.push(x); x += 1.0; }
    s.push(d);
    let pts: Vec<(f64, f64)> = s.iter().map(|v| {
        let f = v / d;
        (tower.lat() + (cpe.lat() - tower.lat()) * f, tower.lon() + (cpe.lon() - tower.lon()) * f)
    }).collect();
    let prof = bheat_profile(&pts, heat_path);
    let (g0, _) = prof[0]?;
    let (gn, cn) = prof[prof.len() - 1]?;
    let tx = g0 + tx_agl;
    let wavelength = 0.299_792_458 / ghz;
    // Floor: above the local roof/canopy by the mast, and at least MIN_AGL_M.
    let mut rx_req = (cn.max(gn) + MAST_M).max(gn + MIN_AGL_M);
    for (sv, p) in s.iter().zip(prof.iter()) {
        if *sv <= SKIP_TOWER_M || *sv >= d - SKIP_CPE_M { continue; }
        let Some((g, c)) = p else { continue };
        let need = c.max(*g) + sv * (d - sv) / (2.0 * EARTH_K_R_M)
            + fresnel_k * (wavelength * sv * (d - sv) / d).sqrt();
        rx_req = rx_req.max(tx + (need - tx) * d / sv);
    }
    Some(rx_req - gn)
}

/// Required mount heights for one AP: at the CRM point (60% Fresnel) and at
/// the best spot within the search radius (direct ray).
pub fn required_mounts(tower: &LatLon, tx_agl: f64, ghz: f64, pos: &LatLon, heat_path: &str) -> (Option<f64>, Option<f64>) {
    let at_point = required_agl(tower, tx_agl, ghz, pos, 0.6, heat_path);
    let dlat = 1.0 / 111_320.0;
    let dlon = 1.0 / (111_320.0 * pos.lat().to_radians().cos());
    let n = (SEARCH_RADIUS_M / SEARCH_GRID_M) as i32;
    let mut best: Option<f64> = None;
    for i in -n..=n {
        for j in -n..=n {
            let (di, dj) = (i as f64 * SEARCH_GRID_M, j as f64 * SEARCH_GRID_M);
            if di * di + dj * dj > SEARCH_RADIUS_M * SEARCH_RADIUS_M { continue; }
            let cand = LatLon::new(pos.lat() + di * dlat, pos.lon() + dj * dlon);
            if let Some(h) = required_agl(tower, tx_agl, ghz, &cand, 0.0, heat_path) {
                best = Some(best.map_or(h, |b: f64| b.min(h)));
            }
        }
    }
    (at_point, best)
}

pub fn assess(tower: &LatLon, tx_agl: f64, ghz: f64, pos: &LatLon, heat_path: &str) -> Clearance {
    let point = clearance_at(tower, tx_agl, ghz, pos, POINT_MAX_AGL_M, heat_path);
    if let Some((_, f)) = point {
        if f >= 0.0 {
            let (m, nb) = required_mounts(tower, tx_agl, ghz, pos, heat_path);
            return Clearance { disposition: Disposition::Clear, point_fresnel_m: Some(f), best_ray_m: None,
                               min_mount_agl_m: m, min_mount_nearby_agl_m: nb };
        }
    }
    let dlat = 1.0 / 111_320.0;
    let dlon = 1.0 / (111_320.0 * pos.lat().to_radians().cos());
    let n = (SEARCH_RADIUS_M / SEARCH_GRID_M) as i32;
    let mut best: Option<f64> = None;
    for i in -n..=n {
        for j in -n..=n {
            let (di, dj) = (i as f64 * SEARCH_GRID_M, j as f64 * SEARCH_GRID_M);
            if di * di + dj * dj > SEARCH_RADIUS_M * SEARCH_RADIUS_M { continue; }
            let cand = LatLon::new(pos.lat() + di * dlat, pos.lon() + dj * dlon);
            if let Some((ray, _)) = clearance_at(tower, tx_agl, ghz, &cand, SEARCH_MAX_AGL_M, heat_path) {
                best = Some(best.map_or(ray, |b: f64| b.max(ray)));
                if ray >= 0.0 { break; } // one clear mount is enough
            }
        }
        if best.map_or(false, |b| b >= 0.0) { break; }
    }
    let disposition = match (point, best) {
        (None, _) | (_, None) => Disposition::Unverified,
        (_, Some(b)) if b >= 0.0 => Disposition::CheckMount,
        _ => Disposition::Obstructed,
    };
    let (m, nb) = required_mounts(tower, tx_agl, ghz, pos, heat_path);
    Clearance { disposition, point_fresnel_m: point.map(|p| p.1), best_ray_m: best,
                min_mount_agl_m: m, min_mount_nearby_agl_m: nb }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disposition_order_is_most_to_least_confident() {
        let mut v = vec![Disposition::Obstructed, Disposition::Clear, Disposition::Unverified, Disposition::CheckMount];
        v.sort_by_key(|d| rank(*d));
        assert_eq!(v, vec![Disposition::Clear, Disposition::CheckMount, Disposition::Unverified, Disposition::Obstructed]);
    }
    #[test]
    fn haversine_matches_known_distance() {
        let a = LatLon::new(39.0, -95.0);
        let b = LatLon::new(39.01, -95.0);
        assert!((haversine_m(&a, &b) - 1111.95).abs() < 0.5);
    }
}

/// Sort key: most to least confident.
pub fn rank(d: Disposition) -> u8 {
    match d {
        Disposition::Clear => 0,
        Disposition::CheckMount => 1,
        Disposition::Unverified => 2,
        Disposition::Obstructed => 3,
    }
}
