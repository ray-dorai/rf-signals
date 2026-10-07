use crate::WISP;
use rf_signal_algorithms::{
    bheat::heat_altitude, geometry::haversine_distance, itwom_point_to_point, lat_lon_path_1m,
    lat_lon_vec_to_heights, Distance, Frequency, LatLon, PTPClimate, PTPPath,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct ClickSite {
    pub base_height_m: f64,
    pub lidar_height_m: f64,
    pub towers: Vec<TowerEvaluation>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct TowerEvaluation {
    pub tower: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub rssi: f64,
    pub distance_km: f64,
    pub mode: String,
    /// LiDAR clear-shot grade: clear | check_mount | unverified | obstructed.
    #[serde(default)]
    pub disposition: String,
    #[serde(default)]
    pub point_fresnel_m: Option<f64>,
    /// Lowest clear-shot mount above ground at the address (60% Fresnel), m.
    #[serde(default)]
    pub min_mount_agl_m: Option<f64>,
    /// Lowest clear-shot mount at the best spot within 15 m (direct ray), m.
    #[serde(default)]
    pub min_mount_nearby_agl_m: Option<f64>,
    /// "itwom", "vband_fspl_gas", "vband_channel_unknown" or "free_space_estimate".
    #[serde(default)]
    pub rssi_basis: String,
    /// V-band: rain fade (dB) at the configured availability; rssi is clear-air.
    #[serde(default)]
    pub rain_fade_db: Option<f64>,
    #[serde(default)]
    pub rain_availability_pct: Option<f64>,
}

pub fn evaluate_tower_click(
    pos: &LatLon,
    frequency: Frequency,
    cpe_height: f64,
    heat_path: &str,
    link_budget: f64,
) -> ClickSite {
    let services = crate::calculators::services_in_range(pos);
    // cpe_height is the tallest mount to try, and it was previously accepted and
    // then ignored -- the sweep stopped at a hardcoded 3.6 m whatever was asked.
    let evaluation =
        crate::calculators::evaluate_wireless_services(pos, &services, heat_path, cpe_height);

    let mut evaluation = evaluation;
    evaluation.sort_by(|a, b| {
        crate::calculators::clearshot::rank(a.disposition)
            .cmp(&crate::calculators::clearshot::rank(b.disposition))
            .then(b.signal.total_cmp(&a.signal))
    });
    let towers = evaluation
        .iter()
        .map(|e| TowerEvaluation {
            tower: e.tower.clone(),
            name: format!("{}:{} @{}m", e.tower, e.name, e.cpe_height),
            lat: e.tower_pos.lat(),
            lon: e.tower_pos.lon(),
            rssi: e.signal,
            distance_km: e.range_km,
            mode: e.mode.clone(),
            disposition: serde_json::to_value(e.disposition)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default(),
            point_fresnel_m: e.point_fresnel_m,
            min_mount_agl_m: e.min_mount_agl_m,
            min_mount_nearby_agl_m: e.min_mount_nearby_agl_m,
            rssi_basis: e.rssi_basis.to_string(),
            rain_fade_db: e.rain_fade_db,
            rain_availability_pct: e.rain_fade_db.map(|_| crate::calculators::wave_availability_pct()),
        })
        .collect();

    let h = heat_altitude(pos.lat(), pos.lon(), heat_path)
        .unwrap_or((Distance::with_meters(0), Distance::with_meters(0)));
    ClickSite {
        base_height_m: h.0.as_meters(),
        lidar_height_m: h.1.as_meters(),
        towers,
    }
}
