//! Fixed-height AP-specific evidence. Never represents a package approval.
use super::*;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct SurveyResult {
    pub ap_id: String,
    pub ap_name: String,
    pub tower: String,
    pub model: String,
    pub frequency_ghz: f64,
    pub channel_width_mhz: Option<f64>,
    pub ap_height_m: f64,
    pub ap_height_source: String,
    pub cpe_height_m: f64,
    pub range_km: f64,
    pub range_limit_km: f64,
    pub budget_source: String,
    pub status: String,
    pub reasons: Vec<String>,
    pub predicted_downlink_dbm: Option<f64>,
    /// Model output regardless of warning code. Diagnostic only: never a
    /// qualifying prediction, never a calibration target on its own.
    pub unqualified_downlink_dbm: Option<f64>,
    /// First obstruction distance from the CPE along the path (m), when the
    /// surface blocks the direct ray. Near-CPE obstructions usually mean the
    /// CRM coordinate is not the antenna position.
    pub nearest_obstruction_from_cpe_m: Option<f64>,
    pub model_error_code: Option<i32>,
    /// Which ITWOM parameter-range checks fired, e.g.
    /// "surface_refractivity_outside_250_400N=243.1 (code 4)".
    pub model_warnings: Vec<String>,
    /// Surface refractivity after elevation scaling (N-units) and mean profile
    /// elevation used for it (m).
    pub model_surface_refractivity_n: Option<f64>,
    pub model_mean_elevation_m: Option<f64>,
    pub propagation_mode: Option<String>,
    pub lidar_profile_complete: bool,
    pub minimum_ray_clearance_m: Option<f64>,
    pub minimum_60pct_fresnel_clearance_m: Option<f64>,
    pub auto_approvable: bool,
}

/// Geometry screen only. Surface combines buildings and vegetation; negative
/// clearance does not by itself reject lower-frequency NLOS service.
fn clearance(ground: &[f64], surface: &[f64], tx_h: f64, rx_h: f64,
             distance_m: f64, ghz: f64) -> Option<(f64, f64)> {
    if ground.len() < 3 || ground.len() != surface.len() || distance_m <= 0.0 || ghz <= 0.0 {
        return None;
    }
    let tx = ground[0] + tx_h;
    let rx = ground[ground.len()-1] + rx_h;
    let mut ray_min = f64::INFINITY;
    let mut fresnel_min = f64::INFINITY;
    for (i, top) in surface.iter().enumerate().take(surface.len()-1).skip(1) {
        let f = i as f64 / (surface.len()-1) as f64;
        let d1 = f * distance_m;
        let d2 = distance_m - d1;
        let earth_bulge = d1 * d2 / (2.0 * (4.0/3.0) * 6_371_000.0);
        let gap = tx + (rx-tx)*f - top - earth_bulge;
        let fresnel = (0.299792458 / ghz * d1*d2/distance_m).sqrt();
        ray_min = ray_min.min(gap);
        fresnel_min = fresnel_min.min(gap - 0.6*fresnel);
    }
    if ray_min.is_finite() && fresnel_min.is_finite() { Some((ray_min, fresnel_min)) } else { None }
}

/// Distance from the receiver to the closest profile point whose surface
/// reaches the direct ray (4/3-earth). `None` when the ray is clear.
fn nearest_obstruction_from_rx(ground: &[f64], surface: &[f64], tx_h: f64, rx_h: f64, distance_m: f64) -> Option<f64> {
    let n = surface.len();
    if n < 3 || ground.len() != n || distance_m <= 0.0 { return None; }
    let tx = ground[0] + tx_h;
    let rx = ground[n-1] + rx_h;
    (1..n-1).rev().find_map(|i| {
        let f = i as f64 / (n-1) as f64;
        let d1 = f * distance_m;
        let bulge = d1 * (distance_m - d1) / (2.0 * (4.0/3.0) * 6_371_000.0);
        (tx + (rx-tx)*f - surface[i] - bulge < 0.0).then(|| distance_m - d1)
    })
}

fn signal_status(error: i32, signal: f64, complete: bool, cutoff: f64) -> &'static str {
    if error != 0 || !signal.is_finite() || !complete { "needs_review" }
    else if signal < cutoff { "below_rf_cutoff" }
    else { "rf_candidate_unvalidated" }
}

pub fn survey_at_height(pos: &LatLon, height: f64, cutoff: f64, ap_id: Option<&str>, heat_path: &str) -> Vec<SurveyResult> {
    // Release the registry lock before terrain work.
    let towers = WISP.read().towers.clone();
    let mut results = vec![];
    for tower in towers {
        for ap in &tower.access_points {
            let id = ap.equipment_id.clone().unwrap_or_else(|| format!("{}::{}", tower.name, ap.name));
            if ap_id.map(|wanted| wanted != id).unwrap_or(false) { continue; }
            let tx_pos = LatLon::new(tower.lat, tower.lon);
            let range = haversine_distance(pos, &tx_pos);
            let tx_height = ap.height_meters.unwrap_or(tower.height_meters);
            let mut result = SurveyResult {
                ap_id: id, ap_name: ap.name.clone(), tower: tower.name.clone(), model: ap.model.clone(),
                frequency_ghz: ap.frequency_ghz, channel_width_mhz: ap.channel_width_mhz,
                ap_height_m: tx_height,
                ap_height_source: if ap.height_meters.is_some() { "ap_inventory_unverified" } else { "tower_inventory_fallback" }.into(),
                cpe_height_m: height, range_km: range.as_km(), range_limit_km: ap.max_range_km,
                budget_source: ap.budget_source.clone(), status: "needs_review".into(),
                reasons: vec!["Mount coordinates/heights and requested package performance are not field-verified".into()],
                predicted_downlink_dbm: None, unqualified_downlink_dbm: None,
                nearest_obstruction_from_cpe_m: None, model_error_code: None, model_warnings: vec![],
                model_surface_refractivity_n: None, model_mean_elevation_m: None, propagation_mode: None,
                lidar_profile_complete: false, minimum_ray_clearance_m: None,
                minimum_60pct_fresnel_clearance_m: None, auto_approvable: false,
            };
            if range.as_km() >= ap.max_range_km {
                result.status = "outside_planning_range".into();
                result.reasons.push("Outside configured empirical range; not proof of physical impossibility".into());
                results.push(result); continue;
            }
            if range.as_meters() < 50.0 {
                result.reasons.push("Short-path obstruction and near-field behavior require review".into());
                results.push(result); continue;
            }
            let path = lat_lon_path_1m(&tx_pos, pos);
            let lidar: Vec<_> = path.iter().map(|p| bheat_altitude(p.lat(), p.lon(), heat_path)).collect();
            result.lidar_profile_complete = lidar.iter().all(Option::is_some);
            if !result.lidar_profile_complete {
                result.reasons.push("Missing cooked LiDAR on path; terrain-only fallback cannot verify obstacles".into());
            }
            let profile = lat_lon_vec_to_ground_clutter(&path, heat_path);
            let ground: Vec<f64> = profile.iter().map(|p| p.0).collect();
            let surface: Vec<f64> = profile.iter().map(|p| p.0.max(p.1)).collect();
            if ground.len() < 3 || ground.iter().chain(surface.iter()).any(|v| !v.is_finite()) {
                result.reasons.push("Missing or invalid terrain profile".into());
                results.push(result); continue;
            }
            result.nearest_obstruction_from_cpe_m = nearest_obstruction_from_rx(&ground, &surface, tx_height, height, range.as_meters());
            if let Some((ray, fresnel)) = clearance(&ground, &surface, tx_height, height, range.as_meters(), ap.frequency_ghz) {
                result.minimum_ray_clearance_m = Some(ray);
                result.minimum_60pct_fresnel_clearance_m = Some(fresnel);
            }
            if ap.frequency_ghz > 20.0 {
                result.reasons.push("mmWave geometry screen only: validated gas/rain/equipment model not yet implemented; ITWOM not used".into());
                results.push(result); continue;
            }
            let service = WirelessService {
                tower: tower.name.clone(), tower_pos: tx_pos, name: ap.name.clone(),
                pos: tx_pos, height: Distance::with_meters(tx_height),
                frequency: Frequency::with_ghz(ap.frequency_ghz), link_budget_db: ap.link_budget,
                range, azimuth: ap.azimuth, downtilt: ap.downtilt, pattern: ap.pattern.clone(), tower_height_m: tx_height,
            };
            let input = if profile_uses_clutter() { &surface } else { &ground };
            let Some(lr) = itm_eval_full(height, input, &service, local_canopy_m(pos.lat(), pos.lon(), heat_path)) else {
                result.reasons.push("Terrain profile rejected by the propagation model".into());
                results.push(result); continue;
            };
            let valid = lr.is_valid();
            let (loss, mode, error) = (lr.dbloss.clone(), lr.mode.clone(), lr.error_num);
            result.model_error_code = Some(error);
            result.model_warnings = lr.warnings.iter()
                .map(|w| format!("{}={:.6} (code {}{})", w.check, w.value, w.code, if w.blocking { ", blocking" } else { "" })).collect();
            result.model_surface_refractivity_n = Some(lr.derived.ens);
            result.model_mean_elevation_m = Some(lr.derived.zsys_m);
            // Staging diagnostic: dump the exact profile for reference-model comparison.
            if let Ok(dir) = std::env::var("BRACKET_HEAT_SURVEY_PROFILE_DUMP") {
                let dump = serde_json::json!({
                    "ap_id": result.ap_id, "lat": pos.lat(), "lon": pos.lon(),
                    "elevations_m": input, "step_m": 1.0, "tx_height_m": tx_height, "rx_height_m": height,
                    "frequency_mhz": service.frequency.as_mhz(),
                    "canopy_m": local_canopy_m(pos.lat(), pos.lon(), heat_path),
                    "uses_clutter": profile_uses_clutter(),
                    "itwom_loss_db": loss, "itwom_mode": mode, "itwom_error": error,
                    "itwom_dist_m": lr.derived.dist_m, "surface_m": surface, "ground_m": ground, "itwom_ens": lr.derived.ens,
                });
                let name = format!("{}/{}_{:.6}_{:.6}.json", dir, result.ap_id, pos.lat(), pos.lon());
                let _ = std::fs::write(name, dump.to_string());
            }
            result.propagation_mode = Some(mode);
            let gain = pattern_gain_dbi(&service, pos, ground[0], ground[ground.len()-1] + height);
            let veg = vegetation_depth_m(&ground, &surface, tx_height, height);
            let signal = ap.link_budget + gain.unwrap_or(0.0) - loss - vegetation_loss_db(veg, service.frequency);
            // Invalid model results are evidence, not calibration targets.
            if signal.is_finite() { result.unqualified_downlink_dbm = Some(signal); }
            // Only NTIA-class errors invalidate the prediction; warnings are listed.
            if valid && signal.is_finite() { result.predicted_downlink_dbm = Some(signal); }
            result.status = signal_status(if valid { 0 } else { error.max(1) }, signal, result.lidar_profile_complete, cutoff).into();
            if !valid { result.reasons.push(format!("ITWOM blocking error (code {})", error)); }
            else if error != 0 { result.reasons.push(format!("ITWOM non-blocking warning (legacy code {}): {}", error, result.model_warnings.join("; "))); }
            if gain.is_none() {
                result.status = "needs_review".into();
                result.reasons.push("Directional antenna pattern or azimuth unavailable".into());
            }
            if ap.budget_source != "snmp_tx_plus_family_cpe_gain" {
                result.status = "needs_review".into();
                result.reasons.push("Transmit budget is provisional rather than SNMP-derived".into());
            }
            result.reasons.push("CPE gain is a family assumption; uplink, interference, capacity, and tier performance are not qualified".into());
            results.push(result);
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn flat_profile_clearance_and_obstruction() {
        let ground = vec![0.0; 101];
        let (ray, fresnel) = clearance(&ground, &ground, 10.0, 10.0, 100.0, 60.0).unwrap();
        assert!(ray > 9.9 && fresnel > 9.0 && fresnel < ray);
        let mut surface = ground.clone(); surface[50] = 20.0;
        assert!(clearance(&ground, &surface, 10.0, 10.0, 100.0, 60.0).unwrap().0 < 0.0);
    }
    #[test]
    fn nearest_obstruction_is_measured_from_receiver() {
        let ground = vec![0.0; 101];
        assert_eq!(nearest_obstruction_from_rx(&ground, &ground, 10.0, 6.0, 100.0), None);
        let mut surface = ground.clone(); surface[20] = 30.0; surface[95] = 12.0;
        let d = nearest_obstruction_from_rx(&ground, &surface, 10.0, 6.0, 100.0).unwrap();
        assert!((d - 5.0).abs() < 1e-9);
    }
    #[test]
    fn invalid_or_missing_data_never_qualifies() {
        assert_eq!(signal_status(4, -40.0, true, -78.0), "needs_review");
        assert_eq!(signal_status(0, -40.0, false, -78.0), "needs_review");
        assert_eq!(signal_status(0, f64::NAN, true, -78.0), "needs_review");
        assert_eq!(signal_status(0, -79.0, true, -78.0), "below_rf_cutoff");
        assert_eq!(signal_status(0, -78.0, true, -78.0), "rf_candidate_unvalidated");
    }
}
