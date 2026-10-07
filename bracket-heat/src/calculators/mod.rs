use crate::WISP;
mod survey;
pub mod clearshot;
pub use clearshot::Disposition;
pub use survey::survey_at_height;
pub use survey::SurveyResult;
use rf_signal_algorithms::{
    bheat::{bheat_altitude, heat_altitude}, geometry::haversine_distance,
    itwom_point_to_point, lat_lon_path_1m, lat_lon_vec_to_ground_clutter, Distance, Frequency,
    LatLon,
    PTPClimate, PTPPath,
};

/// Representative canopy height, in metres above local ground, around a receiver.
///
/// ITWOM applies its `saalos` clutter attenuation whenever the receive antenna
/// sits below `prop.cch`. Upstream presets that to a flat 22.5 m, so every
/// subscriber on a 4 m mount is treated as standing under closed canopy
/// whether the site is cottonwood bottomland or bare rangeland -- and the
/// attenuation saturates at a 22 dB cap, which is a large constant applied
/// almost fleet-wide.
///
/// Two details matter here:
///
/// * The `.bheat` clutter layer stores a per-cell MAXIMUM, so the cell holding
///   a house also holds the cottonwood standing next to it. Taking that maximum
///   as "the canopy the subscriber is under" is exactly what buries a mount that
///   in reality has a clear shot. We sample a neighbourhood and take the MEDIAN,
///   which asks what the signal actually traverses rather than what the single
///   tallest return in one cell was.
/// * We deliberately use `bheat_altitude`, not `heat_altitude`. The latter falls
///   back to SRTM and reports clutter equal to ground, which would read as
///   "no vegetation" everywhere outside the cooked LiDAR footprint. Returning
///   `None` there instead keeps the upstream preset, so an un-cooked area stays
///   as conservative as it is today rather than silently becoming optimistic.
pub fn local_canopy_m(lat: f64, lon: f64, heat_path: &str) -> Option<f64> {
    const SPAN_M: f64 = 40.0; // side of the sampled box
    const STEPS: i32 = 5; // 5x5 samples, ~10 m apart

    let dlat = SPAN_M / 111_320.0;
    let dlon = SPAN_M / (111_320.0 * lat.to_radians().cos().abs().max(1e-6));

    let mut seen: Vec<f64> = Vec::with_capacity((STEPS * STEPS) as usize);
    for i in 0..STEPS {
        for j in 0..STEPS {
            let fi = (i as f64 / (STEPS - 1) as f64) - 0.5;
            let fj = (j as f64 / (STEPS - 1) as f64) - 0.5;
            if let Some((ground, clutter)) =
                bheat_altitude(lat + fi * dlat, lon + fj * dlon, heat_path)
            {
                let above = clutter.as_meters() - ground.as_meters();
                if above.is_finite() && above >= 0.0 {
                    seen.push(above);
                }
            }
        }
    }

    if seen.is_empty() {
        return None; // no cooked LiDAR here: keep the upstream preset
    }
    seen.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(seen[seen.len() / 2])
}

/// Depth of vegetation, in metres, that the direct ray actually passes through.
///
/// `ground` and `canopy` are absolute altitudes sampled at 1 m along the path.
/// A sample counts only when there is real canopy there AND the straight
/// tower-to-CPE ray is below the canopy top at that point: foliage the beam
/// flies over costs nothing.
pub fn vegetation_depth_m(
    ground: &[f64],
    canopy: &[f64],
    tx_height_m: f64,
    rx_height_m: f64,
) -> f64 {
    let n = ground.len();
    if n < 2 || canopy.len() != n {
        return 0.0;
    }
    const MIN_CANOPY_M: f64 = 2.0; // below this it is bare ground noise, not foliage
    let tx = ground[0] + tx_height_m;
    let rx = ground[n - 1] + rx_height_m;
    let mut depth = 0.0;
    for i in 0..n {
        let above = canopy[i] - ground[i];
        if above < MIN_CANOPY_M {
            continue;
        }
        let ray = tx + (rx - tx) * (i as f64 / (n - 1) as f64);
        if ray < canopy[i] {
            // 1 m sampling, so each qualifying sample is one metre of foliage
            depth += 1.0;
        }
    }
    depth
}

/// Gain in dBi that this AP's antenna presents toward `pos`, or `None` when the
/// AP has no pattern and no azimuth -- in which case the caller keeps the old
/// behaviour of using `link_budget` as-is.
///
/// Without this, every sector radiates equally in all directions, so a
/// subscriber standing behind a 90-degree panel is offered the same signal as
/// one on boresight -- and the planner routinely proposes an AP the customer
/// sits behind.
fn pattern_gain_dbi(
    service: &WirelessService,
    pos: &LatLon,
    tower_ground_m: f64,
    rx_alt_m: f64,
) -> Option<f64> {
    let az0 = service.azimuth?;
    let key = service.pattern.as_ref()?;
    let reg = crate::PATTERNS.read();
    let pat = reg.get(key)?;

    // Azimuth: degrees off boresight, folded to 0..180.
    let bearing = bearing_deg(&service.pos, pos);
    let az_off = ((bearing - az0 + 540.0) % 360.0 - 180.0).abs();

    // Elevation: how far below the antenna the subscriber sits, as an angle,
    // then measured against the antenna's own downtilt. Both cuts are indexed
    // by angular distance off boresight, so only the magnitude matters.
    let d_m = haversine_distance(&service.pos, pos).as_meters().max(1.0);
    let tx_alt = tower_ground_m + service.tower_height_m;
    let depression = ((tx_alt - rx_alt_m) / d_m).atan().to_degrees();
    let tilt = service.downtilt.unwrap_or(0.0) + pat.tilt_deg;
    let el_off = (depression - tilt).abs();

    Some(pat.gain_toward(az_off, el_off))
}

/// Initial bearing from `a` to `b`, degrees clockwise from true north.
fn bearing_deg(a: &LatLon, b: &LatLon) -> f64 {
    let (p1, p2) = (a.lat().to_radians(), b.lat().to_radians());
    let dl = (b.lon() - a.lon()).to_radians();
    let y = dl.sin() * p2.cos();
    let x = p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos();
    (y.atan2(x).to_degrees() + 360.0) % 360.0
}

/// Whether the ITWOM terrain profile includes canopy tops as though they were
/// ground (`RF_PROFILE_CLUTTER=1`), or runs on bare earth (default).
///
/// Bare earth stops tree crowns acting as knife edges, which removes a large
/// over-prediction of loss on working links. But working links are all
/// successes: at addresses where an install failed for lack of line of sight, the
/// bare-earth model still says "serve". This switch exists so the two can be
/// compared on a population that contains both; obstruction is judged
/// separately in `clearshot`.
pub fn profile_uses_clutter() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("RF_PROFILE_CLUTTER")
            .map(|s| s.trim() == "1" || s.trim().eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

/// Foliage depth split into the stretch nearest the RECEIVER and everything
/// before it, in metres, using the same ray test as `vegetation_depth_m`.
///
/// Rural subscribers are typically farmsteads inside a planted shelterbelt with
/// open rangeland in between, so foliage is not distributed along the path --
/// it is bunched in the last hundred metres or so, and it is directional,
/// because what matters is whether the beam exits through the trees or through
/// a gap. A whole-path average dilutes that to nothing across several km.
pub fn vegetation_depth_split(
    ground: &[f64],
    canopy: &[f64],
    tx_height_m: f64,
    rx_height_m: f64,
    near_m: f64,
) -> (f64, f64) {
    let n = ground.len();
    if n < 2 || canopy.len() != n {
        return (0.0, 0.0);
    }
    const MIN_CANOPY_M: f64 = 2.0;
    let tx = ground[0] + tx_height_m;
    let rx = ground[n - 1] + rx_height_m;
    let boundary = (n as f64 - near_m).max(0.0) as usize; // 1 m sampling
    let (mut near, mut far) = (0.0, 0.0);
    for i in 0..n {
        if canopy[i] - ground[i] < MIN_CANOPY_M {
            continue;
        }
        let ray = tx + (rx - tx) * (i as f64 / (n - 1) as f64);
        if ray < canopy[i] {
            if i >= boundary {
                near += 1.0;
            } else {
                far += 1.0;
            }
        }
    }
    (near, far)
}

/// Vegetation attenuation, ITU-R P.833 style: a saturating exponential.
///
///     A = Am * (1 - exp(-d * gamma / Am))
///
/// The shape is the point. Loss grows quickly through the first few metres of
/// foliage and then saturates, because once the direct ray is extinguished the
/// signal arrives by diffraction and scatter around the vegetation rather than
/// through it. Treating the same foliage as opaque terrain instead produces
/// unbounded knife-edge loss: a few metres of trees can be charged tens of dB
/// where this model gives single digits.
///
/// Disabled by default -- see `veg_gamma_db_per_m`. The constants are NOT
/// authoritative ITU values; they are per-market tunables.
pub fn vegetation_loss_db(depth_m: f64, frequency: Frequency) -> f64 {
    if depth_m <= 0.0 {
        return 0.0;
    }
    let ghz = frequency.as_ghz();
    let gamma = veg_gamma_db_per_m() * ghz.powf(0.36); // specific attenuation, dB/m
    let a_max = veg_a_max_db() * ghz.powf(0.25); // saturation ceiling, dB
    if gamma <= 0.0 || a_max <= 0.0 {
        return 0.0;
    }
    a_max * (1.0 - (-depth_m * gamma / a_max).exp())
}

/// Tunables for `vegetation_loss_db`, read once from the environment:
///   RF_VEG_GAMMA_DB_PER_M   specific attenuation (default 0.0 -- OFF)
///   RF_VEG_A_MAX_DB         saturation ceiling  (default 15.0)
///
/// GAMMA DEFAULTS TO ZERO, which disables the term, and that is a measured
/// decision rather than a placeholder: fitted against measured links in a
/// sparsely treed, semi-arid market on a train/test split, the best gamma was
/// zero on both halves, and switching the term on made predictions worse.
///
/// Two reasons it earned nothing there, and they pull together:
///   * it double-counts. `cch` is already driven from the cooked LiDAR canopy,
///     so ITWOM charges receiver-side clutter through `saalos` before this term
///     is reached.
///   * sparse vegetation: most links crossed little or no foliage, and too few
///     crossed enough to identify the saturation ceiling, so A_MAX is
///     deliberately not fitted.
///
/// The machinery stays because it is correct and other markets are not that
/// terrain. Fit gamma against your own measured links before enabling it; do
/// not inherit this zero for a treed market.
pub fn veg_gamma_db_per_m() -> f64 {
    static V: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("RF_VEG_GAMMA_DB_PER_M")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or(0.0)
    })
}

/// Rain rate exceeded 0.01% of an average year, mm/h (`RF_RAIN_R001_MM_H`).
/// Take it from ITU-R P.837 for your area; it runs roughly 15-30 on the US high
/// plains and far higher in wet climates. The default is a mid high-plains value.
pub fn rain_r001_mm_h() -> f64 {
    static V: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("RF_RAIN_R001_MM_H").ok().and_then(|s| s.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0 && *v < 250.0).unwrap_or(24.0))
}

/// Availability the reported V-band rain fade is sized for, percent of the
/// year (`RF_WAVE_AVAILABILITY_PCT`, 99.0-99.999; default 99.9 = 8.8 h/yr).
pub fn wave_availability_pct() -> f64 {
    static V: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("RF_WAVE_AVAILABILITY_PCT").ok().and_then(|s| s.parse::<f64>().ok())
        .filter(|v| (99.0..=99.999).contains(v)).unwrap_or(99.9))
}

pub fn veg_a_max_db() -> f64 {
    static V: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("RF_VEG_A_MAX_DB")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(15.0)
    })
}

#[derive(Clone, Debug)]
pub struct WirelessService {
    tower: String,
    tower_pos: LatLon,
    name: String,
    pos: LatLon,
    height: Distance,
    frequency: Frequency,
    link_budget_db: f64,
    range: Distance,
    azimuth: Option<f64>,
    downtilt: Option<f64>,
    pattern: Option<String>,
    tower_height_m: f64,
}

#[derive(Clone, Debug)]
pub struct PossibleLink {
    pub tower: String,
    pub tower_pos: LatLon,
    pub name: String,
    pub cpe_height: f64,
    pub mode: String,
    pub signal: f64,
    pub range_km: f64,
    pub disposition: Disposition,
    /// 60%-Fresnel clearance at the CRM point, metres (negative = obstructed).
    pub point_fresnel_m: Option<f64>,
    pub min_mount_agl_m: Option<f64>,
    pub min_mount_nearby_agl_m: Option<f64>,
    /// "itwom", "vband_fspl_gas" (free space + P.676 gas on the AP's channel),
    /// "vband_channel_unknown" or "free_space_estimate".
    pub rssi_basis: &'static str,
    /// V-band only: rain fade (dB) exceeded for (100 - availability)% of the
    /// year. `signal` is clear-air; subtract this for the rain-faded level.
    pub rain_fade_db: Option<f64>,
}

/// Finds all APs within range of a given point
pub fn services_in_range(pos: &LatLon) -> Vec<WirelessService> {
    let mut result = Vec::new();

    let wisp_reader = WISP.read();
    wisp_reader.towers.iter().for_each(|t| {
        let range = haversine_distance(pos, &LatLon::new(t.lat, t.lon));

        t.access_points
            .iter()
            .filter(|ap| range.as_km() < ap.max_range_km)
            .for_each(|ap| {
                result.push(WirelessService {
                    tower: t.name.clone(),
                    tower_pos: LatLon::new(t.lat, t.lon),
                    name: ap.name.clone(),
                    pos: LatLon::new(t.lat, t.lon),
                    height: Distance::with_meters(ap.height_meters.unwrap_or(t.height_meters)),
                    frequency: Frequency::with_ghz(ap.frequency_ghz),
                    link_budget_db: ap.link_budget,
                    range: range.clone(),
                    azimuth: ap.azimuth,
                    downtilt: ap.downtilt,
                    pattern: ap.pattern.clone(),
                    tower_height_m: ap.height_meters.unwrap_or(t.height_meters),
                });
            });
    });

    result
}

/// Highest CPE mount the sweep will consider if the caller does not say.
/// Kept at the original hardcoded ceiling so behaviour is unchanged by default.
pub const DEFAULT_MAX_CPE_HEIGHT_M: f64 = 3.6;

pub fn evaluate_wireless_services(
    pos: &LatLon,
    services: &Vec<WirelessService>,
    heat_path: &str,
    max_cpe_height: f64,
) -> Vec<PossibleLink> {
    let mut result = Vec::new();
    services.iter().for_each(|svc| {
        evaluate_wireless_service(pos, svc, heat_path, &mut result, max_cpe_height);
    });
    result
}

fn evaluate_wireless_service(
    pos: &LatLon,
    service: &WirelessService,
    heat_path: &str,
    results: &mut Vec<PossibleLink>,
    max_cpe_height: f64,
) {
    // mmWave (outside ITWOM's 20 MHz-20 GHz) and sub-50 m paths: no terrain
    // model applies. Offer them on free-space loss plus the antenna pattern, and
    // let the LiDAR clear-shot grade carry the decision -- 60 GHz is line-of-
    // sight or nothing. Dropping them outright hid every Wave AP from the survey.
    if service.frequency.as_ghz() > 20.0 || service.range.as_meters() < 50.0 {
        let cpe_height = if max_cpe_height > 0.0 { max_cpe_height } else { DEFAULT_MAX_CPE_HEIGHT_M };
        let ground = heat_altitude(pos.lat(), pos.lon(), heat_path).map(|g| g.0.as_meters());
        let tower_ground = heat_altitude(service.pos.lat(), service.pos.lon(), heat_path).map(|g| g.0.as_meters());
        let (Some(ground), Some(tower_ground)) = (ground, tower_ground) else { return };
        let mut loss = rf_signal_algorithms::free_space_path_loss_db(service.frequency, Distance::with_meters(service.range.as_meters().max(1.0)));
        // V-band: add ITU-R P.676 gas loss for this AP's actual channel at the
        // path's elevation, and report the ITU-R P.530 rain fade at the target
        // availability. A nominal "60.0 GHz" is a placeholder, not a channel:
        // gas loss there could be 0.4 or 14 dB/km, so it stays a free-space
        // estimate and says so.
        let ghz = service.frequency.as_ghz();
        let d_km = service.range.as_km();
        let (mut basis, mut rain_fade_db) = ("free_space_estimate", None);
        if rf_signal_algorithms::vband_supported(ghz) {
            if (ghz - 60.0).abs() < 0.01 {
                basis = "vband_channel_unknown";
            } else {
                loss += rf_signal_algorithms::vband_gas_db_per_km(ghz, 0.5 * (ground + tower_ground)) * d_km;
                basis = "vband_fspl_gas";
            }
            rain_fade_db = Some(rf_signal_algorithms::vband_rain_fade_db(ghz, d_km, rain_r001_mm_h(), 100.0 - wave_availability_pct()));
        }
        let gain = pattern_gain_dbi(service, pos, tower_ground, ground + cpe_height).unwrap_or(0.0);
        let signal = service.link_budget_db + gain - loss;
        if !signal.is_finite() || signal < -80.0 { return; }
        let c = clearshot::assess(&service.pos, service.height.as_meters(), service.frequency.as_ghz(), pos, heat_path);
        let mode = match c.disposition {
            Disposition::Clear | Disposition::CheckMount => "L-o-S (LiDAR)",
            Disposition::Unverified => "Unverified (no LiDAR)",
            Disposition::Obstructed => "Obstructed (LiDAR)",
        };
        results.push(PossibleLink {
            tower: service.tower.clone(), tower_pos: service.tower_pos, name: service.name.clone(),
            cpe_height, mode: mode.into(), signal, range_km: service.range.as_km(),
            disposition: c.disposition, point_fresnel_m: c.point_fresnel_m,
            min_mount_agl_m: c.min_mount_agl_m, min_mount_nearby_agl_m: c.min_mount_nearby_agl_m, rssi_basis: basis,
            rain_fade_db,
        });
        return;
    } else {
        // ITM requires that the tower not include clutter
        let base_tower_height = heat_altitude(service.pos.lat(), service.pos.lon(), heat_path)
            .unwrap_or((Distance::with_meters(0.0), Distance::with_meters(0.0)))
            .0
            .as_meters();

        // Calculate the line between tower and SM
        let path = lat_lon_path_1m(&service.pos, pos);

        // Ground and canopy kept apart: diffraction geometry runs on bare earth,
        // vegetation is charged as attenuation afterwards. See
        // `vegetation_loss_db` for why feeding canopy in as terrain is wrong.
        let profile = lat_lon_vec_to_ground_clutter(&path, heat_path);
        let ground: Vec<f64> = profile.iter().map(|p| p.0).collect();
        let canopy: Vec<f64> = profile.iter().map(|p| p.1).collect();
        let mut los_path: Vec<f64> = if profile_uses_clutter() {
            canopy.clone()
        } else {
            ground.clone()
        };

        // Force the tower height in spot 0.
        los_path[0] = base_tower_height;

        let canopy_m = local_canopy_m(pos.lat(), pos.lon(), heat_path);

        // Sweep the CPE mount height until a link closes. The ceiling used to be
        // hardcoded at 3.6 m, which is below the treeline the LiDAR clutter layer
        // now models: a real roof or mast mount sits at 6-10 m and clears hedges,
        // fences and low canopy that stop a 3.6 m pole. A 3.6 m ceiling makes the
        // model report no service at addresses that are in fact served.
        for cpe_height in mount_heights(max_cpe_height) {
            let veg_m =
                vegetation_depth_m(&ground, &canopy, service.height.as_meters(), cpe_height);
            // Only conditions NTIA ITM v1.4 treats as errors disqualify. Its
            // warnings (Ns < 250 above ~1,760 m mean elevation; paths < 1 km)
            // are not errors: rejecting them removes every AP within 1 km and
            // every path over high-elevation terrain.
            let Some(lr) = itm_eval_full(cpe_height, &los_path, service, canopy_m) else { return };
            if !lr.is_valid() { continue; }
            let (raw_loss, mode) = (lr.dbloss, lr.mode);
            let mut loss = raw_loss + vegetation_loss_db(veg_m, service.frequency);
            // Off-boresight loss. `link_budget` excludes the AP antenna when a
            // pattern is attached, so the pattern supplies the gain -- including
            // its peak -- rather than adding to an already-included one.
            let rx_alt = ground[ground.len() - 1] + cpe_height;
            if let Some(g) = pattern_gain_dbi(service, pos, base_tower_height, rx_alt) {
                loss -= g;
            }
            let signal = service.link_budget_db - loss;
            let mut ok = true;
            if !signal.is_finite() || signal < -80.0 {
                ok = false;
            }
            // Reject 5.8 or higher with 2 obstacles
            if ok && service.frequency.as_ghz() > 5.0 && mode == "2_Hrzn_Diff" {
                ok = false;
            }
            if ok && service.frequency.as_ghz() > 9.0 && mode != "L-o-S" {
                ok = false;
            }
            if ok {
                let c = clearshot::assess(&service.pos, service.height.as_meters(), service.frequency.as_ghz(), pos, heat_path);
                results.push(PossibleLink {
                    tower: service.tower.clone(),
                    tower_pos: service.tower_pos,
                    name: service.name.clone(),
                    cpe_height,
                    mode: mode.clone(),
                    signal,
                    range_km: service.range.as_km(),
                    disposition: c.disposition,
                    point_fresnel_m: c.point_fresnel_m,
                    min_mount_agl_m: c.min_mount_agl_m,
                    min_mount_nearby_agl_m: c.min_mount_nearby_agl_m,
                    rssi_basis: "itwom",
                    rain_fade_db: None,
                });
                // Return the first qualifying, valid mount; failed LOS and
                // unchanged propagation modes must not terminate the search.
                break;
            }
        }
    }
}

/// As `itm_eval`, but keeps the parameter-check diagnostics.
pub(crate) fn itm_eval_full(
    cpe_height: f64,
    path_as_distances: &Vec<f64>,
    service: &WirelessService,
    canopy_m: Option<f64>,
) -> Option<rf_signal_algorithms::PTPResult> {
    // A profile ITM cannot take (non-finite samples, too short) skips this AP;
    // it must never panic, because a panic empties the whole survey response.
    let mut terrain_path = PTPPath::new(
        path_as_distances.clone(),
        service.height,
        Distance::with_meters(cpe_height),
        Distance::with_meters(1.0),
    )
    .ok()?;
    terrain_path.clutter_canopy_m = canopy_m;

    Some(itwom_point_to_point(
        &mut terrain_path,
        PTPClimate::default(),
        service.frequency,
        0.5,
        0.5,
        1,
    ))
}

fn mount_heights(requested: f64) -> Vec<f64> {
    let ceiling = if requested == 0.0 { DEFAULT_MAX_CPE_HEIGHT_M } else { requested };
    if !ceiling.is_finite() || ceiling < 0.5 || ceiling > 100.0 { return vec![]; }
    let mut heights = vec![];
    let mut h = 0.5;
    while h < ceiling { heights.push(h); h += 0.25; }
    heights.push(ceiling);
    heights
}

#[cfg(test)]
mod height_tests {
    use super::*;
    #[test]
    fn includes_exact_ceiling_without_invalid_low_mounts() {
        assert_eq!(mount_heights(1.1), vec![0.5, 0.75, 1.0, 1.1]);
        assert_eq!(mount_heights(0.5), vec![0.5]);
        assert_eq!(mount_heights(6.0).last(), Some(&6.0));
    }
    #[test]
    fn rejects_invalid_or_unbounded_mount_requests() {
        for h in [f64::NAN, f64::INFINITY, -1.0, 0.25, 101.0] {
            assert!(mount_heights(h).is_empty());
        }
    }
}
